//! Applies the app icon picked in Settings to the Dock tile and the menu-bar
//! item. The `.app` bundle always carries the default icon (Finder, Launchpad);
//! another choice only replaces what the running app shows.

use crate::app::AppState;
use retone_core::app_icon::AppIcon;
use tauri::{AppHandle, Manager};

const LETTER_TEMPLATE: &[u8] = include_bytes!("../../src/assets/app-icons/letterTemplate@3x.png");

/// The Dock image for an icon (512 px, macOS icon grid).
fn dock_png(icon: AppIcon) -> &'static [u8] {
    match icon {
        AppIcon::Mono => include_bytes!("../../src/assets/app-icons/mono.png"),
        AppIcon::Light => include_bytes!("../../src/assets/app-icons/light.png"),
        AppIcon::Aurora => include_bytes!("../../src/assets/app-icons/aurora.png"),
        AppIcon::Ring => include_bytes!("../../src/assets/app-icons/ring.png"),
    }
}

/// The menu-bar template image for an icon.
pub fn tray_template(icon: AppIcon) -> &'static [u8] {
    match icon {
        AppIcon::Mono => include_bytes!("../../src/assets/iconTemplate@3x.png"),
        AppIcon::Light | AppIcon::Aurora => LETTER_TEMPLATE,
        AppIcon::Ring => include_bytes!("../../src/assets/app-icons/ringTemplate@3x.png"),
    }
}

/// The saved choice; the default icon before `AppState` exists.
pub fn saved(app: &AppHandle) -> AppIcon {
    app.try_state::<AppState>()
        .map(|state| state.env.app_icon())
        .unwrap_or_default()
}

/// Sets the Dock tile to `icon`. Call it each time the tile appears: an icon
/// set while the app is Accessory is dropped when the tile comes back.
pub fn set_dock_icon(app: &AppHandle, icon: AppIcon) {
    #[cfg(target_os = "macos")]
    {
        // An unbundled binary (`tauri dev`, `target/debug/retone`) has no
        // bundle icon to fall back on, so it always gets an explicit image.
        let use_bundle_icon = icon.is_bundle_icon() && is_bundled();
        let _ = app.run_on_main_thread(move || {
            if use_bundle_icon {
                retone_macos::reset_application_icon();
            } else {
                retone_macos::set_application_icon(dock_png(icon));
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, icon);
}

/// Applies a new choice to the tray now and to the Dock if its tile is showing.
pub fn apply(app: &AppHandle, icon: AppIcon) {
    crate::tray::set_icon(app, tray_template(icon));
    if crate::windows::is_settings_visible(app) {
        set_dock_icon(app, icon);
    }
}

/// Whether the process runs from inside a `.app` bundle, which supplies its own
/// Dock icon.
#[cfg(target_os = "macos")]
fn is_bundled() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_readable_dock_and_tray_images() {
        for icon in AppIcon::ALL {
            for png in [dock_png(icon), tray_template(icon)] {
                assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "{icon:?}");
                tauri::image::Image::from_bytes(png).expect("decodable PNG");
            }
        }
    }
}
