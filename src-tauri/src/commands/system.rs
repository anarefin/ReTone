//! App info, links, startup preferences and the UI language.

use crate::app::{AppState, StartupPreferences};
use crate::env::{APP_ICON, START_MINIMIZED, UI_LANGUAGE};
use crate::{app_icon, i18n, menu, tray, windows};
use retone_core::app_icon::AppIcon;
#[cfg(target_os = "macos")]
use retone_core::login_item::LoginItemPort;
use retone_core::login_item::{self, AutoStartState};
use retone_core::navigation::{UrlOpener, open_external_url, validate_external_url};
use retone_core::update_check::{self, LATEST_RELEASE_URL, UpdateStatus};
use serde_json::{Value, json};
use std::time::Duration;
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

/// Asks GitHub for the latest published release:
/// `{ status: "upToDate" | "available" | "failed", currentVersion, latestVersion?, url? }`.
/// No release yet (404) counts as up to date. Response bodies are never logged.
#[tauri::command]
pub async fn check_for_update(app: AppHandle, state: State<'_, AppState>) -> Result<Value, ()> {
    let current = app.package_info().version.to_string();
    let failed = |reason: &str| {
        tracing::warn!(target: "app", reason, "Update check failed");
        json!({ "status": "failed", "currentVersion": current })
    };
    let response = match state
        .http
        .follow
        .get(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) if error.is_timeout() => return Ok(failed("timeout")),
        Err(_) => return Ok(failed("network")),
    };
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(json!({ "status": "upToDate", "currentVersion": current }));
    }
    if !status.is_success() {
        return Ok(failed(status.as_str()));
    }
    let release: Value = match response.json().await {
        Ok(release) => release,
        Err(_) => return Ok(failed("invalid json")),
    };
    Ok(match update_check::evaluate(&current, &release) {
        Ok(UpdateStatus::UpToDate) => json!({ "status": "upToDate", "currentVersion": current }),
        Ok(UpdateStatus::Available { version, url }) => {
            tracing::info!(target: "app", version, "Update available");
            json!({
                "status": "available",
                "currentVersion": current,
                "latestVersion": version,
                "url": url,
            })
        }
        Err(reason) => failed(reason),
    })
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
pub fn get_app_icon(state: State<'_, AppState>) -> &'static str {
    state.env.app_icon().id()
}

/// Saves the app icon picked in Settings and shows it in the Dock and menu bar.
/// Sync, so it runs on the main thread like the other tray changes.
#[tauri::command]
pub fn set_app_icon(app: AppHandle, state: State<'_, AppState>, icon: String) -> Value {
    let Some(icon) = AppIcon::parse(&icon) else {
        return json!({ "success": false, "error": "unknown_icon" });
    };
    if let Err(error) = state.env.set(APP_ICON, Some(icon.id())) {
        tracing::warn!(target: "environment", "Failed to save APP_ICON: {error}");
    }
    app_icon::apply(&app, icon);
    json!({ "success": true, "icon": icon.id() })
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
