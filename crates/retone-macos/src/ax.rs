//! Accessibility: the trust check and prompt, and the focused element's
//! selected text, with a System Events fallback.
//!
//! Reads only. Never write AX attributes on target apps: flipping
//! `AXEnhancedUserInterface` puts a Chromium app into screen-reader mode for
//! its lifetime and blurs its focused editor.

use crate::process::run_with_timeout;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use retone_core::input::ports::{Accessibility, AxSelection, NativeRead, SelectionReader};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

type AXError = i32;
const AX_SUCCESS: AXError = 0;
const AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;
/// What a dormant (Chromium/Electron) tree answers.
const AX_ERROR_NO_VALUE: AXError = -25212;

/// Focused-element retries, as the Swift helper: 5 attempts 300 ms apart.
const FOCUSED_ELEMENT_ATTEMPTS: u32 = 5;
const FOCUSED_ELEMENT_RETRY_DELAY: Duration = Duration::from_millis(300);

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: *const c_void;
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: *const c_void, seconds: f32) -> AXError;
}

/// Whether this process is trusted for Accessibility. No prompt.
pub fn is_trusted() -> bool {
    // SAFETY: no arguments; reads the process's TCC state.
    unsafe { AXIsProcessTrusted() != 0 }
}

/// `AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: true})`: shows
/// the system prompt when the process is not trusted.
pub fn prompt_for_trust() -> bool {
    // SAFETY: kAXTrustedCheckOptionPrompt is a constant CFStringRef exported by
    // ApplicationServices.
    let key: &CFString = unsafe { &*(kAXTrustedCheckOptionPrompt as *const CFString) };
    let value: &CFBoolean = CFBoolean::new(true);
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[value]);
    let options_ptr: *const CFDictionary<CFString, CFBoolean> = &*options;
    // SAFETY: a valid CFDictionary with the documented key.
    unsafe { AXIsProcessTrustedWithOptions(options_ptr.cast()) != 0 }
}

/// [`Accessibility`] over the system calls.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacAccessibility;

impl Accessibility for MacAccessibility {
    fn is_trusted(&self) -> bool {
        is_trusted()
    }

    fn prompt(&self) -> bool {
        prompt_for_trust()
    }
}

/// An `AXUIElementRef`, released on drop.
struct Element(CFRetained<CFType>);

impl Element {
    fn application(pid: i32) -> Option<Self> {
        // SAFETY: Create rule: the returned reference is owned by us.
        let raw = unsafe { AXUIElementCreateApplication(pid) };
        NonNull::new(raw.cast::<CFType>()).map(|ptr| Element(unsafe { CFRetained::from_raw(ptr) }))
    }

    fn as_ptr(&self) -> *const c_void {
        let ptr: *const CFType = &*self.0;
        ptr.cast()
    }

    fn set_messaging_timeout(&self, timeout: Duration) {
        // SAFETY: a live element; the call only sets a per-element timeout.
        unsafe { AXUIElementSetMessagingTimeout(self.as_ptr(), timeout.as_secs_f32()) };
    }

    fn attribute(&self, name: &str) -> Result<CFRetained<CFType>, AXError> {
        let name = CFString::from_str(name);
        let name_ptr: *const CFString = &*name;
        let mut value: *mut c_void = std::ptr::null_mut();
        // SAFETY: a live element and attribute name; Copy rule for `value`.
        let error =
            unsafe { AXUIElementCopyAttributeValue(self.as_ptr(), name_ptr.cast(), &mut value) };
        if error != AX_SUCCESS {
            return Err(error);
        }
        NonNull::new(value.cast::<CFType>())
            .map(|ptr| unsafe { CFRetained::from_raw(ptr) })
            .ok_or(AX_ERROR_NO_VALUE)
    }

    fn string_attribute(&self, name: &str) -> Result<Option<String>, AXError> {
        self.attribute(name)
            .map(|value| value.downcast::<CFString>().ok().map(|s| s.to_string()))
    }

    fn element_attribute(&self, name: &str) -> Result<Element, AXError> {
        self.attribute(name).map(Element)
    }

    fn role(&self) -> Option<String> {
        self.string_attribute("AXRole").ok().flatten()
    }
}

