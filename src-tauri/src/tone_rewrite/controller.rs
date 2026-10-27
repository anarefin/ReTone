//! The ReTone flow.
//!
//! Hotkey → capture the selection (or, with the whole-field setting on, select
//! the whole field) → tone picker at the cursor → the picker's rewrite replaces
//! the selection in place. The model call runs in the picker's renderer; the
//! paste stays here, behind the session's revalidation of the selection.
//!
//! The picker window is created once (`prewarm`, or the first press) and then
//! hidden and shown again, so a press never waits on a renderer loading the
//! app bundle. Each open pushes its payload with `tone-rewrite-show`; each
//! close sends `tone-rewrite-reset` so the renderer drops a running rewrite.
//!
//! The window and the selection side sit behind [`PickerHost`] and
//! [`SelectionSide`], so the flow is tested with fakes.

use retone_core::payload::{
    CapturePort, Display, MIN_PICKER_HEIGHT, PickerPayload, Point, REQUEST_TAG, Rect,
    SHOW_SIZE_WAIT, capture_selection, display_nearest_point, picker_bounds,
};
use retone_core::session::ReplaceError;
use serde::Serialize;
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::oneshot;

pub const PICKER_WIDTH: f64 = 280.0;
/// The picker's size before its renderer reports one.
pub const PICKER_INITIAL_HEIGHT: f64 = 264.0;

/// The picker window. A window has a generation (never 0); a recreated window
/// gets a new one, so work begun for an old window can tell it is gone.
pub trait PickerHost: Send + Sync + 'static {
    /// Creates the hidden picker when there is none and returns the live
    /// window's generation, or 0 when it could not be created.
    fn create(&self) -> impl Future<Output = u64> + Send;
    /// The live window's generation, 0 when there is none.
    fn generation(&self) -> u64;
    /// Resolves once that window's page has loaded (or gave up waiting).
    fn wait_loaded(&self, generation: u64) -> impl Future<Output = ()> + Send;
    /// The cursor and the displays, in global points.
    fn cursor_and_displays(&self) -> Option<(Point, Vec<Display>)>;
    fn set_bounds(&self, bounds: Rect);
    /// Shows the picker and gives it key focus (keyboard navigation).
    fn show_and_focus(&self);
    /// Shows the picker without taking focus from the target app.
    fn show_inactive(&self);
    fn hide(&self);
    /// Destroys the window (the hotkey was cleared, or its renderer died).
    fn destroy(&self);
    fn emit_show(&self, payload: &PickerPayload);
    fn emit_reset(&self);
}

/// The selection side: capture (via [`CapturePort`]), refocus, replace.
pub trait SelectionSide: CapturePort + Send + Sync + 'static {
    /// Brings the captured target app frontmost again.
    fn activate_target(&self) -> impl Future<Output = ()> + Send;
    fn replace(
        &self,
        session_id: String,
        text: String,
    ) -> impl Future<Output = Result<(), ReplaceError>> + Send;
    /// Leaves a rewrite that could not be pasted on the clipboard.
    fn write_clipboard(&self, text: &str);
    /// Cancels local model requests whose id starts with `prefix`; a rewrite
    /// left running holds the local model busy.
    fn cancel_tagged(&self, prefix: &str);
}

/// What `tone_rewrite_apply` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub copied: bool,
}

impl ApplyResult {
    fn ok() -> Self {
        ApplyResult {
            success: true,
            code: None,
            copied: false,
        }
    }

    fn failed(code: &str, copied: bool) -> Self {
        ApplyResult {
            success: false,
            code: Some(code.to_string()),
            copied,
        }
    }
}

#[derive(Default)]
struct State {
    /// The open's payload and its id (each open gets a new one).
    payload: Option<(u64, PickerPayload)>,
    next_open: u64,
    /// The cursor point the picker opened at, and that display's work area.
    anchor: Option<(Point, Rect)>,
    bounds: Option<Rect>,
    opening: bool,
    applying: bool,
    visible: bool,
    height_ack: Option<oneshot::Sender<()>>,
}

pub struct ToneRewriteController<H, S> {
    host: H,
    selection: S,
    state: Mutex<State>,
    /// Off until the renderer syncs the setting, so a press never selects the
    /// whole field unasked.
    whole_field: AtomicBool,
}

