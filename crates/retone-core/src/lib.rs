//! Pure ReTone logic. Nothing here touches the OS, so every module is unit-tested
//! headless. OS boundaries live in `retone-macos` and the app crate.

pub mod app_icon;
pub mod channel;
pub mod dock_policy;
pub mod env_file;
pub mod hotkey;
pub mod i18n;
pub mod input;
pub mod log_level;
pub mod login_item;
pub mod navigation;
pub mod payload;
pub mod providers;
pub mod session;
pub mod tray_placement;

/// The running OS in Node's `process.platform` spelling (`darwin`, `win32`,
/// `linux`), which the platform policies and the renderer use.
pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}
