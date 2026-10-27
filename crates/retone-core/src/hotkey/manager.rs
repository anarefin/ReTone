//! Named hotkey slots. Each slot
//! binds a list of hotkeys: regular chords go to a global-shortcut
//! [`ShortcutBackend`], while Globe, mouse-button and lone right-modifier
//! hotkeys get no accelerator (the native listener serves them). Only macOS is
//! supported.

use super::kinds::{
    is_globe_like_hotkey, is_modifier_only_hotkey, is_mouse_button_hotkey, is_right_side_modifier,
    is_unsupported_fn_combination, needs_native_listener, normalize_to_accelerator,
};
use super::list::{parse_hotkey_list, serialize_hotkey_items};
use super::native::ListenerConfig;
use serde::Serialize;
use std::sync::Arc;

/// The only slot: the ReTone picker hotkey.
pub const TONE_REWRITE_SLOT: &str = "toneRewrite";

/// Hotkeys suggested when a registration fails and nothing better fits.
pub const FALLBACK_HOTKEYS: [&str; 3] = ["F8", "F9", "Control+Shift+Space"];

const SINGLE_SUGGESTIONS: [&str; 5] = ["F8", "F9", "F10", "Pause", "ScrollLock"];
const MAC_COMPOUND_SUGGESTIONS: [&str; 3] = ["Control+Alt", "Alt+Command", "Command+Shift+Space"];

/// Settings titles that name each slot in a conflict message.
fn slot_label_key(slot: &str) -> Option<&'static str> {
    (slot == TONE_REWRITE_SLOT).then_some("reTone.hotkey.title")
}

/// `t(key, vars)` in the main-process language.
pub type Translate = Arc<dyn Fn(&str, &[(&str, &str)]) -> String + Send + Sync>;
/// A slot's press handler; receives the hotkey that fired.
pub type SlotCallback = Arc<dyn Fn(&str) + Send + Sync>;
/// What the backend runs on each press of one accelerator.
pub type PressHandler = Box<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// The accelerator cannot be parsed.
    Invalid(String),
    /// The OS or the backend refused it.
    Refused,
}

/// The global-shortcut service.
pub trait ShortcutBackend: Send {
    fn register(&mut self, accelerator: &str, on_press: PressHandler) -> Result<(), RegisterError>;
    fn unregister(&mut self, accelerator: &str);
    fn is_registered(&self, accelerator: &str) -> bool;
    fn unregister_all(&mut self);
}

#[derive(Default)]
struct Slot {
    hotkeys: Vec<String>,
    /// Index-for-index with `hotkeys`; `None` for native-listener entries.
    accelerators: Vec<Option<String>>,
    callback: Option<SlotCallback>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotkeyFailure {
    pub hotkey: String,
    pub error: String,
}

/// Outcome of registering a slot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterOutcome {
    pub success: bool,
    /// The primary hotkey on success.
    pub hotkey: Option<String>,
    pub hotkeys: Vec<String>,
    pub error: Option<String>,
    pub reason: Option<String>,
    pub suggestions: Vec<String>,
    /// Hotkeys that failed while others registered (non-atomic only).
    pub failures: Vec<HotkeyFailure>,
    pub conflict_slot: Option<String>,
}

impl RegisterOutcome {
    fn failed(error: String) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }
}

/// `hotkey-registration-failed` payload sent when a saved hotkey no longer
/// registers at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureNotice {
    pub hotkey: String,
    pub error: String,
    pub suggestions: Vec<String>,
}

struct SingleOutcome {
    hotkey: String,
    accelerator: Option<String>,
    error: Option<String>,
    reason: Option<String>,
    suggestions: Vec<String>,
}

impl SingleOutcome {
    fn ok(hotkey: &str, accelerator: Option<String>) -> Self {
        Self {
            hotkey: hotkey.to_string(),
            accelerator,
            error: None,
            reason: None,
            suggestions: Vec::new(),
        }
    }

    fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

pub struct HotkeyManager<B: ShortcutBackend> {
    backend: B,
    slots: Vec<(String, Slot)>,
    translate: Translate,
}

impl<B: ShortcutBackend> HotkeyManager<B> {
    pub fn new(backend: B, translate: Translate) -> Self {
        Self {
            backend,
            slots: Vec::new(),
            translate,
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    fn t(&self, key: &str, vars: &[(&str, &str)]) -> String {
        (self.translate)(key, vars)
    }

    fn registration_failed(&self, hotkey: &str) -> String {
        self.t("hotkey.errors.registrationFailed", &[("hotkey", hotkey)])
    }

    fn slot(&self, name: &str) -> Option<&Slot> {
        self.slots.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }

    fn ensure_slot(&mut self, name: &str) -> &mut Slot {
        if let Some(index) = self.slots.iter().position(|(n, _)| n == name) {
            return &mut self.slots[index].1;
        }
        self.slots.push((name.to_string(), Slot::default()));
        &mut self.slots.last_mut().expect("just pushed").1
    }

    /// Primary (first) hotkey of a slot.
    pub fn slot_hotkey(&self, slot: &str) -> Option<String> {
        self.slot(slot).and_then(|s| s.hotkeys.first().cloned())
    }

    /// Every hotkey bound to a slot.
    pub fn slot_hotkeys(&self, slot: &str) -> Vec<String> {
        self.slot(slot)
            .map(|s| s.hotkeys.clone())
            .unwrap_or_default()
    }

    pub fn slot_has_hotkey(&self, slot: &str, key: &str) -> bool {
        !key.is_empty()
            && self
                .slot(slot)
                .is_some_and(|s| s.hotkeys.iter().any(|h| h == key))
    }

    /// The slot that owns `key`; first match wins.
    pub fn find_slot_by_hotkey(&self, key: &str) -> Option<&str> {
        if key.is_empty() {
            return None;
        }
        self.slots
            .iter()
            .find(|(_, s)| s.hotkeys.iter().any(|h| h == key))
            .map(|(n, _)| n.as_str())
    }

    /// True when any hotkey of `slot` needs the native listener.
    pub fn slot_needs_native_listener(&self, slot: &str) -> bool {
        self.slot_hotkeys(slot)
            .iter()
            .any(|h| needs_native_listener(h))
    }

    /// Which mouse buttons the listener must swallow for these slots, and
    /// whether ReTone owns Globe (then macOS's own Globe action stands down).
    pub fn mac_native_listener_config(&self, slots: &[&str]) -> ListenerConfig {
        let mut mouse_buttons = Vec::new();
        let mut suppress_globe_action = false;
        for slot in slots {
            for hotkey in self.slot_hotkeys(slot) {
                if is_mouse_button_hotkey(&hotkey) {
                    mouse_buttons.push(hotkey);
                } else if is_globe_like_hotkey(&hotkey) {
                    suppress_globe_action = true;
                }
            }
        }
        ListenerConfig::new(&mouse_buttons, suppress_globe_action)
    }

    fn failure_reason(&self, accelerator: &str) -> (String, String, Vec<String>) {
        let suggestions = self.suggestions(accelerator);
        if self.backend.is_registered(accelerator) {
            return (
                "already_registered".into(),
                self.t(
                    "hotkey.errors.alreadyRegistered",
                    &[("hotkey", accelerator)],
                ),
                suggestions,
            );
        }
        (
            "registration_failed".into(),
            self.registration_failed(accelerator),
            suggestions,
        )
    }

    /// Up to three alternatives to `failed`, skipping keys another armed slot
    /// already holds.
    pub fn suggestions(&self, failed: &str) -> Vec<String> {
        let candidates: &[&str] = if failed.contains('+') {
            &MAC_COMPOUND_SUGGESTIONS
        } else {
            &SINGLE_SUGGESTIONS
        };
        let bound: Vec<&String> = self
            .slots
            .iter()
            .filter(|(_, s)| s.callback.is_some())
            .flat_map(|(_, s)| s.hotkeys.iter())
            .collect();
        let usable = |hotkey: &str| hotkey != failed && !bound.iter().any(|b| *b == hotkey);
        let mut picked: Vec<String> = candidates
            .iter()
            .copied()
            .filter(|h| usable(h))
            .map(str::to_string)
            .collect();
        if picked.is_empty() {
            picked = FALLBACK_HOTKEYS
                .iter()
                .copied()
                .filter(|h| usable(h))
                .map(str::to_string)
                .collect();
        }
        picked.truncate(3);
        picked
    }

    /// Registers a slot's hotkey list (comma-separated). Best-effort by default:
    /// succeeds if at least one hotkey registers, with the rest in `failures`.
    /// `atomic` rolls the slot back to its previous bindings on any failure.
    pub fn register_slot(
        &mut self,
        slot: &str,
        input: &str,
        callback: SlotCallback,
        atomic: bool,
    ) -> RegisterOutcome {
        let desired = parse_hotkey_list(input);
        if desired.is_empty() {
            return RegisterOutcome::failed(self.registration_failed(input));
        }
        let result = self.setup_shortcuts(slot, &desired, callback.clone(), atomic);
        if result.success {
            self.ensure_slot(slot).callback = Some(callback);
        }
        result
    }

    fn setup_shortcuts(
        &mut self,
        slot_name: &str,
        desired: &[String],
        callback: SlotCallback,
        atomic: bool,
    ) -> RegisterOutcome {
        tracing::debug!(
            target: "hotkey",
            slot = slot_name,
            hotkeys = %desired.join(", "),
            current = %self.slot_hotkeys(slot_name).join(", "),
            "Setting up hotkeys"
        );

        // Reject a conflict with another slot before tearing anything down.
        for hotkey in desired {
            if let Some(conflict) = self.find_slot_conflict(slot_name, hotkey) {
                return conflict;
            }
        }

        let slot = self.ensure_slot(slot_name);
        let previous_hotkeys = slot.hotkeys.clone();
        let previous_accelerators = slot.accelerators.clone();
        for accelerator in previous_accelerators.iter().flatten() {
            tracing::debug!(target: "hotkey", accelerator, "Unregistering previous accelerator");
            self.backend.unregister(accelerator);
        }

        let mut registered_hotkeys = Vec::new();
        let mut registered_accelerators = Vec::new();
        let mut failures: Vec<SingleOutcome> = Vec::new();
        for hotkey in desired {
            let outcome = self.register_single_hotkey(hotkey, callback.clone());
            if outcome.is_ok() {
                registered_hotkeys.push(outcome.hotkey);
                registered_accelerators.push(outcome.accelerator);
            } else {
                failures.push(outcome);
            }
        }

        if registered_hotkeys.is_empty() || (atomic && !failures.is_empty()) {
            // Roll back: release what we just registered, then restore the
            // previous bindings so the slot keeps working.
            for accelerator in registered_accelerators.iter().flatten() {
                self.backend.unregister(accelerator);
            }
            self.restore_previous(&previous_hotkeys, &previous_accelerators, &callback);
            let slot = self.ensure_slot(slot_name);
            slot.hotkeys = previous_hotkeys;
            slot.accelerators = previous_accelerators;

            let first = failures.into_iter().next();
            let suggestions = first
                .as_ref()
                .map(|f| f.suggestions.clone())
                .unwrap_or_default();
            let mut error = first
                .as_ref()
                .and_then(|f| f.error.clone())
                .unwrap_or_else(|| self.registration_failed(&desired[0]));
            if !suggestions.is_empty() {
                let joined = suggestions.join(", ");
                error.push(' ');
                error
                    .push_str(&self.t("hotkey.errors.trySuggestions", &[("suggestions", &joined)]));
            }
            return RegisterOutcome {
                success: false,
                error: Some(error),
                reason: first.and_then(|f| f.reason),
                suggestions,
                ..RegisterOutcome::default()
            };
        }

        let slot = self.ensure_slot(slot_name);
        slot.hotkeys = registered_hotkeys.clone();
        slot.accelerators = registered_accelerators;
        slot.callback = Some(callback);
        tracing::debug!(
            target: "hotkey",
            slot = slot_name,
            hotkeys = %registered_hotkeys.join(", "),
            "Slot registered"
        );

        RegisterOutcome {
            success: true,
            hotkey: registered_hotkeys.first().cloned(),
            hotkeys: registered_hotkeys,
            failures: failures
                .into_iter()
                .map(|f| HotkeyFailure {
                    hotkey: f.hotkey,
                    error: f.error.unwrap_or_default(),
                })
                .collect(),
            ..RegisterOutcome::default()
        }
    }

    /// Registers one hotkey without touching any slot. Native-listener hotkeys
    /// succeed with no accelerator.
    fn register_single_hotkey(&mut self, hotkey: &str, callback: SlotCallback) -> SingleOutcome {
        if is_mouse_button_hotkey(hotkey) {
            tracing::debug!(target: "hotkey", hotkey, "Mouse button set - using the native listener");
            return SingleOutcome::ok(hotkey, None);
        }
        if is_globe_like_hotkey(hotkey) {
            tracing::debug!(target: "hotkey", hotkey, "Globe/Fn key set - using the native listener");
            return SingleOutcome::ok(hotkey, None);
        }
        if is_unsupported_fn_combination(hotkey) {
            return SingleOutcome {
                error: Some(self.t("hotkey.errors.fnCombinationUnsupported", &[])),
                reason: Some("fn_combination_unsupported".into()),
                ..SingleOutcome::ok(hotkey, None)
            };
        }
        if is_right_side_modifier(hotkey) {
            tracing::debug!(target: "hotkey", hotkey, "Right-side modifier set - using the native listener");
            return SingleOutcome::ok(hotkey, None);
        }

        let accelerator = normalize_to_accelerator(hotkey);
        let fired = hotkey.to_string();
        let on_press: PressHandler = Box::new(move || callback(&fired));
        match self.backend.register(&accelerator, on_press) {
            Ok(()) => {
                tracing::debug!(target: "hotkey", hotkey, "Registration succeeded");
                SingleOutcome::ok(hotkey, Some(accelerator))
            }
            Err(RegisterError::Refused) => {
                let (reason, message, suggestions) = self.failure_reason(&accelerator);
                tracing::error!(target: "hotkey", hotkey, reason, "Failed to register hotkey");
                SingleOutcome {
                    error: Some(message),
                    reason: Some(reason),
                    suggestions,
                    ..SingleOutcome::ok(hotkey, None)
                }
            }
            Err(RegisterError::Invalid(detail)) => {
                tracing::error!(target: "hotkey", hotkey, detail, "Error setting up shortcut");
                SingleOutcome {
                    error: Some(self.registration_failed(hotkey)),
                    ..SingleOutcome::ok(hotkey, None)
                }
            }
        }
    }

    fn find_slot_conflict(&self, slot_name: &str, hotkey: &str) -> Option<RegisterOutcome> {
        let accelerator = (!(needs_native_listener(hotkey) || is_modifier_only_hotkey(hotkey)))
            .then(|| normalize_to_accelerator(hotkey));
        for (other_name, other) in &self.slots {
            if other_name == slot_name {
                continue;
            }
            let matches = other.hotkeys.iter().any(|h| h == hotkey)
                || accelerator
                    .as_ref()
                    .is_some_and(|a| other.accelerators.iter().flatten().any(|o| o == a));
            if matches {
                tracing::warn!(target: "hotkey", hotkey, slot = %other_name, "Hotkey conflicts with another slot");
                let label = slot_label_key(other_name)
                    .map(|key| self.t(key, &[]))
                    .unwrap_or_else(|| other_name.clone());
                return Some(RegisterOutcome {
                    error: Some(self.t("hotkey.errors.slotConflict", &[("slot", &label)])),
                    reason: Some("slot_conflict".into()),
                    conflict_slot: Some(other_name.clone()),
                    ..RegisterOutcome::default()
                });
            }
        }
        None
    }

    fn restore_previous(
        &mut self,
        hotkeys: &[String],
        accelerators: &[Option<String>],
        callback: &SlotCallback,
    ) {
        for (hotkey, accelerator) in hotkeys.iter().zip(accelerators) {
            // Native-listener entries are re-armed by the listener sync instead.
            let Some(accelerator) = accelerator else {
                continue;
            };
            let callback = callback.clone();
            let fired = hotkey.clone();
            let restored = self
                .backend
                .register(accelerator, Box::new(move || callback(&fired)))
                .is_ok();
            if restored {
                tracing::debug!(target: "hotkey", hotkey, "Restored previous hotkey after failed registration");
            } else {
                tracing::warn!(target: "hotkey", hotkey, "Could not restore previous hotkey");
            }
        }
    }

    /// Releases a slot's accelerators and clears it. No-op when empty.
    pub fn unregister_slot(&mut self, slot_name: &str) {
        let Some(index) = self.slots.iter().position(|(n, _)| n == slot_name) else {
            return;
        };
        let slot = &mut self.slots[index].1;
        if slot.hotkeys.is_empty() && slot.accelerators.is_empty() {
            return;
        }
        let accelerators = std::mem::take(&mut slot.accelerators);
        slot.hotkeys.clear();
        for accelerator in accelerators.iter().flatten() {
            self.backend.unregister(accelerator);
        }
    }

    /// Releases every slot's accelerators but keeps the bindings, so a hotkey
    /// being recorded is not intercepted (`set-hotkey-listening-mode` on).
    pub fn suspend_for_capture(&mut self) {
        let accelerators: Vec<String> = self
            .slots
            .iter()
            .flat_map(|(_, s)| s.accelerators.iter().flatten().cloned())
            .collect();
        for accelerator in accelerators {
            self.backend.unregister(&accelerator);
        }
    }

    /// Re-arms every armed slot after capture. Slots served only by the native
    /// listener need nothing here.
    pub fn rearm_after_capture(&mut self) {
        let armed: Vec<(String, Vec<String>, SlotCallback)> = self
            .slots
            .iter()
            .filter_map(|(name, slot)| {
                let callback = slot.callback.clone()?;
                (!slot.hotkeys.is_empty()).then(|| (name.clone(), slot.hotkeys.clone(), callback))
            })
            .collect();
        for (name, hotkeys, callback) in armed {
            let native_only = hotkeys
                .iter()
                .all(|h| needs_native_listener(h) || is_modifier_only_hotkey(h));
            if native_only {
                continue;
            }
            let result =
                self.register_slot(&name, &serialize_hotkey_items(&hotkeys), callback, false);
            if !result.success {
                tracing::warn!(target: "hotkey", slot = %name, "Slot was not restored after capture");
            }
        }
    }

    /// Notices for every saved hotkey that no longer registers, so a slot never
    /// goes silently dead at startup.
    pub fn restore_failure_notices(
        &self,
        requested: &str,
        result: &RegisterOutcome,
    ) -> Vec<FailureNotice> {
        let mut notices = Vec::new();
        let notice = |hotkey: &str, error: Option<&String>, suggestions: &[String]| FailureNotice {
            hotkey: hotkey.to_string(),
            error: error
                .cloned()
                .unwrap_or_else(|| self.registration_failed(hotkey)),
            suggestions: if suggestions.is_empty() {
                FALLBACK_HOTKEYS.iter().map(|s| s.to_string()).collect()
            } else {
                suggestions.to_vec()
            },
        };
        if !result.success {
            notices.push(notice(
                requested,
                result.error.as_ref(),
                &result.suggestions,
            ));
        }
        for failure in &result.failures {
            notices.push(notice(&failure.hotkey, Some(&failure.error), &[]));
        }
        notices
    }

    /// Clears every slot and releases every accelerator.
    pub fn unregister_all(&mut self) {
        for (_, slot) in &mut self.slots {
            slot.hotkeys.clear();
            slot.accelerators.clear();
        }
        self.backend.unregister_all();
    }

    pub fn is_hotkey_registered(&self, accelerator: &str) -> bool {
        self.backend.is_registered(accelerator)
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
