//! ReTone picker commands. Payload, apply, close and height answer only the
//! live picker window.

use crate::app::AppState;
use crate::hotkeys::HotkeyService;
use crate::tone_rewrite::{self, controller::ApplyResult};
use retone_core::payload::PickerPayload;
use serde_json::Value;
use tauri::{AppHandle, State, WebviewWindow};

/// Registers the hotkey (atomically) before saving it; an empty hotkey clears it.
/// Sync, so it runs on the main thread like the global-shortcut calls it makes.
#[tauri::command]
pub fn update_tone_rewrite_hotkey(service: State<'_, HotkeyService>, hotkey: String) -> Value {
    service.update_tone_rewrite_hotkey(&hotkey)
}

#[tauri::command]
pub fn get_tone_rewrite_key(state: State<'_, AppState>) -> String {
    state.env.tone_rewrite_key()
}

fn picker_controller(
    app: &AppHandle,
    window: &WebviewWindow,
) -> Option<std::sync::Arc<tone_rewrite::AppController>> {
    tone_rewrite::controller(app).filter(|c| c.host().is_current(window.label()))
}

/// The open's payload; the picker pulls it once mounted.
#[tauri::command]
pub fn tone_rewrite_get_payload(app: AppHandle, window: WebviewWindow) -> Option<PickerPayload> {
    picker_controller(&app, &window)?.payload()
}

#[tauri::command]
pub async fn tone_rewrite_apply(
    app: AppHandle,
    window: WebviewWindow,
    session_id: String,
    text: String,
) -> ApplyResult {
    let Some(controller) = tone_rewrite::controller(&app) else {
        return ApplyResult {
            success: false,
            code: Some("unavailable".into()),
            copied: false,
        };
    };
    if !controller.host().is_current(window.label()) {
        return ApplyResult {
            success: false,
            code: Some("not_picker".into()),
            copied: false,
        };
    }
    controller.apply(session_id, text).await
}

/// Esc in the picker: hide it and hand focus back to the target.
#[tauri::command]
pub async fn tone_rewrite_close(app: AppHandle, window: WebviewWindow) {
    if let Some(controller) = picker_controller(&app, &window) {
        controller.close(true).await;
    }
}

/// The renderer's content height. Any number;
/// the controller ignores what is not a usable height.
#[tauri::command]
pub async fn tone_rewrite_set_height(app: AppHandle, window: WebviewWindow, height: Value) {
    if let Some(controller) = picker_controller(&app, &window) {
        controller.set_height(height.as_f64().unwrap_or(f64::NAN));
    }
}
