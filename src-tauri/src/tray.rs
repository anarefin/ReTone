//! The menu-bar item: template icon, Open/Hide Settings and
//! Quit. The menu is rebuilt whenever the settings window is shown or hidden so
//! its first item reads right, and on a language change.

use crate::{app, app_icon, i18n, windows};
use retone_core::tray_placement::{MACOS_TRAY_GUID, TrayPlacementStep, macos_tray_placement_plan};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Wry};

pub const TRAY_ID: &str = "retone-tray";
const TOGGLE_SETTINGS_ID: &str = "tray-toggle-settings";
const QUIT_ID: &str = "tray-quit";

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let settings_label = if windows::is_settings_visible(app) {
        i18n::t("tray.hideSettings")
    } else {
        i18n::t("tray.openSettings")
    };
    let toggle = MenuItem::with_id(app, TOGGLE_SETTINGS_ID, settings_label, true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, QUIT_ID, i18n::t("tray.quit"), true, None::<&str>)?;
    Menu::with_items(app, &[&toggle, &separator, &quit])
}

fn toggle_settings(app: &AppHandle) {
    if windows::is_settings_visible(app) {
        windows::hide_settings(app);
    } else {
        windows::show_settings(app);
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    for step in macos_tray_placement_plan() {
        match step {
            TrayPlacementStep::RegisterDefault { key, position } => {
                #[cfg(target_os = "macos")]
                retone_macos::register_integer_default(&key, position);
                #[cfg(not(target_os = "macos"))]
                let _ = (key, position);
            }
            TrayPlacementStep::CreateItem => {
                let icon = Image::from_bytes(app_icon::tray_template(app_icon::saved(app)))?;
                TrayIconBuilder::with_id(TRAY_ID)
                    .icon(icon)
                    .icon_as_template(true)
                    .tooltip(i18n::t("tray.tooltip"))
                    .menu(&build_menu(app)?)
                    .show_menu_on_left_click(true)
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        TOGGLE_SETTINGS_ID => toggle_settings(app),
                        QUIT_ID => {
                            tracing::info!(target: "tray", "Quitting app via tray menu");
                            app::quit(app);
                        }
                        _ => {}
                    })
                    .build(app)?;
            }
            TrayPlacementStep::SetAutosaveName(name) => set_autosave_name(app, name),
        }
    }
    Ok(())
}

fn set_autosave_name(app: &AppHandle, name: &'static str) {
    debug_assert_eq!(name, MACOS_TRAY_GUID);
    #[cfg(target_os = "macos")]
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let result = tray.with_inner_tray_icon(move |inner| {
            if let Some(item) = inner.ns_status_item() {
                retone_macos::set_status_item_autosave_name(&item, name);
            }
        });
        if let Err(error) = result {
            tracing::warn!(target: "tray", "Failed to set the tray autosave name: {error}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Rebuilds the menu and tooltip (visibility or language changed).
/// Swaps the menu-bar image for another template image.
pub fn set_icon(app: &AppHandle, template_png: &[u8]) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let result = Image::from_bytes(template_png)
        .and_then(|icon| tray.set_icon(Some(icon)))
        .and_then(|()| tray.set_icon_as_template(true));
    if let Err(error) = result {
        tracing::warn!(target: "tray", "Failed to change the tray icon: {error}");
    }
}

pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    match build_menu(app) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(error) => tracing::warn!(target: "tray", "Failed to rebuild the tray menu: {error}"),
    }
    let _ = tray.set_tooltip(Some(i18n::t("tray.tooltip")));
}
