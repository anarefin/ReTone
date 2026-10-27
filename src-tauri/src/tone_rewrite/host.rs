//! The picker window as the controller drives it: creation on the main
//! thread, page-load tracking, placement in global points, show with or
//! without focus, events to its renderer.

use super::controller::PickerHost;
use crate::windows;
use retone_core::payload::{Display, PickerPayload, Point, Rect};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewWindow};
use tokio::sync::{oneshot, watch};

/// A press waits this long at most for a fresh picker's page to load before
/// pushing its payload anyway (the renderer also pulls it once mounted).
const LOAD_WAIT: Duration = Duration::from_secs(10);

/// When the focus diagnostics look again after showing the picker.
#[cfg(target_os = "macos")]
const FOCUS_CHECK_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone)]
struct PickerHandle {
    generation: u64,
    label: String,
    loaded: watch::Receiver<bool>,
}

pub struct TauriHost {
    app: AppHandle,
    current: Mutex<Option<PickerHandle>>,
    next_generation: AtomicU64,
    /// One creation at a time (a prewarm and a press can race).
    creating: tokio::sync::Mutex<()>,
}

impl TauriHost {
    pub fn new(app: AppHandle) -> Self {
        TauriHost {
            app,
            current: Mutex::new(None),
            next_generation: AtomicU64::new(0),
            creating: tokio::sync::Mutex::new(()),
        }
    }

    fn handle(&self) -> Option<PickerHandle> {
        self.current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn window(&self) -> Option<WebviewWindow> {
        let handle = self.handle()?;
        self.app.get_webview_window(&handle.label)
    }

    /// Whether `label` is the live picker (commands from any other window,
    /// or from a picker being torn down, are refused).
    pub fn is_current(&self, label: &str) -> bool {
        self.handle().is_some_and(|handle| handle.label == label)
    }

    /// The window with `label` was destroyed. Returns whether it was the live
    /// picker.
    pub fn forget(&self, label: &str) -> bool {
        let mut current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if current.as_ref().is_some_and(|handle| handle.label == label) {
            *current = None;
            return true;
        }
        false
    }

    fn displays(&self) -> Vec<Display> {
        let monitors = match self.app.available_monitors() {
            Ok(monitors) => monitors,
            Err(error) => {
                tracing::warn!(target: "tone-rewrite", %error, "Could not list displays");
                return Vec::new();
            }
        };
        monitors
            .iter()
            .map(|monitor| {
                // Physical values are points × that display's own scale.
                let scale = monitor.scale_factor().max(1.0);
                let position = monitor.position();
                let size = monitor.size();
                let area = monitor.work_area();
                Display {
                    bounds: Rect {
                        x: f64::from(position.x) / scale,
                        y: f64::from(position.y) / scale,
                        width: f64::from(size.width) / scale,
                        height: f64::from(size.height) / scale,
                    },
                    work_area: Rect {
                        x: f64::from(area.position.x) / scale,
                        y: f64::from(area.position.y) / scale,
                        width: f64::from(area.size.width) / scale,
                        height: f64::from(area.size.height) / scale,
                    },
                }
            })
            .collect()
    }
}

impl PickerHost for TauriHost {
    async fn create(&self) -> u64 {
        let _creating = self.creating.lock().await;
        if let Some(handle) = self.handle() {
            return handle.generation;
        }
        let generation = self.next_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let label = format!("{}-{generation}", windows::PICKER_LABEL);
        let (loaded_tx, loaded_rx) = watch::channel(false);
        let (done_tx, done_rx) = oneshot::channel();
        let app = self.app.clone();
        let window_label = label.clone();
        let started = std::time::Instant::now();
        let work = move || {
            let result = windows::create_picker_window(&app, &window_label, move || {
                let _ = loaded_tx.send(true);
            });
            let _ = done_tx.send(result.map(|_| ()).map_err(|e| e.to_string()));
        };
        // The NSWindow is configured on creation, which AppKit wants on the
        // main thread.
        if let Err(error) = self.app.run_on_main_thread(work) {
            tracing::warn!(target: "tone-rewrite", %error, "Could not schedule the picker window");
            return 0;
        }
        match done_rx.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(target: "tone-rewrite", %error, "Tone rewrite picker failed to load");
                return 0;
            }
            Err(_) => return 0,
        }
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = Some(PickerHandle {
            generation,
            label: label.clone(),
            loaded: loaded_rx.clone(),
        });
        tracing::debug!(target: "tone-rewrite", label, "Tone rewrite picker created");

