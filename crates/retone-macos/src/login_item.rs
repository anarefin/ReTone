//! Launch at login through `SMAppService.mainApp` (macOS 13+), and whether
//! this launch came from a login.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSBundle};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use retone_core::login_item::{LoginItemPort, LoginItemStatus};

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// `kAEOpenApplication`.
const OPEN_APPLICATION: u32 = four_cc(b"oapp");
/// `keyAEPropData`.
const PROP_DATA: u32 = four_cc(b"prdt");
/// `keyAELaunchedAsLogInItem`.
const LAUNCHED_AS_LOGIN_ITEM: u32 = four_cc(b"lgit");

/// Whether this process runs from an `.app` bundle. A bare binary (`tauri
/// dev`, `cargo run`) has no main-app service to register, and registering it
/// anyway would leave a stray login item.
fn in_app_bundle() -> bool {
    let path = NSBundle::mainBundle().bundlePath().to_string();
    path.ends_with(".app")
}

fn sm_app_service_available() -> bool {
    AnyClass::get(c"SMAppService").is_some()
}

fn main_app() -> Option<Retained<SMAppService>> {
    if !sm_app_service_available() || !in_app_bundle() {
        return None;
    }
    // SAFETY: the class exists (checked above); `mainAppService` has no
    // preconditions.
    Some(unsafe { SMAppService::mainAppService() })
}

/// `SMAppService.mainApp`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacLoginItem;

impl LoginItemPort for MacLoginItem {
    fn status(&self) -> LoginItemStatus {
        let Some(service) = main_app() else {
            return LoginItemStatus::Unavailable;
        };
        // SAFETY: a live service object.
        match unsafe { service.status() } {
            SMAppServiceStatus::Enabled => LoginItemStatus::Enabled,
            SMAppServiceStatus::RequiresApproval => LoginItemStatus::RequiresApproval,
            SMAppServiceStatus::NotFound => LoginItemStatus::NotFound,
            _ => LoginItemStatus::NotRegistered,
        }
    }

    fn register(&self) -> Result<(), String> {
        let service = main_app().ok_or_else(unavailable)?;
        // SAFETY: a live service object.
        unsafe { service.registerAndReturnError() }
            .map_err(|error| error.localizedDescription().to_string())
    }

    fn unregister(&self) -> Result<(), String> {
        let service = main_app().ok_or_else(unavailable)?;
        // SAFETY: a live service object.
        unsafe { service.unregisterAndReturnError() }
            .map_err(|error| error.localizedDescription().to_string())
    }
}

fn unavailable() -> String {
    retone_core::login_item::UNAVAILABLE_MESSAGE.to_string()
}

/// Whether the event being handled is the launch event of a login item.
/// Meaningful only while `applicationDidFinishLaunching` runs (Tauri's
/// `setup`), when the current Apple Event is still the one that launched us.
pub fn launched_as_login_item() -> bool {
    let Some(event) = NSAppleEventManager::sharedAppleEventManager().currentAppleEvent() else {
        return false;
    };
    // `eventID` / `paramDescriptorForKeyword:` sit behind objc2's CoreServices
    // feature; plain messages avoid pulling that framework's bindings in.
    // SAFETY: both selectors exist on NSAppleEventDescriptor with these
    // signatures (`AEEventID` and `AEKeyword` are `FourCharCode`, a u32).
    let event_id: u32 = unsafe { msg_send![&*event, eventID] };
    if event_id != OPEN_APPLICATION {
        return false;
    }
    let prop: Option<Retained<NSAppleEventDescriptor>> =
        unsafe { msg_send![&*event, paramDescriptorForKeyword: PROP_DATA] };
    prop.is_some_and(|prop| prop.enumCodeValue() == LAUNCHED_AS_LOGIN_ITEM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_character_codes_match_the_carbon_constants() {
        assert_eq!(OPEN_APPLICATION, 0x6F61_7070);
        assert_eq!(PROP_DATA, 0x7072_6474);
        assert_eq!(LAUNCHED_AS_LOGIN_ITEM, 0x6C67_6974);
    }

    // A test binary is not an app bundle: nothing is read or registered.
    #[test]
    fn a_bare_binary_has_no_login_item() {
        assert!(!in_app_bundle());
        assert_eq!(MacLoginItem.status(), LoginItemStatus::Unavailable);
        assert!(MacLoginItem.register().is_err());
    }

    #[test]
    fn a_test_process_was_not_launched_at_login() {
        assert!(!launched_as_login_item());
    }
}
