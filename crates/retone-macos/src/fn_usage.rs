//! The macOS Globe-key action preference (`com.apple.HIToolbox
//! AppleFnUsageType`), through the private `TISGetFnUsageType` /
//! `TISUpdateFnUsageType` that System Settings itself calls: they persist the
//! value and broadcast the change so it applies live. Writing the preference
//! directly is ignored until the next login.

use crate::main_thread::run_on_main;
use objc2_core_foundation::{
    CFPreferencesCopyValue, CFPreferencesSetValue, CFPreferencesSynchronize, CFString,
    kCFPreferencesAnyHost, kCFPreferencesCurrentUser,
};
use retone_core::hotkey::globe_preference::{FN_USAGE_DO_NOTHING, FnUsagePort};
use std::ffi::{CStr, c_void};
use std::sync::OnceLock;
use std::time::Duration;

type GetUsageType = unsafe extern "C" fn() -> i32;
type UpdateUsageType = unsafe extern "C" fn(i32);

/// Text Input Source calls hop to the main thread (some TIS calls assert it).
const MAIN_THREAD_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
struct EntryPoints {
    get: GetUsageType,
    update: UpdateUsageType,
}

fn entry_points() -> Option<EntryPoints> {
    static ENTRY_POINTS: OnceLock<Option<EntryPoints>> = OnceLock::new();
    *ENTRY_POINTS.get_or_init(|| {
        let path = c"/System/Library/Frameworks/Carbon.framework/Carbon";
        let symbol = |handle: *mut c_void, name: &CStr| {
            // SAFETY: a valid handle and NUL-terminated name.
            let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
            (!ptr.is_null()).then_some(ptr)
        };
        // SAFETY: dlopen of a system framework; the handle is never closed.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) };
        if handle.is_null() {
            return None;
        }
        let get = symbol(handle, c"TISGetFnUsageType")?;
        let update = symbol(handle, c"TISUpdateFnUsageType")?;
        // SAFETY: the symbols have these C signatures (as the Swift helper used them).
        Some(unsafe {
            EntryPoints {
                get: std::mem::transmute::<*mut c_void, GetUsageType>(get),
                update: std::mem::transmute::<*mut c_void, UpdateUsageType>(update),
            }
        })
    })
}

fn domain() -> objc2_core_foundation::CFRetained<CFString> {
    CFString::from_static_str("com.apple.HIToolbox")
}

fn key() -> objc2_core_foundation::CFRetained<CFString> {
    CFString::from_static_str("AppleFnUsageType")
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MacFnUsage;

impl FnUsagePort for MacFnUsage {
    fn available(&self) -> bool {
        entry_points().is_some()
    }

    fn get(&self) -> i32 {
        let Some(entry) = entry_points() else {
            return FN_USAGE_DO_NOTHING;
        };
        // SAFETY: a no-argument getter.
        run_on_main(MAIN_THREAD_TIMEOUT, move || unsafe { (entry.get)() }).unwrap_or_else(|| {
            tracing::warn!(target: "hotkey", "Reading the Globe action timed out");
            FN_USAGE_DO_NOTHING
        })
    }

    fn update(&self, value: i32) {
        let Some(entry) = entry_points() else {
            return;
        };
        // SAFETY: takes the new usage type.
        if run_on_main(MAIN_THREAD_TIMEOUT, move || unsafe {
            (entry.update)(value)
        })
        .is_none()
        {
            tracing::warn!(target: "hotkey", value, "Updating the Globe action timed out");
        }
    }

    // CFPreferencesCopyAppValue caches foreign domains for the life of the
    // process and would miss changes made after launch.
    fn raw_value_exists(&self) -> bool {
        let (domain, key) = (domain(), key());
        // SAFETY: immutable CoreFoundation constants.
        let (user, host) = unsafe { (kCFPreferencesCurrentUser, kCFPreferencesAnyHost) };
        CFPreferencesSynchronize(&domain, user, host);
        CFPreferencesCopyValue(&key, &domain, user, host).is_some()
    }

    fn clear_raw_value(&self) {
        let (domain, key) = (domain(), key());
        // SAFETY: immutable CoreFoundation constants; a null value removes the key.
        unsafe {
            let (user, host) = (kCFPreferencesCurrentUser, kCFPreferencesAnyHost);
            CFPreferencesSetValue(&key, None, &domain, user, host);
            CFPreferencesSynchronize(&domain, user, host);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_private_entry_points_resolve() {
        assert!(MacFnUsage.available());
    }

    #[test]
    fn reading_the_raw_preference_does_not_change_it() {
        let before = MacFnUsage.raw_value_exists();
        assert_eq!(MacFnUsage.raw_value_exists(), before);
    }
}
