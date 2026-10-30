//! Slot manager tests: the Fn-combination guard, rollback, unregistering, and
//! the macOS conflict, suggestion and capture paths.

use super::*;
use crate::i18n::Catalog;
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};

static EN: LazyLock<Catalog> = LazyLock::new(|| {
    let mut catalog = Catalog::new();
    catalog
        .add_json(
            "en",
            include_str!("../../../../src/locales/en/translation.json"),
        )
        .unwrap();
    catalog
});

fn translate() -> Translate {
    Arc::new(|key, vars| EN.t("en", key, vars))
}

/// `globalShortcut` stub: accelerators containing "BAD" simulate an OS refusal,
/// "INVALID" an unparsable accelerator.
#[derive(Default)]
struct FakeBackend {
    registered: BTreeMap<String, PressHandler>,
    order: Vec<String>,
    unregister_calls: Vec<String>,
}

impl FakeBackend {
    fn keys(&self) -> Vec<String> {
        self.order.clone()
    }
    fn has(&self, accelerator: &str) -> bool {
        self.registered.contains_key(accelerator)
    }
    fn fire(&self, accelerator: &str) {
        (self.registered[accelerator])();
    }
}

impl ShortcutBackend for FakeBackend {
    fn register(&mut self, accelerator: &str, on_press: PressHandler) -> Result<(), RegisterError> {
        if accelerator.contains("INVALID") {
            return Err(RegisterError::Invalid("bad accelerator".into()));
        }
        if accelerator.contains("BAD") || self.registered.contains_key(accelerator) {
            return Err(RegisterError::Refused);
        }
        self.registered.insert(accelerator.to_string(), on_press);
        self.order.push(accelerator.to_string());
        Ok(())
    }
    fn unregister(&mut self, accelerator: &str) {
        self.unregister_calls.push(accelerator.to_string());
        self.registered.remove(accelerator);
        self.order.retain(|a| a != accelerator);
    }
    fn is_registered(&self, accelerator: &str) -> bool {
        self.registered.contains_key(accelerator)
    }
    fn unregister_all(&mut self) {
        self.registered.clear();
        self.order.clear();
    }
}

fn manager() -> HotkeyManager<FakeBackend> {
    HotkeyManager::new(FakeBackend::default(), translate())
}

fn noop() -> SlotCallback {
    Arc::new(|_| {})
}

fn recorder() -> (SlotCallback, Arc<Mutex<Vec<String>>>) {
    let fired = Arc::new(Mutex::new(Vec::new()));
    let sink = fired.clone();
    (
        Arc::new(move |hotkey: &str| sink.lock().unwrap().push(hotkey.to_string())),
        fired,
    )
}

// --- hotkeyFnGuard ---

#[test]
fn an_fn_combination_is_rejected_without_registering_its_base_key() {
    let mut m = manager();
    let result = m.register_slot("slot-a", "Fn+A", noop(), true);
    assert!(!result.success);
    assert_eq!(result.reason.as_deref(), Some("fn_combination_unsupported"));
    assert_eq!(
        result.error.as_deref(),
        Some("The Globe/Fn key can only be used by itself.")
    );
    assert!(m.backend().keys().is_empty());
}

#[test]
fn rejecting_an_fn_combination_preserves_the_previous_binding() {
    let mut m = manager();
    let (callback, fired) = recorder();
    m.register_slot("slot-a", "Control+Shift+A", callback.clone(), true);
    let result = m.register_slot("slot-a", "Fn+A", callback, true);
    assert!(!result.success);
    assert_eq!(m.backend().keys(), ["Control+Shift+A"]);
    m.backend().fire("Control+Shift+A");
    assert_eq!(*fired.lock().unwrap(), ["Control+Shift+A"]);
}

#[test]
fn ordinary_hotkeys_register_and_fire_with_the_hotkey_that_fired() {
    let mut m = manager();
    let (callback, fired) = recorder();
    m.register_slot("slot-a", "Control+Shift+A", callback, true);
    m.backend().fire("Control+Shift+A");
    assert_eq!(*fired.lock().unwrap(), ["Control+Shift+A"]);
}

// --- hotkeySlotRollback ---

#[test]
fn atomic_failure_keeps_the_slots_previous_working_hotkeys() {
    let mut m = manager();
    let first = m.register_slot("slot-b", "F7", noop(), true);
    assert!(first.success);
    assert_eq!(m.slot_hotkeys("slot-b"), ["F7"]);

    let second = m.register_slot("slot-b", "F7,Control+BAD", noop(), true);
    assert!(!second.success);
    assert_eq!(m.slot_hotkeys("slot-b"), ["F7"]);
    assert!(
        m.backend().has("F7"),
        "previous hotkey must stay registered after rollback"
    );
    assert!(!m.backend().has("Control+BAD"));
}

#[test]
fn non_atomic_keeps_the_working_subset_and_reports_failures() {
    let mut m = manager();
    let result = m.register_slot("slot-b", "F7,Control+BAD", noop(), false);
    assert!(result.success);
    assert_eq!(result.hotkeys, ["F7"]);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].hotkey, "Control+BAD");
}

