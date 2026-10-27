//! The settings window and the tone picker window.

use crate::tone_rewrite::controller::{PICKER_INITIAL_HEIGHT, PICKER_WIDTH};
use crate::{dock, i18n, tray};
use retone_core::navigation::{
    is_allowed_app_navigation, is_external_browser_url, is_external_open_url,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const SETTINGS_LABEL: &str = "settings";
pub const PICKER_LABEL: &str = "tone-rewrite-picker";

const SETTINGS_PAGE: &str = "index.html";
const PICKER_PAGE: &str = "index.html?tone-rewrite-picker=true";

pub const SETTINGS_WIDTH: f64 = 760.0;
pub const SETTINGS_HEIGHT: f64 = 760.0;
pub const SETTINGS_MIN_WIDTH: f64 = 560.0;
pub const SETTINGS_MIN_HEIGHT: f64 = 480.0;

/// If the page never reports a finished load, show the window anyway after this
/// long so a load failure is not an invisible app.
const SHOW_FALLBACK_DELAY: Duration = Duration::from_secs(3);

/// A settings page that has not loaded by the fallback is reloaded after this
/// long.
const RELOAD_DELAY: Duration = Duration::from_secs(1);

/// How long the reloaded page gets before the load-failure alert.
const RELOAD_GRACE: Duration = Duration::from_secs(10);

/// Set while a settings window created with `show` is waiting for its page.
static SHOW_SETTINGS_ON_LOAD: AtomicBool = AtomicBool::new(false);

/// Whether the settings page has finished loading (cleared when a load starts).
static SETTINGS_LOADED: AtomicBool = AtomicBool::new(false);

/// Set when the settings renderer died, until a reload finishes.
static SETTINGS_RENDERER_GONE: AtomicBool = AtomicBool::new(false);

/// The load-failure alert shows once per session.
static LOAD_ERROR_SHOWN: AtomicBool = AtomicBool::new(false);

/// Where the bundled frontend is served: the Vite dev server in development,
/// the `tauri://localhost` protocol in a build.
fn frontend_base(app: &AppHandle) -> Url {
    if tauri::is_dev()
        && let Some(dev_url) = app.config().build.dev_url.clone()
    {
        return dev_url;
    }
    Url::parse("tauri://localhost/").expect("static url")
}

fn page_url(app: &AppHandle, page: &str) -> Option<Url> {
    frontend_base(app).join(page).ok()
}

/// Opens a link the page navigated to in the default browser; says so when it
/// cannot (the `dialog.openLink` alert).
fn open_in_browser(app: &AppHandle, url: &str) {
    use tauri_plugin_opener::OpenerExt;
    if let Err(error) = app.opener().open_url(url, None::<&str>) {
        tracing::warn!(target: "window", "Failed to open link externally: {error}");
        let error = error.to_string();
        crate::dialog::show_error(
            i18n::t("dialog.openLink.title"),
            i18n::t_with(
                "dialog.openLink.message",
                &[("url", url), ("error", &error)],
            ),
        );
    }
}

/// Links open in the default browser, never in an in-app window or by
/// navigating the app away from itself.
fn guard_navigation(
    app: AppHandle,
    app_url: Option<Url>,
) -> impl Fn(&Url) -> bool + Send + 'static {
    move |url: &Url| {
        let target = url.as_str();
        if is_allowed_app_navigation(target, app_url.as_ref().map(Url::as_str)) {
            return true;
        }
        if is_external_browser_url(target) {
            open_in_browser(&app, target);
        } else {
            tracing::debug!(target: "window", url = target, "Blocked untrusted navigation");
        }
        false
    }
}

fn guard_new_window(
    app: AppHandle,
) -> impl Fn(Url, tauri::webview::NewWindowFeatures) -> NewWindowResponse<tauri::Wry> + Send + 'static
{
    move |url: Url, _features| {
        if is_external_open_url(url.as_str()) {
            open_in_browser(&app, url.as_str());
        } else {
            tracing::debug!(target: "window", url = url.as_str(), "Blocked untrusted window open");
        }
        NewWindowResponse::Deny
    }
}

pub fn settings_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(SETTINGS_LABEL)
}

