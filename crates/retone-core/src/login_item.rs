//! Launch at login.
//!
//! macOS 13+ routes login items through `SMAppService`, which can register an
//! item and still leave it awaiting approval in System Settings. The state the
//! settings window shows is therefore `{ enabled, requiresApproval }`, read
//! back after every change rather than assumed. A login launch comes up in the
//! tray; on macOS the only signal for it is the launch Apple Event
//! (`wasOpenedAtLogin`), never a flag on argv.

use serde::Serialize;

/// `SMAppService.status`, plus `Unavailable` where there is no `SMAppService`
/// to ask (macOS 12, or a binary that is not inside an app bundle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemStatus {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
    Unavailable,
}

impl LoginItemStatus {
    /// The status as a kebab-case string, for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            LoginItemStatus::NotRegistered => "not-registered",
            LoginItemStatus::Enabled => "enabled",
            LoginItemStatus::RequiresApproval => "requires-approval",
            LoginItemStatus::NotFound => "not-found",
            LoginItemStatus::Unavailable => "unavailable",
        }
    }
}

/// What `get_auto_start_enabled` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoStartState {
    pub enabled: bool,
    pub requires_approval: bool,
}

/// An item awaiting approval reads as off (it will not launch), with the
/// reason surfaced so the toggle is not one that silently will not stick.
pub fn resolve_auto_start_state(status: LoginItemStatus) -> AutoStartState {
    AutoStartState {
        enabled: status == LoginItemStatus::Enabled,
        requires_approval: status == LoginItemStatus::RequiresApproval,
    }
}

/// Whether the session, not the user, started this process. On macOS argv
/// carries no such flag (`openAsHidden` is a no-op on 13+), so only the launch
/// event counts.
pub fn was_launched_hidden(_argv: &[String], opened_at_login: bool) -> bool {
    opened_at_login
}

/// The OS side: `SMAppService.mainApp`.
pub trait LoginItemPort {
    fn status(&self) -> LoginItemStatus;
    fn register(&self) -> Result<(), String>;
    fn unregister(&self) -> Result<(), String>;
}

/// Reads the login item's state; a port that cannot answer reads as off.
pub fn auto_start_state(port: &impl LoginItemPort) -> AutoStartState {
    resolve_auto_start_state(port.status())
}

/// Registers or unregisters the login item. A call that fails but leaves the
/// item where it was asked to go counts as done: registering an item the user
/// switched off in System Settings fails yet leaves it awaiting approval (which
/// the read-back then shows), and unregistering an item that is not there
/// fails with "not found".
pub fn set_auto_start_enabled(port: &impl LoginItemPort, enabled: bool) -> Result<(), String> {
    if port.status() == LoginItemStatus::Unavailable {
        return Err(UNAVAILABLE_MESSAGE.to_string());
    }
    let result = if enabled {
        port.register()
    } else {
        port.unregister()
    };
    let Err(error) = result else {
        return Ok(());
    };
    let after = port.status();
    let settled = if enabled {
        matches!(
            after,
            LoginItemStatus::Enabled | LoginItemStatus::RequiresApproval
        )
    } else {
        matches!(
            after,
            LoginItemStatus::NotRegistered | LoginItemStatus::NotFound
        )
    };
    if settled { Ok(()) } else { Err(error) }
}

