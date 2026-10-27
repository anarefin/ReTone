//! macOS privacy settings.

use super::system::open_url;
use crate::app::AppState;
use serde_json::Value;
use tauri::{AppHandle, State};

const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
const LOGIN_ITEMS_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";

/// `AXIsProcessTrusted`. A non-silent check reuses its verdict for 5 s and
/// shows the denial dialog when untrusted; the settings window checks silently.
#[tauri::command]
pub fn check_accessibility_permission(state: State<'_, AppState>, silent: Option<bool>) -> bool {
    state.input.check_accessibility(silent.unwrap_or(false))
}

#[tauri::command]
pub fn open_accessibility_settings(app: AppHandle) -> Value {
    open_url(&app, ACCESSIBILITY_SETTINGS_URL)
}

#[tauri::command]
pub fn open_login_items_settings(app: AppHandle) -> Value {
    open_url(&app, LOGIN_ITEMS_SETTINGS_URL)
}