impl<H: PickerHost, S: SelectionSide> ToneRewriteController<H, S> {
    pub fn new(host: H, selection: S) -> Self {
        ToneRewriteController {
            host,
            selection,
            state: Mutex::new(State::default()),
            whole_field: AtomicBool::new(false),
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    #[cfg(test)]
    pub fn selection(&self) -> &S {
        &self.selection
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_whole_field(&self, enabled: bool) {
        self.whole_field.store(enabled, Ordering::SeqCst);
    }

    pub fn whole_field(&self) -> bool {
        self.whole_field.load(Ordering::SeqCst)
    }

    pub fn is_visible(&self) -> bool {
        self.lock().visible
    }

    /// A hotkey press: opens the picker over the selection, or closes it when
    /// it is open. Ignored while an open or an apply is under way.
    pub async fn trigger(&self) {
        let open = {
            let mut state = self.lock();
            if state.opening || state.applying {
                tracing::debug!(
                    target: "tone-rewrite",
                    opening = state.opening,
                    applying = state.applying,
                    "Hotkey pressed while the picker is busy; ignored"
                );
                return;
            }
            state.opening = !state.visible;
            state.opening
        };
        if !open {
            tracing::info!(target: "tone-rewrite", "Hotkey pressed; closing the picker");
            self.close(true).await;
            return;
        }
        // Clears `opening` however the open ends.
        struct Opening<'a>(&'a Mutex<State>);
        impl Drop for Opening<'_> {
            fn drop(&mut self) {
                if let Ok(mut state) = self.0.lock() {
                    state.opening = false;
                }
            }
        }
        let _opening = Opening(&self.state);

        tracing::info!(target: "tone-rewrite", "Hotkey pressed; opening the picker");
        let started = Instant::now();
        let payload = capture_selection(&self.selection, self.whole_field()).await;
        tracing::debug!(
            target: "tone-rewrite",
            state = payload.state(),
            code = payload.code(),
            selected_all = matches!(payload, PickerPayload::Ready { selected_all: true, .. }),
            capture_ms = started.elapsed().as_millis() as u64,
            "Tone rewrite capture"
        );
        self.open_picker(payload).await;
        tracing::debug!(
            target: "tone-rewrite",
            total_ms = started.elapsed().as_millis() as u64,
            visible = self.is_visible(),
            "Tone rewrite picker shown"
        );
    }

    /// Creates and loads the hidden picker ahead of the first press. Safe to
    /// call repeatedly; a window that crashed or was destroyed is created again.
    pub async fn prewarm(&self) {
        let mut generation = self.host.generation();
        if generation == 0 {
            generation = self.host.create().await;
        }
        if generation != 0 {
            self.host.wait_loaded(generation).await;
        }
    }

    fn anchor(&self) -> Option<(Point, Rect)> {
        if let Some(anchor) = self.lock().anchor {
            return Some(anchor);
        }
        // Outside the lock: the host may wait on the main thread.
        let (cursor, displays) = self.host.cursor_and_displays()?;
        let display = display_nearest_point(&displays, cursor)?;
        let anchor = (cursor, display.work_area);
        Some(*self.lock().anchor.get_or_insert(anchor))
    }

    pub async fn open_picker(&self, payload: PickerPayload) {
        let mut generation = self.host.generation();
        if generation == 0 {
            generation = self.host.create().await;
            if generation == 0 {
                tracing::warn!(target: "tone-rewrite", "Tone rewrite picker could not be created");
                return;
            }
        }
        self.host.wait_loaded(generation).await;
        if self.host.generation() != generation {
            return;
        }

        self.lock().anchor = None;
        let bounds = self.anchor().map(|(cursor, work_area)| {
            picker_bounds(cursor, work_area, PICKER_WIDTH, PICKER_INITIAL_HEIGHT)
        });
        let (sized_tx, sized_rx) = oneshot::channel();
        let open_id = {
            let mut state = self.lock();
            state.next_open += 1;
            let open_id = state.next_open;
            state.payload = Some((open_id, payload.clone()));
            state.bounds = bounds;
            state.height_ack = Some(sized_tx);
            open_id
        };
        if let Some(bounds) = bounds {
            self.host.set_bounds(bounds);
        }
        self.host.emit_show(&payload);
        // Wait for the first size the renderer reports for this payload, so
        // the previous open's size never flashes.
        let _ = tokio::time::timeout(SHOW_SIZE_WAIT, sized_rx).await;
        {
            let mut state = self.lock();
            state.height_ack = None;
            let current = state.payload.as_ref().map(|(id, _)| *id);
            if self.host.generation() != generation || current != Some(open_id) {
                return;
            }
            state.visible = true;
        }
        self.host.show_and_focus();
    }

    /// The renderer's height for its content. Sizes the picker from the same
    /// cursor point it opened at, so growing never drifts it away from the
    /// selection, and releases a waiting open.
    pub fn set_height(&self, height: f64) {
        if !height.is_finite() || height < MIN_PICKER_HEIGHT {
            return;
        }
        let Some((cursor, work_area)) = self.anchor() else {
            return;
        };
        let bounds = picker_bounds(cursor, work_area, PICKER_WIDTH, height.ceil());
        let (changed, ack) = {
            let mut state = self.lock();
            let changed = state.bounds != Some(bounds);
            state.bounds = Some(bounds);
            (changed, state.height_ack.take())
        };
        if changed {
            self.host.set_bounds(bounds);
        }
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    }

    /// The picker pulls its payload once mounted, so it never misses a push
    /// sent before its listener existed.
    pub fn payload(&self) -> Option<PickerPayload> {
        self.lock()
            .payload
            .as_ref()
            .map(|(_, payload)| payload.clone())
    }

    /// Pastes the rewrite over the captured selection. On failure the rewrite
    /// is left on the clipboard and the picker comes back (without focus) to
    /// say why.
    pub async fn apply(&self, session_id: String, text: String) -> ApplyResult {
        {
            let mut state = self.lock();
            // A rewrite that finishes after the picker closed must not paste.
            if !state.visible {
                return ApplyResult::failed("not_picker", false);
            }
            if text.trim().is_empty() {
                return ApplyResult::failed("invalid_replacement", false);
            }
            state.applying = true;
        }
        let generation = self.host.generation();
        // The picker holds focus; the target must be frontmost again before
        // the selection is re-read and replaced, and before the picker hides.
        self.selection.activate_target().await;
        self.host.hide();
        let result = match self.selection.replace(session_id, text.clone()).await {
            Ok(()) => {
                self.close(false).await;
                ApplyResult::ok()
            }
            Err(error) => {
                // Nothing was pasted: keep the rewrite on the clipboard and show why.
                self.selection.write_clipboard(&text);
                tracing::debug!(target: "tone-rewrite", code = error.as_str(), "Tone rewrite paste declined");
                if generation != 0 && self.host.generation() == generation {
                    self.host.show_inactive();
                }
                ApplyResult::failed(error.as_str(), true)
            }
        };
        self.lock().applying = false;
        result
    }

    /// Hides the picker (never destroys it, keeping the loaded renderer for
    /// the next press), tells the renderer to drop a running rewrite and
    /// cancels local requests. With `return_focus`, a visible picker hands
    /// focus back to the target app first.
    pub async fn close(&self, return_focus: bool) {
        let was_visible = {
            let mut state = self.lock();
            let was_visible = state.visible;
            state.payload = None;
            state.anchor = None;
            state.visible = false;
            state.height_ack = None;
            was_visible
        };
        // A blur or a second hotkey press hides the picker without its
        // renderer cancelling, and a local rewrite left running holds the
        // model busy.
        self.selection.cancel_tagged(REQUEST_TAG);
        let generation = self.host.generation();
        if generation == 0 {
            return;
        }
        self.host.emit_reset();
        if return_focus && was_visible {
            // The blur this causes finds the picker no longer visible.
            self.selection.activate_target().await;
            // A press during the wait may have opened the picker again.
            if self.lock().visible || self.host.generation() != generation {
                return;
            }
        }
        self.host.hide();
    }

    /// The picker lost focus: another app already took it, so there is
    /// nothing to hand back. Returns whether a close is due.
    pub fn should_close_on_blur(&self) -> bool {
        let state = self.lock();
        state.visible && !state.applying
    }

    /// The picker window is gone (destroyed or crashed).
    pub fn window_gone(&self) {
        self.selection.cancel_tagged(REQUEST_TAG);
        let mut state = self.lock();
        state.payload = None;
        state.visible = false;
        state.anchor = None;
        state.height_ack = None;
    }

    /// The hotkey was cleared: the picker is no longer needed.
    pub fn destroy(&self) {
        {
            let mut state = self.lock();
            state.payload = None;
            state.visible = false;
        }
        self.host.destroy();
    }
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