#[test]
fn re_registering_a_slot_replaces_its_previous_accelerators() {
    let mut m = manager();
    m.register_slot("slot-b", "F7", noop(), true);
    let result = m.register_slot("slot-b", "F6,F5", noop(), true);
    assert!(result.success);
    assert_eq!(m.slot_hotkeys("slot-b"), ["F6", "F5"]);
    assert!(
        !m.backend().has("F7"),
        "old accelerator must be unregistered"
    );
    assert!(m.backend().has("F6") && m.backend().has("F5"));
}

// --- hotkeySlotUnregister ---

#[test]
fn unregister_slot_releases_every_accelerator_the_slot_registered() {
    let mut m = manager();
    m.register_slot("slot-b", "F7,Control+Alt", noop(), true);
    m.unregister_slot("slot-b");
    assert!(m.backend().keys().is_empty());
    assert!(m.slot_hotkeys("slot-b").is_empty());
}

#[test]
fn a_combo_freed_by_unregister_slot_can_be_registered_again() {
    let mut m = manager();
    m.register_slot("slot-b", "Control+Alt", noop(), true);
    m.unregister_slot("slot-b");
    let again = m.register_slot("slot-b", "Control+Alt", noop(), true);
    assert!(again.success);
    assert_eq!(m.slot_hotkeys("slot-b"), ["Control+Alt"]);
}

#[test]
fn unregister_slot_never_releases_an_accelerator_it_does_not_own() {
    let mut m = manager();
    m.register_slot("slot-b", "F7", noop(), true);
    m.register_slot("cancel", "F8", noop(), true);
    m.backend_mut().unregister_calls.clear();
    m.unregister_slot("slot-b");
    assert_eq!(m.backend().unregister_calls, ["F7"]);
    assert_eq!(m.backend().keys(), ["F8"]);
    assert_eq!(m.slot_hotkeys("cancel"), ["F8"]);
}

#[test]
fn unregister_slot_skips_hotkeys_a_native_listener_owns() {
    let mut m = manager();
    m.register_slot("slot-a", "F7,RightShift", noop(), true);
    m.backend_mut().unregister_calls.clear();
    m.unregister_slot("slot-a");
    assert_eq!(m.backend().unregister_calls, ["F7"]);
}

#[test]
fn unregister_slot_on_an_already_cleared_slot_is_a_no_op() {
    let mut m = manager();
    m.register_slot("slot-b", "F7", noop(), true);
    m.unregister_slot("slot-b");
    m.backend_mut().unregister_calls.clear();
    m.unregister_slot("slot-b");
    assert!(m.backend().unregister_calls.is_empty());
}

// --- macOS paths ---

#[test]
fn native_listener_hotkeys_succeed_without_an_accelerator() {
    let mut m = manager();
    let result = m.register_slot(
        TONE_REWRITE_SLOT,
        "GLOBE,MouseButton4,RightCommand",
        noop(),
        true,
    );
    assert!(result.success);
    assert_eq!(result.hotkey.as_deref(), Some("GLOBE"));
    assert!(m.backend().keys().is_empty());
    assert!(m.slot_needs_native_listener(TONE_REWRITE_SLOT));
    assert!(m.slot_has_hotkey(TONE_REWRITE_SLOT, "RightCommand"));
    assert_eq!(
        m.find_slot_by_hotkey("MouseButton4"),
        Some(TONE_REWRITE_SLOT)
    );
}

#[test]
fn right_side_modifiers_in_a_chord_register_as_plain_modifiers() {
    let mut m = manager();
    let (callback, fired) = recorder();
    assert!(
        m.register_slot(TONE_REWRITE_SLOT, "RightCommand+K", callback, true)
            .success
    );
    assert_eq!(m.backend().keys(), ["Command+K"]);
    m.backend().fire("Command+K");
    assert_eq!(*fired.lock().unwrap(), ["RightCommand+K"]);
}

#[test]
fn mac_listener_config_lists_mouse_buttons_and_globe_ownership() {
    let mut m = manager();
    m.register_slot(
        TONE_REWRITE_SLOT,
        "MouseButton5,F8,Fn,MouseButton4",
        noop(),
        true,
    );
    let config = m.mac_native_listener_config(&[TONE_REWRITE_SLOT]);
    assert_eq!(config.mouse_buttons, ["MouseButton4", "MouseButton5"]);
    assert!(config.suppress_globe_action);

    m.register_slot(TONE_REWRITE_SLOT, "F8", noop(), true);
    let config = m.mac_native_listener_config(&[TONE_REWRITE_SLOT]);
    assert!(config.mouse_buttons.is_empty() && !config.suppress_globe_action);
    assert!(!m.slot_needs_native_listener(TONE_REWRITE_SLOT));
}

#[test]
fn a_refused_hotkey_explains_and_suggests_mac_alternatives() {
    let mut m = manager();
    let result = m.register_slot(TONE_REWRITE_SLOT, "Control+BAD", noop(), true);
    assert!(!result.success);
    assert_eq!(result.reason.as_deref(), Some("registration_failed"));
    assert_eq!(
        result.suggestions,
        ["Control+Alt", "Alt+Command", "Command+Shift+Space"]
    );
    assert_eq!(
        result.error.as_deref(),
        Some(
            "Could not register \"Control+BAD\". It may be in use by another application. \
             Try: Control+Alt, Alt+Command, Command+Shift+Space"
        )
    );
}

