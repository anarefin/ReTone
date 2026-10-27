//! macOS Dock icon policy and its single owner.
//!
//! The Dock icon follows the settings window: it appears when the window opens
//! and goes away when it is closed to the tray, so ReTone lives in the menu
//! bar like other background utilities.
//!
//! Callers report the window's state explicitly. Window show/hide events look
//! like the obvious source, but on macOS they are occlusion events (covered,
//! minimized, other Space), and deriving from them makes the icon flicker.

/// Whether the Dock icon should be visible right now. `None` off macOS, where
/// there is no Dock to act on.
pub fn resolve_dock_visibility(platform: &str, control_panel_visible: bool) -> Option<bool> {
    if platform != "darwin" {
        return None;
    }
    Some(control_panel_visible)
}

/// The OS side of the Dock: the app crate maps it onto the activation policy.
pub trait DockBackend {
    fn is_visible(&self) -> bool;
    fn show(&mut self);
    fn hide(&mut self);
}

/// Tracks the settings window's reported visibility and applies it to the Dock,
/// acting only on a change (showing an already-visible icon hands activation to
/// the Dock on an active app).
pub struct DockController<B: DockBackend> {
    platform: &'static str,
    control_panel_visible: bool,
    backend: B,
}

impl<B: DockBackend> DockController<B> {
    pub fn new(platform: &'static str, backend: B) -> Self {
        Self {
            platform,
            control_panel_visible: false,
            backend,
        }
    }

    /// Called once at startup, before any window shows: hides the icon until
    /// the settings window opens, so tray-only launches never show one.
    pub fn init(&mut self) {
        self.control_panel_visible = false;
        self.apply();
    }

    /// Reported by every path that surfaces or hides the settings window.
    pub fn set_control_panel_visible(&mut self, visible: bool) {
        self.control_panel_visible = visible;
        self.apply();
    }

    pub fn control_panel_visible(&self) -> bool {
        self.control_panel_visible
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn apply(&mut self) {
        let Some(visible) = resolve_dock_visibility(self.platform, self.control_panel_visible)
        else {
            return;
        };
        if visible == self.backend.is_visible() {
            return;
        }
        if visible {
            self.backend.show();
        } else {
            self.backend.hide();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dock_icon_follows_the_control_panel() {
        assert_eq!(resolve_dock_visibility("darwin", true), Some(true));
        assert_eq!(resolve_dock_visibility("darwin", false), Some(false));
    }

    #[test]
    fn hiding_another_panel_cannot_resurrect_the_dock_icon() {
        assert_eq!(resolve_dock_visibility("darwin", false), Some(false));
    }

    #[test]
    fn there_is_no_dock_to_act_on_outside_macos() {
        for platform in ["win32", "linux"] {
            assert_eq!(resolve_dock_visibility(platform, true), None);
        }
    }

    // Mirrors the real Dock: the icon
    // starts visible, and is_visible() reports what show()/hide() did.
    struct FakeDock {
        visible: bool,
        calls: Vec<&'static str>,
    }

    impl FakeDock {
        fn new() -> Self {
            Self {
                visible: true,
                calls: Vec::new(),
            }
        }
    }

    impl DockBackend for FakeDock {
        fn is_visible(&self) -> bool {
            self.visible
        }
        fn show(&mut self) {
            self.calls.push("show");
            self.visible = true;
        }
        fn hide(&mut self) {
            self.calls.push("hide");
            self.visible = false;
        }
    }

    #[test]
    fn the_dock_icon_is_only_shown_or_hidden_when_the_control_panel_changes_state() {
        let mut dock = DockController::new("darwin", FakeDock::new());
        dock.init();
        dock.set_control_panel_visible(true);
        // A second open while the panel is up must not show the icon again.
        dock.set_control_panel_visible(true);
        dock.set_control_panel_visible(false);
        dock.set_control_panel_visible(true);

        assert_eq!(dock.backend().calls, vec!["hide", "show", "hide", "show"]);
    }

    #[test]
    fn the_dock_icon_is_left_alone_outside_macos() {
        for platform in ["win32", "linux"] {
            let mut dock = DockController::new(platform, FakeDock::new());
            dock.init();
            dock.set_control_panel_visible(true);
            dock.set_control_panel_visible(false);
            assert!(dock.backend().calls.is_empty());
        }
    }
}
