//! Synthetic ⌘C/⌘V/⌘A: CGEvents
//! with layout-aware key codes, the modifier-release wait, and the
//! `osascript` fallback.

use crate::ax;
use crate::main_thread::run_on_main;
use crate::process::{ProcessError, run_with_timeout};
use objc2_core_foundation::{CFArray, CFData, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
};
use retone_core::input::layout::{choose_layout, find_virtual_key};
use retone_core::input::ports::{KeySynth, KeySynthError, Shortcut};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

/// Where the events are posted. The session tap, as the Swift helper did
/// (`.cgSessionEventTap`).
pub const EVENT_TAP: CGEventTapLocation = CGEventTapLocation::SessionEventTap;
/// Shift, Control, Option and Command: a ⌘C posted while one is held reaches
/// the app as another shortcut.
pub const HELD_MODIFIER_MASK: CGEventFlags = CGEventFlags::MaskShift
    .union(CGEventFlags::MaskControl)
    .union(CGEventFlags::MaskAlternate)
    .union(CGEventFlags::MaskCommand);
const MODIFIER_POLL: Duration = Duration::from_millis(10);
const KEY_DOWN_HOLD: Duration = Duration::from_millis(8);
const AFTER_KEY_UP: Duration = Duration::from_millis(20);
/// How long a keyboard-layout lookup waits for the main thread.
const LAYOUT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);
const OSASCRIPT_PASTE_TIMEOUT: Duration = Duration::from_secs(3);
const OSASCRIPT_SELECT_ALL_TIMEOUT: Duration = Duration::from_millis(1200);

// UCKeyTranslate arguments, as the Swift helper passed them.
const K_UC_KEY_ACTION_DISPLAY: u16 = 3;
/// `cmdKey >> 8`.
const COMMAND_MODIFIER_STATE: u32 = 1;
/// `OptionBits(kUCKeyTranslateNoDeadKeysBit)`: the bit index (0), not a mask.
const KEY_TRANSLATE_OPTIONS: u32 = 0;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *mut c_void;
    fn TISCreateInputSourceList(
        properties: *const c_void,
        include_all_installed: u8,
    ) -> *mut c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    static kTISPropertyInputSourceID: *const c_void;
    static kTISPropertyInputSourceType: *const c_void;
    static kTISTypeKeyboardInputMode: *const c_void;
    fn LMGetKbdType() -> u8;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_length: usize,
        actual_length: *mut usize,
        unicode: *mut u16,
    ) -> i32;
}

fn cf_string_constant(ptr: *const c_void) -> &'static CFString {
    // SAFETY: the TIS property keys are immutable CFStringRef constants.
    unsafe { &*(ptr as *const CFString) }
}

/// A Text Input Source (`TISInputSourceRef`). Use on the main thread only.
#[derive(Clone)]
pub struct InputSource(CFRetained<CFType>);

impl InputSource {
    fn from_create(raw: *mut c_void) -> Option<Self> {
        // SAFETY: Copy/Create rule: we own the reference.
        NonNull::new(raw.cast::<CFType>())
            .map(|ptr| InputSource(unsafe { CFRetained::from_raw(ptr) }))
    }

    pub fn current_keyboard_layout() -> Option<Self> {
        // SAFETY: no arguments; returns a retained source or null.
        Self::from_create(unsafe { TISCopyCurrentKeyboardLayoutInputSource() })
    }

    pub fn current_ascii_capable_layout() -> Option<Self> {
        // SAFETY: as above.
        Self::from_create(unsafe { TISCopyCurrentASCIICapableKeyboardLayoutInputSource() })
    }

    fn list(filter_key: *const c_void, filter_value: &CFType) -> Vec<Self> {
        let key = cf_string_constant(filter_key);
        let filter = CFDictionary::<CFString, CFType>::from_slices(&[key], &[filter_value]);
        let filter_ptr: *const CFDictionary<CFString, CFType> = &*filter;
        // SAFETY: a valid filter dictionary; Create rule for the array.
        let raw = unsafe { TISCreateInputSourceList(filter_ptr.cast(), 1) };
        let Some(array) = NonNull::new(raw.cast::<CFArray>()) else {
            return Vec::new();
        };
        // SAFETY: we own the returned array.
        let array: CFRetained<CFArray> = unsafe { CFRetained::from_raw(array) };
        // SAFETY: TISCreateInputSourceList returns an array of input sources.
        let array: &CFArray<CFType> = unsafe { array.cast_unchecked() };
        array.iter().map(InputSource).collect()
    }

    /// An installed source by id, e.g. `com.apple.keylayout.Dvorak`.
    pub fn with_id(id: &str) -> Option<Self> {
        let id = CFString::from_str(id);
        Self::list(unsafe { kTISPropertyInputSourceID }, &id)
            .into_iter()
            .next()
    }

    /// Installed input methods' modes (some carry no layout data).
    pub fn keyboard_input_modes() -> Vec<Self> {
        let kind = cf_string_constant(unsafe { kTISTypeKeyboardInputMode });
        Self::list(unsafe { kTISPropertyInputSourceType }, kind)
    }

    fn as_ptr(&self) -> *const c_void {
        let ptr: *const CFType = &*self.0;
        ptr.cast()
    }

    /// The `'uchr'` layout data, if this source has one.
    pub fn layout_data(&self) -> Option<LayoutData> {
        // SAFETY: a live source; Get rule, so retain what we keep.
        let raw =
            unsafe { TISGetInputSourceProperty(self.as_ptr(), kTISPropertyUnicodeKeyLayoutData) };
        NonNull::new(raw.cast::<CFData>()).map(|ptr| LayoutData(unsafe { CFRetained::retain(ptr) }))
    }
}

/// A keyboard layout's `UCKeyboardLayout` bytes.
pub struct LayoutData(CFRetained<CFData>);

