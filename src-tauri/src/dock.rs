//! The single owner of the macOS Dock icon. The icon
//! tracks the settings window, reported explicitly by the show/hide paths in
//! `windows.rs`, never derived from window events (occlusion on macOS).

use retone_core::dock_policy::{DockBackend, DockController};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

/// Maps Dock visibility onto the activation policy: Regular shows the icon and
/// the app menu, Accessory leaves only the menu-bar item.
pub struct TauriDock {
    app: AppHandle,
    // A Tauri app starts with the Regular policy, so the icon starts visible.
    visible: bool,
}

impl DockBackend for TauriDock {
    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.set(true);
    }

    fn hide(&mut self) {
        self.set(false);
    }
}

impl TauriDock {
    fn set(&mut self, visible: bool) {
        #[cfg(target_os = "macos")]
        {
            let policy = if visible {
                tauri::ActivationPolicy::Regular
            } else {
                tauri::ActivationPolicy::Accessory
            };
            if let Err(error) = self.app.set_activation_policy(policy) {
                tracing::warn!(target: "dock", "Failed to set the activation policy: {error}");
                return;
            }
        }
        self.visible = visible;
    }
}

pub struct Dock(Mutex<DockController<TauriDock>>);

/// Creates the Dock owner and hides the icon until the settings window opens.
pub fn init(app: &AppHandle) {
    let mut controller = DockController::new(
        retone_core::node_platform(),
        TauriDock {
            app: app.clone(),
            visible: true,
        },
    );
    controller.init();
    app.manage(Dock(Mutex::new(controller)));
}

pub fn set_settings_visible(app: &AppHandle, visible: bool) {
    if let Some(dock) = app.try_state::<Dock>()
        && let Ok(mut controller) = dock.0.lock()
    {
        controller.set_control_panel_visible(visible);
    }
}