        // Log the load time once the page reports in.
        let mut loaded = loaded_rx;
        tauri::async_runtime::spawn(async move {
            if loaded.wait_for(|done| *done).await.is_ok() {
                tracing::debug!(
                    target: "tone-rewrite",
                    load_ms = started.elapsed().as_millis() as u64,
                    "Tone rewrite picker loaded"
                );
            }
        });
        generation
    }

    fn generation(&self) -> u64 {
        self.handle().map_or(0, |handle| handle.generation)
    }

    async fn wait_loaded(&self, generation: u64) {
        let Some(mut loaded) = self
            .handle()
            .filter(|handle| handle.generation == generation)
            .map(|handle| handle.loaded)
        else {
            return;
        };
        match tokio::time::timeout(LOAD_WAIT, loaded.wait_for(|done| *done)).await {
            Ok(_) => {}
            Err(_) => {
                tracing::warn!(target: "tone-rewrite", "Tone rewrite picker page did not finish loading")
            }
        }
    }

    fn cursor_and_displays(&self) -> Option<(Point, Vec<Display>)> {
        let (x, y) = cursor()?;
        Some((Point { x, y }, self.displays()))
    }

    fn set_bounds(&self, bounds: Rect) {
        let Some(window) = self.window() else {
            return;
        };
        let _ = window.set_size(LogicalSize::new(bounds.width, bounds.height));
        let _ = window.set_position(LogicalPosition::new(bounds.x, bounds.y));
    }

    fn show_and_focus(&self) {
        let Some(window) = self.window() else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            // Queued behind the size and position messages, on the main thread.
            let target = window.clone();
            let scheduled = window.run_on_main_thread(move || {
                let Ok(ns_window) = target.ns_window() else {
                    let _ = target.show();
                    let _ = target.set_focus();
                    return;
                };
                // SAFETY: Tauri's live NSWindow, on the main thread.
                let before = unsafe { retone_macos::panel::focus_state(ns_window) };
                if before.is_some_and(|state| state.panel) {
                    // Key without activating ReTone: the target app stays
                    // active, and macOS cannot refuse it.
                    unsafe { retone_macos::panel::make_key_and_order_front(ns_window) };
                } else {
                    // Not a panel: the keyboard needs ReTone active, which
                    // macOS 14+ may decline.
                    let _ = target.show();
                    let _ = target.set_focus();
                }
                log_focus(&target, "shown");
            });
            if scheduled.is_err() {
                let _ = window.show();
                let _ = window.set_focus();
            }
            // Re-check once AppKit has settled: a picker that is on screen
            // but not key ignores the keyboard (the Phase 5 flake).
            let check = window.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(FOCUS_CHECK_DELAY).await;
                let target = check.clone();
                let _ = check.run_on_main_thread(move || log_focus(&target, "settled"));
            });
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    fn show_inactive(&self) {
        let Some(window) = self.window() else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            let target = window.clone();
            let _ = window.run_on_main_thread(move || match target.ns_window() {
                // SAFETY: Tauri's live NSWindow, on the main thread.
                Ok(ns_window) => unsafe { retone_macos::order_front_regardless(ns_window) },
                Err(_) => {
                    let _ = target.show();
                }
            });
        }
        #[cfg(not(target_os = "macos"))]
        let _ = window.show();
    }

    fn hide(&self) {
        if let Some(window) = self.window() {
            let _ = window.hide();
        }
    }

    fn destroy(&self) {
        let window = self.window();
        if let Some(handle) = self.handle() {
            self.forget(&handle.label);
        }
        if let Some(window) = window {
            tracing::debug!(target: "tone-rewrite", label = window.label(), "Destroying the tone rewrite picker");
            let _ = window.destroy();
        }
    }

    fn emit_show(&self, payload: &PickerPayload) {
        if let Some(handle) = self.handle()
            && let Err(error) =
                self.app
                    .emit_to(handle.label.as_str(), "tone-rewrite-show", payload)
        {
            tracing::warn!(target: "tone-rewrite", %error, "Could not send the picker payload");
        }
    }

    fn emit_reset(&self) {
        if let Some(handle) = self.handle() {
            let _ = self
                .app
                .emit_to(handle.label.as_str(), "tone-rewrite-reset", ());
        }
    }
}

/// Logs where the keyboard is after a show. A visible picker that is not the
/// key window cannot receive keys, so that case is a warning.
#[cfg(target_os = "macos")]
fn log_focus(window: &WebviewWindow, phase: &'static str) {
    let Ok(ns_window) = window.ns_window() else {
        return;
    };
    // SAFETY: Tauri's live NSWindow; callers run on the main thread.
    let Some(state) = (unsafe { retone_macos::panel::focus_state(ns_window) }) else {
        return;
    };
    if state.visible && !state.key {
        tracing::warn!(
            target: "tone-rewrite",
            phase,
            panel = state.panel,
            app_active = state.app_active,
            "Picker is visible but not the key window; it will not get the keyboard"
        );
    } else {
        tracing::debug!(
            target: "tone-rewrite",
            phase,
            visible = state.visible,
            key = state.key,
            panel = state.panel,
            app_active = state.app_active,
            "Picker focus"
        );
    }
}

#[cfg(target_os = "macos")]
fn cursor() -> Option<(f64, f64)> {
    retone_macos::cursor_location()
}

#[cfg(not(target_os = "macos"))]
fn cursor() -> Option<(f64, f64)> {
    None
}
