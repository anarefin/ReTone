//! The ReTone flow: the entry points the hotkeys and window events call, over
//! one [`controller::ToneRewriteController`] kept in Tauri's managed state.
//!
//! Hotkey presses arrive on the main thread (global shortcuts) or the
//! `hotkey-events` thread (native listener); the work is moved onto the async
//! runtime so neither is held up by a capture.

pub mod controller;
pub mod host;
pub mod selection;

use crate::app::AppState;
use controller::ToneRewriteController;
use host::TauriHost;
use selection::TauriSelection;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub type AppController = ToneRewriteController<TauriHost, TauriSelection>;

/// The managed controller.
pub struct ToneRewrite(pub Arc<AppController>);

/// Creates the controller (no window yet: `prewarm` or the first press does).
pub fn install(app: &AppHandle) {
    let controller = ToneRewriteController::new(
        TauriHost::new(app.clone()),
        TauriSelection::new(app.clone()),
    );
    let whole_field = app
        .state::<AppState>()
        .startup_preferences
        .lock()
        .map(|prefs| prefs.tone_rewrite_whole_field)
        .unwrap_or(false);
    controller.set_whole_field(whole_field);
    app.manage(ToneRewrite(Arc::new(controller)));
}

pub fn controller(app: &AppHandle) -> Option<Arc<AppController>> {
    app.try_state::<ToneRewrite>().map(|state| state.0.clone())
}

/// A hotkey press: opens the picker over the selection, or closes it when it
/// is open.
pub fn toggle(app: &AppHandle) {
    let Some(controller) = controller(app) else {
        tracing::warn!(target: "tone-rewrite", "Hotkey pressed before the ReTone flow was ready");
        return;
    };
    tauri::async_runtime::spawn(async move { controller.trigger().await });
}

/// Creates and loads the hidden picker so the first press shows it at once.
/// The local model is warmed by the picker itself while it is open
/// (`warmToneRewriteModel` → `llama_server_start`).
pub fn prewarm(app: &AppHandle) {
    let Some(controller) = controller(app) else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        controller.prewarm().await;
        tracing::debug!(target: "tone-rewrite", "Tone rewrite picker prewarmed");
    });
}

/// The hotkey was cleared: the picker is no longer needed.
pub fn destroy(app: &AppHandle) {
    if let Some(controller) = controller(app) {
        controller.destroy();
    }
}

/// The picker lost focus: another app took it, so close without handing focus
/// back.
pub fn on_picker_blur(app: &AppHandle, label: &str) {
    let Some(controller) = controller(app) else {
        return;
    };
    if !controller.host().is_current(label) || !controller.should_close_on_blur() {
        return;
    }
    tracing::debug!(target: "tone-rewrite", "Picker lost focus; closing");
    tauri::async_runtime::spawn(async move { controller.close(false).await });
}

/// A picker window was destroyed.
pub fn on_picker_destroyed(app: &AppHandle, label: &str) {
    if let Some(controller) = controller(app)
        && controller.host().forget(label)
    {
        tracing::debug!(target: "tone-rewrite", label, "Tone rewrite picker closed");
        controller.window_gone();
    }
}

/// The picker's web content process died: drop the window, so the next press
/// creates a fresh one.
pub fn on_picker_renderer_gone(app: &AppHandle, label: &str) {
    let Some(controller) = controller(app) else {
        return;
    };
    if !controller.host().is_current(label) {
        return;
    }
    tracing::warn!(target: "tone-rewrite", label, "Tone rewrite picker renderer terminated; recreating on the next press");
    controller.window_gone();
    let label = label.to_string();
    // Not from inside WebKit's delegate callback.
    let _ = app.run_on_main_thread(move || {
        if controller.host().is_current(&label) {
            controller.destroy();
        }
    });
}