#[test]
fn single_key_suggestions_skip_keys_another_slot_holds() {
    let mut m = manager();
    m.register_slot("other", "F8", noop(), true);
    let result = m.register_slot(TONE_REWRITE_SLOT, "BAD", noop(), true);
    assert_eq!(result.suggestions, ["F9", "F10", "Pause"]);
}

#[test]
fn an_unparsable_accelerator_fails_without_suggestions() {
    let mut m = manager();
    let result = m.register_slot(TONE_REWRITE_SLOT, "Control+INVALID", noop(), true);
    assert!(!result.success);
    assert_eq!(result.reason, None);
    assert!(result.suggestions.is_empty());
    assert_eq!(
        result.error.as_deref(),
        Some("Could not register \"Control+INVALID\". It may be in use by another application.")
    );
}

#[test]
fn an_empty_list_is_refused() {
    let mut m = manager();
    let result = m.register_slot(TONE_REWRITE_SLOT, " , ", noop(), true);
    assert!(!result.success);
    assert!(m.slot_hotkeys(TONE_REWRITE_SLOT).is_empty());
}

#[test]
fn a_hotkey_another_slot_holds_is_a_slot_conflict() {
    let mut m = manager();
    m.register_slot(TONE_REWRITE_SLOT, "F8", noop(), true);
    let result = m.register_slot("other", "F9,F8", noop(), true);
    assert!(!result.success);
    assert_eq!(result.reason.as_deref(), Some("slot_conflict"));
    assert_eq!(result.conflict_slot.as_deref(), Some(TONE_REWRITE_SLOT));
    assert_eq!(
        result.error.as_deref(),
        Some("This hotkey is already used for Hotkey.")
    );
    assert!(
        !m.backend().has("F9"),
        "conflicts are rejected before registering anything"
    );

    // The accelerator form conflicts too: RightCommand+K holds Command+K.
    m.register_slot(TONE_REWRITE_SLOT, "RightCommand+K", noop(), true);
    let result = m.register_slot("other", "Command+K", noop(), true);
    assert_eq!(result.reason.as_deref(), Some("slot_conflict"));
}

#[test]
fn already_registered_accelerators_report_it() {
    let mut m = manager();
    m.backend_mut().register("F9", Box::new(|| {})).unwrap();
    let result = m.register_slot(TONE_REWRITE_SLOT, "F9", noop(), true);
    assert_eq!(result.reason.as_deref(), Some("already_registered"));
    assert!(
        result
            .error
            .unwrap()
            .starts_with("\"F9\" is already registered by another application.")
    );
}

#[test]
fn capture_suspends_accelerators_and_rearms_them_afterwards() {
    let mut m = manager();
    let (callback, fired) = recorder();
    m.register_slot(TONE_REWRITE_SLOT, "F8,GLOBE", callback, true);
    m.suspend_for_capture();
    assert!(m.backend().keys().is_empty());
    assert_eq!(
        m.slot_hotkeys(TONE_REWRITE_SLOT),
        ["F8", "GLOBE"],
        "bindings are kept"
    );

    m.rearm_after_capture();
    assert_eq!(m.backend().keys(), ["F8"]);
    assert_eq!(m.slot_hotkeys(TONE_REWRITE_SLOT), ["F8", "GLOBE"]);
    m.backend().fire("F8");
    assert_eq!(*fired.lock().unwrap(), ["F8"]);
}

#[test]
fn rearm_skips_native_only_and_unarmed_slots() {
    let mut m = manager();
    m.register_slot(TONE_REWRITE_SLOT, "GLOBE", noop(), true);
    m.suspend_for_capture();
    m.rearm_after_capture();
    assert!(m.backend().keys().is_empty());
    assert_eq!(m.slot_hotkeys(TONE_REWRITE_SLOT), ["GLOBE"]);
}

#[test]
fn restore_failures_become_notices_with_fallback_suggestions() {
    let mut m = manager();
    let result = m.register_slot(TONE_REWRITE_SLOT, "F7,Control+INVALID", noop(), false);
    let notices = m.restore_failure_notices("F7,Control+INVALID", &result);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].hotkey, "Control+INVALID");
    assert_eq!(notices[0].suggestions, FALLBACK_HOTKEYS);

    let failed = m.register_slot("other", "BAD", noop(), false);
    let notices = m.restore_failure_notices("BAD", &failed);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].hotkey, "BAD");
    assert_eq!(notices[0].suggestions, failed.suggestions);
}

#[test]
fn unregister_all_clears_every_slot() {
    let mut m = manager();
    m.register_slot(TONE_REWRITE_SLOT, "F8,GLOBE", noop(), true);
    m.unregister_all();
    assert!(m.backend().keys().is_empty());
    assert!(m.slot_hotkeys(TONE_REWRITE_SLOT).is_empty());
    assert!(!m.is_hotkey_registered("F8"));
}
