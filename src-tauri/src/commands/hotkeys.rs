//! Hotkey capture mode and backend info.

use crate::hotkeys::HotkeyService;
use serde_json::{Value, json};
use tauri::State;

/// While the settings window records a hotkey, every hotkey is released (and
/// none fires); leaving capture re-arms them.
#[tauri::command]
pub fn set_hotkey_listening_mode(service: State<'_, HotkeyService>, enabled: bool) -> Value {
    service.set_listening_mode(enabled)
}

/// macOS uses no desktop-environment backend and no evdev listener.
#[tauri::command]
pub fn get_hotkey_mode_info() -> Value {
    json!({
        "isUsingGnome": false,
        "isUsingHyprland": false,
        "isUsingKDE": false,
        "isUsingNativeShortcut": false,
        "linuxInputAccessDenied": false,
    })
}
