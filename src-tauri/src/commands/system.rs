//! App info, links, startup preferences and the UI language.

use crate::app::{AppState, StartupPreferences};
use crate::env::{START_MINIMIZED, UI_LANGUAGE};
use crate::{i18n, menu, tray, windows};
#[cfg(target_os = "macos")]
use retone_core::login_item::LoginItemPort;
use retone_core::login_item::{self, AutoStartState};
use retone_core::navigation::{UrlOpener, open_external_url, validate_external_url};
use serde_json::{Value, json};
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

struct PluginOpener<'a>(&'a AppHandle);

impl UrlOpener for PluginOpener<'_> {
    fn open(&self, url: &str) -> Result<(), String> {
        self.0
            .opener()
            .open_url(url, None::<&str>)
            .map_err(|e| e.to_string())
    }
}

pub(crate) fn open_url(app: &AppHandle, url: &str) -> Value {
    match open_external_url(&PluginOpener(app), url) {
        Ok(()) => json!({ "success": true }),
        Err(error) => json!({ "success": false, "error": error }),
    }
}

#[tauri::command]
pub fn get_app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
pub fn open_external(app: AppHandle, url: String) -> Value {
    if let Err(error) = validate_external_url(&url) {
        return json!({ "success": false, "error": error.to_string() });
    }
    open_url(&app, &url)
}

/// Writes plain text to the general pasteboard.
#[tauri::command]
pub fn write_clipboard(state: State<'_, AppState>, text: String) -> Value {
    match state.input.write_clipboard(&text) {
        Ok(()) => json!({ "success": true }),
        Err(error) => {
            tracing::warn!(target: "clipboard", %error, "Clipboard write failed");
            json!({ "success": false, "error": error.to_string() })
        }
    }
}

/// Launch at login: `{ enabled, requiresApproval }` from `SMAppService.mainApp`.
/// An item awaiting approval in System Settings reads as off, with the reason.
#[tauri::command]
pub fn get_auto_start_enabled() -> AutoStartState {
    #[cfg(target_os = "macos")]
    {
        let item = retone_macos::login_item::MacLoginItem;
        let status = item.status();
        tracing::debug!(target: "app", status = status.as_str(), "Launch at login status");
        login_item::resolve_auto_start_state(status)
    }
    #[cfg(not(target_os = "macos"))]
    AutoStartState::default()
}

/// Registers or unregisters the login item. The renderer reads the state back
/// afterwards (it may be awaiting approval).
#[tauri::command]
pub fn set_auto_start_enabled(enabled: bool) -> Value {
    #[cfg(target_os = "macos")]
    let result =
        login_item::set_auto_start_enabled(&retone_macos::login_item::MacLoginItem, enabled);
    #[cfg(not(target_os = "macos"))]
    let result: Result<(), String> = Err(login_item::UNAVAILABLE_MESSAGE.to_string());
    match result {
        Ok(()) => {
            tracing::debug!(target: "app", enabled, "Auto-start setting updated");
            json!({ "success": true })
        }
        Err(error) => {
            tracing::error!(target: "app", enabled, %error, "Error setting auto-start");
            json!({ "success": false, "error": error })
        }
    }
}

#[tauri::command]
pub fn get_start_minimized(state: State<'_, AppState>) -> bool {
    state.env.start_minimized()
}

#[tauri::command]
pub fn set_start_minimized(state: State<'_, AppState>, enabled: bool) -> Value {
    let value = if enabled { "true" } else { "false" };
    if let Err(error) = state.env.set(START_MINIMIZED, Some(value)) {
        tracing::warn!(target: "environment", "Failed to save START_MINIMIZED: {error}");
    }
    json!({ "success": true })
}

#[tauri::command]
pub fn get_ui_language(state: State<'_, AppState>) -> String {
    state.env.ui_language()
}

#[tauri::command]
pub fn set_ui_language(app: AppHandle, state: State<'_, AppState>, language: String) -> Value {
    let normalized = i18n::change_language(&language);
    if let Err(error) = state.env.set(UI_LANGUAGE, Some(normalized)) {
        tracing::warn!(target: "environment", "Failed to save UI_LANGUAGE: {error}");
    }
    windows::refresh_localized_ui(&app);
    menu::install(&app);
    tray::refresh(&app);
    json!({ "success": true, "language": normalized })
}

/// The renderer's startup preferences: keep the shared llama-server loaded
/// instead of unloading it after 5 idle minutes, and the whole-field fallback.
#[tauri::command]
pub fn sync_startup_preferences(
    app: AppHandle,
    state: State<'_, AppState>,
    prefs: StartupPreferences,
) {
    if let Ok(mut current) = state.startup_preferences.lock() {
        *current = prefs;
    }
    state
        .local
        .server()
        .set_keep_resident(prefs.keep_local_model_loaded);
    if let Some(controller) = crate::tone_rewrite::controller(&app) {
        controller.set_whole_field(prefs.tone_rewrite_whole_field);
    }
}
