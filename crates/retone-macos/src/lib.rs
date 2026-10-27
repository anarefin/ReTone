//! macOS boundary for ReTone.
//!
//! - `appkit`: the picker's window level and Space behaviour, the tray item's
//!   saved menu-bar position (phase 1).
//! - `ax`, `keys`, `pasteboard`, `workspace` (phase 3): the input layer's OS
//!   side — Accessibility trust and `AXSelectedText` reads, CGEvent ⌘C/⌘V with
//!   layout-aware key codes and the modifier-release wait, NSPasteboard
//!   save/restore, the frontmost app and activation. They implement the
//!   `retone_core::input::ports` traits; [`input_ports`] wires them up.
//!
//! - `event_tap`, `fn_usage` (phase 4): the CGEventTap hotkey listener (Globe/Fn,
//!   right-side modifiers, mouse buttons 4/5) and the Globe-action preference.
//!
//! - `login_item` (phase 7): launch at login (`SMAppService.mainApp`) and
//!   login-launch detection; `panel`: the picker as a non-activating panel.

#[cfg(target_os = "macos")]
mod appkit;
#[cfg(target_os = "macos")]
pub mod ax;
#[cfg(target_os = "macos")]
pub mod event_tap;
#[cfg(target_os = "macos")]
pub mod fn_usage;
#[cfg(target_os = "macos")]
pub mod keys;
#[cfg(target_os = "macos")]
pub mod login_item;
#[cfg(target_os = "macos")]
pub mod main_thread;
#[cfg(target_os = "macos")]
pub mod panel;
#[cfg(target_os = "macos")]
pub mod pasteboard;
pub mod process;
#[cfg(target_os = "macos")]
pub mod workspace;

#[cfg(target_os = "macos")]
pub use appkit::*;

/// `NSScreenSaverWindowLevel` (1000) plus a relative level of 1: the picker sits
/// just above screen-saver level.
pub const PICKER_WINDOW_LEVEL: isize = 1001;

/// The real OS ports of the input layer.
#[cfg(target_os = "macos")]
pub fn input_ports() -> retone_core::input::InputPorts {
    use std::sync::Arc;
    retone_core::input::InputPorts {
        pasteboard: Arc::new(pasteboard::MacPasteboard),
        accessibility: Arc::new(ax::MacAccessibility),
        keys: Arc::new(keys::MacKeySynth),
        workspace: Arc::new(workspace::MacWorkspace),
        reader: Arc::new(ax::MacSelectionReader),
        clock: Arc::new(retone_core::input::ports::SystemClock),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn picker_level_sits_just_above_the_screen_saver_level() {
        assert_eq!(super::PICKER_WINDOW_LEVEL, 1000 + 1);
    }
}
