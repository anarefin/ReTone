//! The tone picker as a non-activating panel.
//!
//! The picker must take the keyboard (arrows, digits, Enter, Esc) while the
//! app the selection came from stays the active app. An ordinary window can
//! only get key events once its app is active, and since macOS 14 activation
//! is cooperative: `activateIgnoringOtherApps:` may be refused when another
//! app is in use, which left the picker on screen without the keyboard. A
//! panel with `NSWindowStyleMaskNonactivatingPanel` becomes key without
//! activating ReTone, which is how Spotlight behaves.
//!
//! Tauri creates the window as tao's `TaoWindow` (an `NSWindow` subclass), so
//! the instance's class is swapped for an `NSPanel` subclass after creation.
//! `NSPanel` adds no instance variables to `NSWindow`, so the object's layout
//! (and tao's own ivar after it) is unchanged.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class};
use objc2_app_kit::{NSApplication, NSPanel, NSWindow, NSWindowStyleMask};
use std::ffi::c_void;

define_class!(
    #[unsafe(super(NSPanel, NSWindow, objc2_app_kit::NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RetonePickerPanel"]
    struct PickerPanel;

    impl PickerPanel {
        // A borderless window refuses key status by default.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        // Never the main window: the settings window keeps that role.
        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }
);

/// Whether the picker panel class has the same instance layout as `NSWindow`
/// (what makes swapping the class of a live window sound).
pub fn panel_layout_matches_window() -> bool {
    let window = NSWindow::class().instance_size();
    let panel = NSPanel::class().instance_size();
    let picker = PickerPanel::class().instance_size();
    panel == window && picker == panel
}

/// Turns the picker window into a non-activating panel that can become key.
/// Returns false (leaving the window as it was) if the layouts differ.
///
/// # Safety
/// `ns_window` must be a live `NSWindow` pointer whose class is `NSWindow` or
/// a subclass that adds only trailing instance variables (tao's `TaoWindow`),
/// and this must run on the main thread.
pub unsafe fn make_nonactivating_panel(ns_window: *mut c_void) -> bool {
    if ns_window.is_null() || !panel_layout_matches_window() {
        return false;
    }
    let object = ns_window as *mut AnyObject;
    // SAFETY: the caller guarantees a live object.
    let current: &AnyClass = unsafe { (*object).class() };
    let class: &AnyClass = PickerPanel::class();
    // The panel must fit in the object as allocated, and the object must be a
    // window: tao's `TaoWindow` is NSWindow plus its own trailing ivar, which
    // the swap leaves where it is.
    if !inherits(current, NSWindow::class()) || class.instance_size() > current.instance_size() {
        return false;
    }
    // `AnyObject::set_class` debug-asserts equal sizes; the panel is smaller
    // than `TaoWindow` (which only adds a trailing ivar), which is sound.
    // SAFETY: a live window; PickerPanel adds no ivars, NSPanel's layout equals
    // NSWindow's (checked above) and the object is at least as large.
    unsafe { objc2::ffi::object_setClass(object, class) };
    // SAFETY: the object is now an NSPanel, on the main thread.
    let panel: &NSPanel = unsafe { &*(ns_window as *const NSPanel) };
    panel.setStyleMask(panel.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    // A panel hides when its app deactivates by default; ReTone is not the
    // active app while the picker is up.
    panel.setHidesOnDeactivate(false);
    panel.setBecomesKeyOnlyIfNeeded(false);
    true
}

fn inherits(class: &AnyClass, ancestor: &AnyClass) -> bool {
    let mut next = Some(class);
    while let Some(current) = next {
        if current == ancestor {
            return true;
        }
        next = current.superclass();
    }
    false
}

/// Shows the window and makes it key without activating the app.
///
/// # Safety
/// `ns_window` must be a live `NSWindow` pointer, on the main thread.
pub unsafe fn make_key_and_order_front(ns_window: *mut c_void) {
    if ns_window.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a live NSWindow on the main thread.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    window.makeKeyAndOrderFront(None);
}

/// Where the keyboard is, for the focus diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusState {
    pub visible: bool,
    pub key: bool,
    pub panel: bool,
    pub app_active: bool,
}

fn is_panel(window: &NSWindow) -> bool {
    inherits((**window).class(), NSPanel::class())
}

/// # Safety
/// `ns_window` must be a live `NSWindow` pointer, on the main thread.
pub unsafe fn focus_state(ns_window: *mut c_void) -> Option<FocusState> {
    if ns_window.is_null() {
        return None;
    }
    let mtm = MainThreadMarker::new()?;
    // SAFETY: the caller guarantees a live NSWindow on the main thread.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    let app: Retained<NSApplication> = NSApplication::sharedApplication(mtm);
    Some(FocusState {
        visible: window.isVisible(),
        key: window.isKeyWindow(),
        panel: is_panel(window),
        app_active: app.isActive(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picker_panel_has_the_window_layout() {
        assert!(panel_layout_matches_window());
    }

    #[test]
    fn the_picker_panel_is_a_panel() {
        assert!(PickerPanel::class().superclass() == Some(NSPanel::class()));
    }
}
