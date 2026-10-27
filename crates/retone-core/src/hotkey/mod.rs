//! Hotkeys: accelerators, hotkey lists, named slots, the autorepeat gate and
//! the Globe/native listener logic. The global-shortcut service and the event tap
//! are traits; the app crate and `retone-macos` implement them.

pub mod accelerator;
pub mod globe_preference;
pub mod kinds;
pub mod list;
pub mod manager;
pub mod native;
pub mod repeat_gate;

pub use manager::{
    FALLBACK_HOTKEYS, FailureNotice, HotkeyManager, PressHandler, RegisterError, RegisterOutcome,
    ShortcutBackend, SlotCallback, TONE_REWRITE_SLOT, Translate,
};
