use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage, NSStatusItem, NSWindow, NSWindowCollectionBehavior};
use objc2_foundation::{NSData, NSDictionary, NSNumber, NSString, NSUserDefaults};
use std::ffi::c_void;

use crate::PICKER_WINDOW_LEVEL;

/// Puts the picker above every app (including full-screen ones) on every Space,
/// without a shadow.
///
/// # Safety
/// `ns_window` must be a live `NSWindow` pointer (Tauri's `ns_window()`), and
/// this must run on the main thread.
pub unsafe fn configure_overlay_window(ns_window: *mut c_void) {
    if ns_window.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live NSWindow on the main thread.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    window.setLevel(PICKER_WINDOW_LEVEL);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    window.setHasShadow(false);
}

/// Registers an integer default in the app's `NSUserDefaults` registration
/// domain. A registered default only fills in for a missing value.
pub fn register_integer_default(key: &str, value: i64) {
    let key = NSString::from_str(key);
    let number = NSNumber::new_i64(value);
    let object: &AnyObject = &number;
    let dict = NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[object]);
    let defaults = NSUserDefaults::standardUserDefaults();
    // SAFETY: the dictionary holds an NSString key and an NSNumber value, which
    // is what registerDefaults: expects.
    unsafe { defaults.registerDefaults(&dict) };
}

/// Sets the status item's autosave name, under which macOS keeps its position.
pub fn set_status_item_autosave_name(item: &NSStatusItem, name: &str) {
    let name = NSString::from_str(name);
    item.setAutosaveName(Some(&name));
}

/// Shows the window above others without activating the app or making it key:
/// the target app keeps focus.
///
/// # Safety
/// `ns_window` must be a live `NSWindow` pointer, and this must run on the
/// main thread.
pub unsafe fn order_front_regardless(ns_window: *mut c_void) {
    if ns_window.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live NSWindow on the main thread.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    window.orderFrontRegardless();
}

/// The mouse cursor in global display coordinates (points, top-left origin of
/// the main display). Any thread.
pub fn cursor_location() -> Option<(f64, f64)> {
    use objc2_core_graphics::CGEvent;
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    Some((point.x, point.y))
}

/// Sets the Dock icon from PNG data: an icon picked in Settings, or any icon
/// outside a `.app` bundle, where macOS shows the generic executable icon.
/// Does nothing off the main thread or when the data isn't a readable image.
pub fn set_application_icon(png: &[u8]) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(png);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: on the main thread with a valid NSImage.
    unsafe { app.setApplicationIconImage(Some(&image)) };
}

/// Puts the bundle's own icon back in the Dock after [`set_application_icon`].
/// Does nothing off the main thread.
pub fn reset_application_icon() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: on the main thread; nil restores the bundle icon.
    unsafe { app.setApplicationIconImage(None) };
}