/// Creates the settings window (hidden), showing it once its page has loaded
/// when `show` is set. An existing window is just shown.
pub fn create_settings_window(app: &AppHandle, show: bool) -> tauri::Result<WebviewWindow> {
    if let Some(window) = settings_window(app) {
        if show {
            show_settings(app);
        }
        return Ok(window);
    }

    SHOW_SETTINGS_ON_LOAD.store(show, Ordering::SeqCst);
    let app_url = page_url(app, SETTINGS_PAGE);
    let title = i18n::t("window.settingsTitle");
    let window = WebviewWindowBuilder::new(
        app,
        SETTINGS_LABEL,
        WebviewUrl::App(SETTINGS_PAGE.into()),
    )
    .title(title)
    .inner_size(SETTINGS_WIDTH, SETTINGS_HEIGHT)
    .min_inner_size(SETTINGS_MIN_WIDTH, SETTINGS_MIN_HEIGHT)
    .resizable(true)
    .minimizable(true)
    .maximizable(true)
    .visible(false)
    .center()
    .on_navigation(guard_navigation(app.clone(), app_url))
    .on_new_window(guard_new_window(app.clone()))
    .on_page_load(|window, payload| {
        if payload.event() != PageLoadEvent::Finished {
            SETTINGS_LOADED.store(false, Ordering::SeqCst);
            return;
        }
        SETTINGS_LOADED.store(true, Ordering::SeqCst);
        SETTINGS_RENDERER_GONE.store(false, Ordering::SeqCst);
        tracing::debug!(target: "window", url = payload.url().as_str(), "Settings page loaded");
        let _ = window.set_title(&i18n::t("window.settingsTitle"));
        if SHOW_SETTINGS_ON_LOAD.swap(false, Ordering::SeqCst) {
            show_settings(window.app_handle());
        }
    })
    .build()?;

    SETTINGS_LOADED.store(false, Ordering::SeqCst);
    watch_settings_load(app.clone());
    Ok(window)
}

/// Tauri reports no load failure, so a page that has not finished loading by
/// the fallback counts as one: the window is shown anyway (when it was asked
/// for), the page is reloaded once after 1 s, and if that does not load
/// either, a packaged build says so.
fn watch_settings_load(app: AppHandle) {
    let spawned = std::thread::Builder::new()
        .name("settings-load".into())
        .spawn(move || {
            std::thread::sleep(SHOW_FALLBACK_DELAY);
            if SETTINGS_LOADED.load(Ordering::SeqCst) {
                return;
            }
            if SHOW_SETTINGS_ON_LOAD.swap(false, Ordering::SeqCst) {
                tracing::warn!(target: "window", "Settings page did not finish loading; showing the window anyway");
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || show_settings(&handle));
            }
            std::thread::sleep(RELOAD_DELAY);
            if SETTINGS_LOADED.load(Ordering::SeqCst) {
                return;
            }
            tracing::warn!(target: "window", "Settings page did not finish loading; reloading it");
            reload_settings(&app);
            std::thread::sleep(RELOAD_GRACE);
            if SETTINGS_LOADED.load(Ordering::SeqCst) {
                return;
            }
            tracing::error!(target: "window", "Settings page failed to load");
            if !tauri::is_dev() {
                show_load_failure_dialog(&app, "Settings");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(target: "window", %error, "Could not watch the settings page load");
    }
}

/// Loads the settings page again (on the main thread).
fn reload_settings(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = settings_window(&handle) else {
            return;
        };
        let result = match page_url(&handle, SETTINGS_PAGE) {
            Some(url) => window.navigate(url),
            None => window.reload(),
        };
        if let Err(error) = result {
            tracing::warn!(target: "window", %error, "Failed to reload the settings page");
        }
    });
}