/// One in-process read of the focused element's `AXSelectedText`. Blocks up to
/// ~1.2 s while a focused element fails to resolve; stops early once
/// `cancelled` is set.
pub fn read_selected_text_native(
    pid: i32,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> NativeRead {
    let Some(app) = Element::application(pid) else {
        return NativeRead::Failed {
            persistent_no_value: false,
        };
    };
    // Bounds every message below, so a hung app can't hold this thread long
    // after the caller gave up.
    app.set_messaging_timeout(timeout);

    let mut attempt_errors = Vec::new();
    let mut focused = None;
    for attempt in 1..=FOCUSED_ELEMENT_ATTEMPTS {
        if cancelled.load(Ordering::SeqCst) {
            return NativeRead::TimedOut;
        }
        match app.element_attribute("AXFocusedUIElement") {
            Ok(element) => {
                focused = Some((element, attempt > 1));
                break;
            }
            Err(error) => attempt_errors.push(error),
        }
        if attempt < FOCUSED_ELEMENT_ATTEMPTS {
            std::thread::sleep(FOCUSED_ELEMENT_RETRY_DELAY);
        }
    }

    let Some((element, resolved_late)) = focused else {
        // Only a run where every attempt failed with kAXErrorNoValue marks the
        // app's tree as dormant; one transient -25212 doesn't.
        return NativeRead::Failed {
            persistent_no_value: !attempt_errors.is_empty()
                && attempt_errors.iter().all(|&e| e == AX_ERROR_NO_VALUE),
        };
    };

    match element.attribute("AXSelectedText") {
        Ok(value) => match value.downcast::<CFString>() {
            Ok(text) => {
                let text = text.to_string();
                NativeRead::Read(if text.is_empty() {
                    AxSelection::None
                } else {
                    AxSelection::Selected(text)
                })
            }
            Err(_) => NativeRead::Failed {
                persistent_no_value: false,
            },
        },
        // A dormant Chromium tree reports the bare window as the focused
        // element; its unsupported AXSelectedText is not an empty selection.
        Err(AX_ERROR_NO_VALUE | AX_ERROR_ATTRIBUTE_UNSUPPORTED) => {
            NativeRead::Read(if element.role().as_deref() == Some("AXWindow") {
                AxSelection::Unknown
            } else {
                AxSelection::None
            })
        }
        Err(error) => NativeRead::Failed {
            persistent_no_value: !resolved_late && error == AX_ERROR_NO_VALUE,
        },
    }
}

/// The System Events script: an empty selection is distinguished from an
/// inaccessible target, so selection editing can fail closed.
pub fn applescript_selected_text_script(pid: i32) -> String {
    format!(
        "tell application \"System Events\"\n\
         \ttry\n\
         \t\tset targetProc to first application process whose unix id is {pid}\n\
         \t\tset focAttr to value of attribute \"AXFocusedUIElement\" of targetProc\n\
         \t\tif focAttr is missing value then return \"UNAVAILABLE:\"\n\
         \t\tset selectedValue to value of attribute \"AXSelectedText\" of focAttr\n\
         \t\tif selectedValue is missing value or selectedValue is \"\" then return \"NONE:\"\n\
         \t\treturn \"SELECTED:\" & selectedValue\n\
         \ton error\n\
         \t\treturn \"UNAVAILABLE:\"\n\
         \tend try\n\
         end tell"
    )
}

/// Parses the script's output.
pub fn parse_applescript_output(stdout: &str) -> AxSelection {
    let output = stdout.strip_suffix('\n').unwrap_or(stdout);
    if output == "NONE:" {
        AxSelection::None
    } else if let Some(text) = output.strip_prefix("SELECTED:") {
        AxSelection::Selected(text.to_string())
    } else {
        AxSelection::Unavailable
    }
}

/// [`SelectionReader`]: the native read on a worker thread with a deadline,
/// and the `osascript` fallback.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacSelectionReader;

impl SelectionReader for MacSelectionReader {
    fn read_native(&self, pid: i32, timeout: Duration) -> NativeRead {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let worker_cancelled = cancelled.clone();
        let spawned = std::thread::Builder::new()
            .name("ax-selected-text".into())
            .spawn(move || {
                let _ = sender.send(read_selected_text_native(pid, timeout, &worker_cancelled));
            });
        if spawned.is_err() {
            return NativeRead::Failed {
                persistent_no_value: false,
            };
        }
        let started = Instant::now();
        let result = receiver.recv_timeout(timeout);
        match result {
            Ok(read) => read,
            Err(_) => {
                cancelled.store(true, Ordering::SeqCst);
                tracing::debug!(
                    target: "clipboard",
                    pid,
                    waited_ms = started.elapsed().as_millis() as u64,
                    "Native selected-text read timed out"
                );
                NativeRead::TimedOut
            }
        }
    }

    fn read_applescript(&self, pid: i32, timeout: Duration) -> AxSelection {
        let script = applescript_selected_text_script(pid);
        match run_with_timeout("/usr/bin/osascript", &["-e", &script], timeout) {
            Ok(output) if output.code == Some(0) => parse_applescript_output(&output.stdout),
            _ => AxSelection::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_applescript_states() {
        assert_eq!(parse_applescript_output("NONE:\n"), AxSelection::None);
        assert_eq!(
            parse_applescript_output("SELECTED:hello world\n"),
            AxSelection::Selected("hello world".into())
        );
        assert_eq!(
            parse_applescript_output("SELECTED:line one\nline two\n"),
            AxSelection::Selected("line one\nline two".into())
        );
        assert_eq!(
            parse_applescript_output("UNAVAILABLE:\n"),
            AxSelection::Unavailable
        );
        assert_eq!(parse_applescript_output(""), AxSelection::Unavailable);
    }

    #[test]
    fn the_script_targets_the_pid() {
        let script = applescript_selected_text_script(4242);
        assert!(script.contains("whose unix id is 4242"));
        assert!(script.starts_with("tell application \"System Events\"\n\ttry\n"));
    }

    #[test]
    fn a_missing_process_is_not_a_dormant_tree() {
        // No process has pid i32::MAX; AX answers an error that is not -25212
        // (or untrusted -25211), never the dormant signature.
        let read =
            read_selected_text_native(i32::MAX, Duration::from_millis(200), &AtomicBool::new(true));
        assert!(matches!(
            read,
            NativeRead::TimedOut
                | NativeRead::Failed {
                    persistent_no_value: false
                }
        ));
    }
}