/// Why launch at login cannot be changed when there is no `SMAppService`.
pub const UNAVAILABLE_MESSAGE: &str =
    "Launch at login needs macOS 13 or later and the installed app";

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    // On macOS 13+ the item counts as on only when `status == "enabled"`, so
    // an item awaiting approval necessarily reads as off.
    #[test]
    fn an_item_awaiting_approval_reads_as_off_with_the_reason_surfaced() {
        assert_eq!(
            resolve_auto_start_state(LoginItemStatus::RequiresApproval),
            AutoStartState {
                enabled: false,
                requires_approval: true
            }
        );
    }

    #[test]
    fn macos_reports_an_approved_item_without_prompting_for_approval() {
        assert_eq!(
            resolve_auto_start_state(LoginItemStatus::Enabled),
            AutoStartState {
                enabled: true,
                requires_approval: false
            }
        );
    }

    #[test]
    fn a_disabled_macos_item_is_neither_enabled_nor_awaiting_approval() {
        for status in [
            LoginItemStatus::NotRegistered,
            LoginItemStatus::NotFound,
            LoginItemStatus::Unavailable,
        ] {
            assert_eq!(
                resolve_auto_start_state(status),
                AutoStartState::default(),
                "{status:?}"
            );
        }
    }

    // openAsHidden and wasOpenedAsHidden are no-ops on macOS 13+, so
    // wasOpenedAtLogin is the only signal left that the session started us.
    #[test]
    fn macos_detects_a_login_launch_from_was_opened_at_login_not_from_argv() {
        let plain = vec!["ReTone".to_string()];
        let flagged = vec!["ReTone".to_string(), "--hidden".to_string()];
        assert!(was_launched_hidden(&plain, true));
        assert!(!was_launched_hidden(&flagged, false));
    }

    #[test]
    fn the_state_serializes_as_the_renderer_expects() {
        let json = serde_json::to_value(AutoStartState {
            enabled: true,
            requires_approval: false,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "enabled": true, "requiresApproval": false })
        );
    }

    struct FakeItem {
        status: Cell<LoginItemStatus>,
        /// Status after a register / unregister, and the call's result.
        on_register: (LoginItemStatus, Result<(), String>),
        on_unregister: (LoginItemStatus, Result<(), String>),
        calls: RefCell<Vec<&'static str>>,
    }

    impl FakeItem {
        fn new(status: LoginItemStatus) -> Self {
            FakeItem {
                status: Cell::new(status),
                on_register: (LoginItemStatus::Enabled, Ok(())),
                on_unregister: (LoginItemStatus::NotRegistered, Ok(())),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl LoginItemPort for FakeItem {
        fn status(&self) -> LoginItemStatus {
            self.status.get()
        }
        fn register(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("register");
            self.status.set(self.on_register.0);
            self.on_register.1.clone()
        }
        fn unregister(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("unregister");
            self.status.set(self.on_unregister.0);
            self.on_unregister.1.clone()
        }
    }

    #[test]
    fn enabling_registers_and_reads_back_as_enabled() {
        let item = FakeItem::new(LoginItemStatus::NotRegistered);
        assert_eq!(set_auto_start_enabled(&item, true), Ok(()));
        assert_eq!(*item.calls.borrow(), ["register"]);
        assert!(auto_start_state(&item).enabled);
    }

    #[test]
    fn disabling_unregisters() {
        let item = FakeItem::new(LoginItemStatus::Enabled);
        assert_eq!(set_auto_start_enabled(&item, false), Ok(()));
        assert_eq!(*item.calls.borrow(), ["unregister"]);
        assert_eq!(auto_start_state(&item), AutoStartState::default());
    }

    // The user switched the item off in System Settings: register fails, the
    // item waits for approval, and the settings window says so.
    #[test]
    fn a_register_that_leaves_the_item_awaiting_approval_succeeds() {
        let mut item = FakeItem::new(LoginItemStatus::NotRegistered);
        item.on_register = (
            LoginItemStatus::RequiresApproval,
            Err("Operation not permitted".into()),
        );
        assert_eq!(set_auto_start_enabled(&item, true), Ok(()));
        assert!(auto_start_state(&item).requires_approval);
    }

    #[test]
    fn a_register_that_leaves_nothing_registered_fails() {
        let mut item = FakeItem::new(LoginItemStatus::NotRegistered);
        item.on_register = (LoginItemStatus::NotRegistered, Err("Invalid".into()));
        assert_eq!(set_auto_start_enabled(&item, true), Err("Invalid".into()));
    }

    #[test]
    fn unregistering_an_item_that_is_not_there_succeeds() {
        let mut item = FakeItem::new(LoginItemStatus::NotRegistered);
        item.on_unregister = (LoginItemStatus::NotRegistered, Err("not found".into()));
        assert_eq!(set_auto_start_enabled(&item, false), Ok(()));
    }

    #[test]
    fn an_unregister_that_leaves_the_item_enabled_fails() {
        let mut item = FakeItem::new(LoginItemStatus::Enabled);
        item.on_unregister = (LoginItemStatus::Enabled, Err("busy".into()));
        assert_eq!(set_auto_start_enabled(&item, false), Err("busy".into()));
    }

    #[test]
    fn without_sm_app_service_nothing_is_attempted() {
        let item = FakeItem::new(LoginItemStatus::Unavailable);
        assert_eq!(
            set_auto_start_enabled(&item, true),
            Err(UNAVAILABLE_MESSAGE.to_string())
        );
        assert!(item.calls.borrow().is_empty());
    }
}