/// The settings window's web content process died (crash, kill, out of
/// memory): reload it after 1 s.
pub fn on_settings_renderer_gone(app: &AppHandle) {
    tracing::error!(target: "window", "Settings renderer process gone; reloading");
    SETTINGS_RENDERER_GONE.store(true, Ordering::SeqCst);
    SETTINGS_LOADED.store(false, Ordering::SeqCst);
    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("settings-reload".into())
        .spawn(move || {
            std::thread::sleep(RELOAD_DELAY);
            if SETTINGS_RENDERER_GONE.load(Ordering::SeqCst) {
                reload_settings(&handle);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(target: "window", %error, "Could not schedule the settings reload");
        reload_settings(app);
    }
}

/// The load-failure alert (once per session). Tauri gives no
/// error code, so the alert names the window and the page.
fn show_load_failure_dialog(app: &AppHandle, window_name: &str) {
    if LOAD_ERROR_SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut detail = vec![i18n::t_with(
        "dialog.loadFailure.detail.window",
        &[("windowName", window_name)],
    )];
    if let Some(url) = page_url(app, SETTINGS_PAGE) {
        detail.push(i18n::t_with(
            "dialog.loadFailure.detail.url",
            &[("url", url.as_str())],
        ));
    }
    detail.push(i18n::t("dialog.loadFailure.detail.hint"));
    crate::dialog::show_error(
        i18n::t("dialog.loadFailure.title"),
        format!(
            "{}\n\n{}",
            i18n::t("dialog.loadFailure.message"),
            detail.join("\n")
        ),
    );
}

/// Shows and focuses the settings window, creating it when needed. The Dock is
/// reported first: a window surfaced before the Dock icon appears is left
/// behind the previously frontmost app.
pub fn show_settings(app: &AppHandle) {
    let Some(window) = settings_window(app) else {
        if let Err(error) = create_settings_window(app, true) {
            tracing::error!(target: "window", "Failed to create the settings window: {error}");
        }
        return;
    };
    dock::set_settings_visible(app, true);
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    let _ = window.show();
    let _ = window.set_focus();
    if SETTINGS_RENDERER_GONE.load(Ordering::SeqCst) {
        reload_settings(app);
    }
    tray::refresh(app);
}

pub fn hide_settings(app: &AppHandle) {
    if let Some(window) = settings_window(app) {
        let _ = window.hide();
    }
    dock::set_settings_visible(app, false);
    tray::refresh(app);
}

pub fn is_settings_visible(app: &AppHandle) -> bool {
    settings_window(app)
        .is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
}

/// Re-applies localized window chrome after a language change.
pub fn refresh_localized_ui(app: &AppHandle) {
    if let Some(window) = settings_window(app) {
        let _ = window.set_title(&i18n::t("window.settingsTitle"));
    }
}

/// The tone picker: frameless, transparent, above everything on every Space,
/// excluded from screen capture, created hidden and reused for every press.
/// Each window gets its own `label` (`tone-rewrite-picker-<n>`), so one
/// recreated after a crash never collides with the one being torn down.
/// `on_loaded` runs whenever its page finishes loading.
///
/// Must run on the main thread (the NSWindow is configured here).
pub fn create_picker_window(
    app: &AppHandle,
    label: &str,
    on_loaded: impl Fn() + Send + Sync + 'static,
) -> tauri::Result<WebviewWindow> {
    let app_url = page_url(app, PICKER_PAGE);
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::App(PICKER_PAGE.into()))
        .title("ReTone")
        .inner_size(PICKER_WIDTH, PICKER_INITIAL_HEIGHT)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .closable(false)
        .focused(false)
        .shadow(false)
        .content_protected(true)
        .accept_first_mouse(true)
        .visible(false)
        .on_navigation(guard_navigation(app.clone(), app_url))
        .on_new_window(guard_new_window(app.clone()))
        .on_page_load(move |_window, payload| {
            if payload.event() == PageLoadEvent::Finished {
                tracing::debug!(target: "window", url = payload.url().as_str(), "Picker page loaded");
                on_loaded();
            }
        })
        .build()?;

    #[cfg(target_os = "macos")]
    match window.ns_window() {
        Ok(ns_window) => {
            // SAFETY: Tauri's live NSWindow (tao's `TaoWindow`), and window
            // setup runs on the main thread.
            unsafe { retone_macos::configure_overlay_window(ns_window) };
            // Takes the keyboard without activating ReTone; see
            // `retone_macos::panel`.
            let panel = unsafe { retone_macos::panel::make_nonactivating_panel(ns_window) };
            if panel {
                // The class swap keeps the window's level and Space behaviour,
                // but set them again in case the panel's defaults applied.
                unsafe { retone_macos::configure_overlay_window(ns_window) };
            } else {
                tracing::warn!(target: "window", "Picker stays an ordinary window; showing it will activate ReTone");
            }
        }
        Err(error) => tracing::warn!(target: "window", "Picker has no NSWindow: {error}"),
    }
    Ok(window)
}

/// Whether `label` names a picker window (any generation).
pub fn is_picker_label(label: &str) -> bool {
    label
        .strip_prefix(PICKER_LABEL)
        .is_some_and(|rest| rest.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_labels_carry_a_generation() {
        assert!(is_picker_label("tone-rewrite-picker-1"));
        assert!(is_picker_label("tone-rewrite-picker-42"));
        assert!(!is_picker_label("tone-rewrite-picker"));
        assert!(!is_picker_label(SETTINGS_LABEL));
        assert!(!is_picker_label("tone-rewrite-pickerX"));
    }
}
