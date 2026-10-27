//! The macOS application menu. Its Edit roles are
//! what make ⌘C/⌘V/⌘A work in the settings window's inputs.

use crate::i18n;
use tauri::menu::{AboutMetadata, Menu, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Wry};

#[cfg(debug_assertions)]
const RELOAD_ID: &str = "menu-reload";
#[cfg(debug_assertions)]
const DEVTOOLS_ID: &str = "menu-toggle-devtools";

pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let app_menu = Submenu::with_items(
        app,
        i18n::t("menu.appLabel"),
        true,
        &[
            &PredefinedMenuItem::about(app, None, Some(AboutMetadata::default()))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some(&i18n::t("menu.quit")))?,
        ],
    )?;

    // These menus keep the standard English labels. No "Paste and Match Style"
    // or "Delete" items: muda has no such roles, and plain-text inputs don't
    // need them.
    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    let window_menu = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::close_window(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::bring_all_to_front(app, None)?,
        ],
    )?;

    #[cfg(debug_assertions)]
    {
        use tauri::menu::MenuItem;
        // Developer-only (debug builds): reload and the web inspector.
        let view_menu = Submenu::with_items(
            app,
            "View",
            true,
            &[
                &MenuItem::with_id(app, RELOAD_ID, "Reload", true, Some("CmdOrCtrl+R"))?,
                &MenuItem::with_id(
                    app,
                    DEVTOOLS_ID,
                    "Toggle Developer Tools",
                    true,
                    Some("Alt+CmdOrCtrl+I"),
                )?,
            ],
        )?;
        Menu::with_items(app, &[&app_menu, &edit_menu, &view_menu, &window_menu])
    }

    #[cfg(not(debug_assertions))]
    Menu::with_items(app, &[&app_menu, &edit_menu, &window_menu])
}

/// Installs (or re-installs after a language change) the application menu.
pub fn install(app: &AppHandle) {
    match build(app) {
        Ok(menu) => {
            if let Err(error) = app.set_menu(menu) {
                tracing::warn!(target: "menu", "Failed to set the app menu: {error}");
            }
        }
        Err(error) => tracing::warn!(target: "menu", "Failed to build the app menu: {error}"),
    }
}

/// App-menu clicks that are not predefined roles.
pub fn handle_event(app: &AppHandle, id: &str) {
    #[cfg(debug_assertions)]
    {
        use tauri::Manager;
        let Some(window) = app
            .webview_windows()
            .into_values()
            .find(|w| w.is_focused().unwrap_or(false))
        else {
            return;
        };
        match id {
            RELOAD_ID => {
                let _ = window.eval("window.location.reload()");
            }
            DEVTOOLS_ID => {
                if window.is_devtools_open() {
                    window.close_devtools();
                } else {
                    window.open_devtools();
                }
            }
            _ => {}
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = (app, id);
}