impl LayoutData {
    /// What `virtual_key` types with ⌘ held.
    pub fn translate(&self, virtual_key: u16) -> Option<String> {
        let mut dead_key_state = 0u32;
        let mut length = 0usize;
        let mut buffer = [0u16; 4];
        // SAFETY: the CFData holds a UCKeyboardLayout and outlives the call;
        // buffer sizes match.
        let status = unsafe {
            UCKeyTranslate(
                self.0.byte_ptr().cast(),
                virtual_key,
                K_UC_KEY_ACTION_DISPLAY,
                COMMAND_MODIFIER_STATE,
                u32::from(LMGetKbdType()),
                KEY_TRANSLATE_OPTIONS,
                &mut dead_key_state,
                buffer.len(),
                &mut length,
                buffer.as_mut_ptr(),
            )
        };
        (status == 0).then(|| String::from_utf16_lossy(&buffer[..length.min(buffer.len())]))
    }
}

/// The key that types `character` with ⌘ in `current`'s layout, or — only if
/// `current` has no layout data — in the ASCII-capable layout. Main thread only.
pub fn virtual_key_in(
    current: Option<&InputSource>,
    ascii_capable: impl FnOnce() -> Option<InputSource>,
    character: &str,
) -> Option<u16> {
    let layout = choose_layout(current.and_then(InputSource::layout_data), || {
        ascii_capable().and_then(|source| source.layout_data())
    })?;
    find_virtual_key(character, |key| layout.translate(key))
}

/// The key for `character` in the active layout. Hops to the main thread.
pub fn lookup_virtual_key(character: &'static str) -> Option<u16> {
    run_on_main(LAYOUT_LOOKUP_TIMEOUT, move || {
        virtual_key_in(
            InputSource::current_keyboard_layout().as_ref(),
            InputSource::current_ascii_capable_layout,
            character,
        )
    })
    .flatten()
}

/// Whether Shift/Control/Option/Command is held right now.
pub fn modifiers_held() -> bool {
    CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).intersects(HELD_MODIFIER_MASK)
}

fn post_command_key(virtual_key: u16) -> Result<(), KeySynthError> {
    let down = CGEvent::new_keyboard_event(None, virtual_key, true)
        .ok_or_else(|| KeySynthError::Failed("could not create the key-down event".into()))?;
    let up = CGEvent::new_keyboard_event(None, virtual_key, false)
        .ok_or_else(|| KeySynthError::Failed("could not create the key-up event".into()))?;
    CGEvent::set_flags(Some(&down), CGEventFlags::MaskCommand);
    CGEvent::set_flags(Some(&up), CGEventFlags::MaskCommand);
    CGEvent::post(EVENT_TAP, Some(&down));
    std::thread::sleep(KEY_DOWN_HOLD);
    CGEvent::post(EVENT_TAP, Some(&up));
    std::thread::sleep(AFTER_KEY_UP);
    Ok(())
}

/// The System Events command for a shortcut.
pub fn osascript_command(shortcut: Shortcut) -> &'static str {
    match shortcut {
        Shortcut::Paste => "tell application \"System Events\" to key code 9 using command down",
        Shortcut::Copy => {
            "tell application \"System Events\" to keystroke \"c\" using command down"
        }
        Shortcut::SelectAll => {
            "tell application \"System Events\" to keystroke \"a\" using command down"
        }
    }
}

/// [`KeySynth`] over CGEvent.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacKeySynth;

impl KeySynth for MacKeySynth {
    fn await_modifier_release(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while modifiers_held() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(MODIFIER_POLL);
        }
        true
    }

    fn post_shortcut(&self, shortcut: Shortcut) -> Result<(), KeySynthError> {
        // Untrusted processes' events are dropped silently, so say so instead.
        if !ax::is_trusted() {
            return Err(KeySynthError::Untrusted);
        }
        // No US-ANSI fallback: on another layout that key can run a different
        // shortcut while looking like a successful paste.
        let key =
            lookup_virtual_key(shortcut.character()).ok_or(KeySynthError::LayoutUnresolved)?;
        post_command_key(key)
    }

    fn osascript_shortcut(&self, shortcut: Shortcut) -> Result<(), KeySynthError> {
        let timeout = match shortcut {
            Shortcut::SelectAll => OSASCRIPT_SELECT_ALL_TIMEOUT,
            _ => OSASCRIPT_PASTE_TIMEOUT,
        };
        match run_with_timeout(
            "/usr/bin/osascript",
            &["-e", osascript_command(shortcut)],
            timeout,
        ) {
            Ok(output) if output.code == Some(0) => Ok(()),
            Ok(output) => {
                let stderr = output.stderr.trim();
                Err(KeySynthError::Failed(if stderr.is_empty() {
                    format!("osascript exited with code {:?}", output.code)
                } else {
                    format!("osascript exited with code {:?}: {stderr}", output.code)
                }))
            }
            Err(ProcessError::TimedOut) => Err(KeySynthError::Failed("osascript timed out".into())),
            Err(error) => Err(KeySynthError::Failed(format!("osascript {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paste_fallback_sends_key_code_nine() {
        assert_eq!(
            osascript_command(Shortcut::Paste),
            "tell application \"System Events\" to key code 9 using command down"
        );
        assert!(osascript_command(Shortcut::SelectAll).contains("keystroke \"a\""));
    }

    #[test]
    fn held_modifiers_cover_shift_control_option_command() {
        assert!(HELD_MODIFIER_MASK.contains(CGEventFlags::MaskShift));
        assert!(HELD_MODIFIER_MASK.contains(CGEventFlags::MaskCommand));
        assert!(!HELD_MODIFIER_MASK.contains(CGEventFlags::MaskSecondaryFn));
    }
}
