//! The macOS native listener and its event decoding: Globe/Fn, lone right-side
//! modifiers and mouse buttons 4/5, which a global shortcut cannot see. The OS
//! side is an [`EventTapPort`] (a CGEventTap in `retone-macos`).

use super::globe_preference::{FnUsagePort, GlobePreference, MarkerStore};

/// What the listener must do beyond watching: which mouse buttons to swallow,
/// and whether to hold the macOS Globe action down.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListenerConfig {
    /// "MouseButton4"/"MouseButton5", de-duplicated and sorted.
    pub mouse_buttons: Vec<String>,
    pub suppress_globe_action: bool,
}

impl ListenerConfig {
    pub fn new<S: AsRef<str>>(mouse_buttons: &[S], suppress_globe_action: bool) -> Self {
        let mut buttons: Vec<String> = mouse_buttons
            .iter()
            .filter_map(|b| canonical_mouse_button(b.as_ref()))
            .map(str::to_string)
            .collect();
        buttons.sort();
        buttons.dedup();
        Self {
            mouse_buttons: buttons,
            suppress_globe_action,
        }
    }
}

fn canonical_mouse_button(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "mousebutton4" => Some("MouseButton4"),
        "mousebutton5" => Some("MouseButton5"),
        _ => None,
    }
}

/// An event the listener reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeKeyEvent {
    GlobeDown,
    GlobeUp,
    RightModifierDown(&'static str),
    RightModifierUp(&'static str),
    MouseButtonDown(&'static str),
    MouseButtonUp(&'static str),
}

/// The modifier flags of a `flagsChanged` event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModifierFlags {
    pub function: bool,
    pub command: bool,
    pub option: bool,
    pub control: bool,
    pub shift: bool,
}

/// Virtual key codes of the right-side modifiers and the flag each sets.
const RIGHT_MODIFIERS: [(u16, &str); 4] = [
    (61, "RightOption"),
    (54, "RightCommand"),
    (62, "RightControl"),
    (60, "RightShift"),
];

/// Turns `flagsChanged` events into Globe and right-modifier transitions, as
/// the Swift listener's global monitor did.
#[derive(Debug, Default)]
pub struct FlagsTracker {
    fn_down: bool,
}

impl FlagsTracker {
    pub fn on_flags_changed(&mut self, key_code: u16, flags: ModifierFlags) -> Vec<NativeKeyEvent> {
        let mut events = Vec::new();
        if flags.function && !self.fn_down {
            self.fn_down = true;
            events.push(NativeKeyEvent::GlobeDown);
        } else if !flags.function && self.fn_down {
            self.fn_down = false;
            events.push(NativeKeyEvent::GlobeUp);
        }
        if let Some((_, name)) = RIGHT_MODIFIERS.iter().find(|(code, _)| *code == key_code) {
            let held = match *name {
                "RightOption" => flags.option,
                "RightCommand" => flags.command,
                "RightControl" => flags.control,
                _ => flags.shift,
            };
            events.push(if held {
                NativeKeyEvent::RightModifierDown(name)
            } else {
                NativeKeyEvent::RightModifierUp(name)
            });
        }
        events
    }
}

/// `kCGMouseEventButtonNumber` 3/4 are the side buttons.
pub fn mouse_button_name(button_number: i64) -> Option<&'static str> {
    match button_number {
        3 => Some("MouseButton4"),
        4 => Some("MouseButton5"),
        _ => None,
    }
}

/// The event tap. Events go to whatever sink the implementation was built with.
pub trait EventTapPort: Send {
    /// Starts watching. Fails without Accessibility/Input Monitoring.
    fn start(&mut self) -> Result<(), String>;
    fn stop(&mut self);
    fn is_running(&self) -> bool;
    /// The side buttons to swallow; an active mouse tap exists only while this
    /// is non-empty.
    fn set_mouse_buttons(&mut self, buttons: &[String]);
}

/// Owns the event tap and the Globe preference, and keeps both in line with
/// the configuration.
pub struct NativeListener<T: EventTapPort, F: FnUsagePort, M: MarkerStore> {
    tap: T,
    preference: GlobePreference<F, M>,
    config: ListenerConfig,
    wanted: bool,
}

impl<T: EventTapPort, F: FnUsagePort, M: MarkerStore> NativeListener<T, F, M> {
    pub fn new(tap: T, preference: GlobePreference<F, M>) -> Self {
        Self {
            tap,
            preference,
            config: ListenerConfig::default(),
            wanted: false,
        }
    }

    pub fn tap(&self) -> &T {
        &self.tap
    }

    pub fn preference(&self) -> &GlobePreference<F, M> {
        &self.preference
    }

    pub fn config(&self) -> &ListenerConfig {
        &self.config
    }

    /// Whether the listener has been asked to run (it may have failed to).
    pub fn is_wanted(&self) -> bool {
        self.wanted
    }

    pub fn is_running(&self) -> bool {
        self.tap.is_running()
    }

    /// Puts back a Globe preference a crashed run left behind.
    pub fn restore_leftover_preference(&mut self) {
        self.preference.restore_leftover();
    }

    /// Replaces the whole configuration. A running listener is reconfigured in
    /// place: a restart would drop events and briefly hand Globe back to macOS.
    /// Returns false when nothing changed.
    pub fn set_configuration(&mut self, config: ListenerConfig) -> bool {
        if config == self.config {
            return false;
        }
        self.config = config;
        if self.tap.is_running() {
            self.apply();
        }
        true
    }

    /// Starts the tap if it isn't running, then applies the configuration: the
    /// Globe action is only taken over once the listener is known to work.
    pub fn start(&mut self) -> Result<(), String> {
        self.wanted = true;
        if self.tap.is_running() {
            return Ok(());
        }
        self.tap.start()?;
        tracing::info!(target: "hotkey", config = ?self.config, "Native key listener started");
        self.apply();
        Ok(())
    }

    /// Stops the tap and hands the Globe action back.
    pub fn stop(&mut self) {
        self.wanted = false;
        self.preference.restore();
        if self.tap.is_running() {
            self.tap.set_mouse_buttons(&[]);
            self.tap.stop();
            tracing::info!(target: "hotkey", "Native key listener stopped");
        }
    }

    fn apply(&mut self) {
        self.tap.set_mouse_buttons(&self.config.mouse_buttons);
        if self.config.suppress_globe_action {
            self.preference.apply();
        } else {
            self.preference.restore();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::globe_preference::fakes::{FakeFnUsage, FakeMarker};
    use super::*;

    #[derive(Default)]
    struct FakeTap {
        running: bool,
        fail: bool,
        starts: usize,
        mouse_calls: Vec<Vec<String>>,
    }

    impl EventTapPort for FakeTap {
        fn start(&mut self) -> Result<(), String> {
            self.starts += 1;
            if self.fail {
                return Err("Failed to create event tap".into());
            }
            self.running = true;
            Ok(())
        }
        fn stop(&mut self) {
            self.running = false;
        }
        fn is_running(&self) -> bool {
            self.running
        }
        fn set_mouse_buttons(&mut self, buttons: &[String]) {
            self.mouse_calls.push(buttons.to_vec());
        }
    }

    type Listener = NativeListener<FakeTap, FakeFnUsage, FakeMarker>;

    fn listener(fn_value: i32) -> (Listener, FakeFnUsage, FakeMarker) {
        let fn_usage = FakeFnUsage::new(fn_value, true);
        let marker = FakeMarker::default();
        let pref = GlobePreference::new(fn_usage.clone(), Some(marker.clone()));
        (
            NativeListener::new(FakeTap::default(), pref),
            fn_usage,
            marker,
        )
    }

    fn config(buttons: &[&str], suppress: bool) -> ListenerConfig {
        ListenerConfig::new(buttons, suppress)
    }

    #[test]
    fn config_keeps_only_side_buttons_deduplicated_and_sorted() {
        let c = config(
            &[
                "MouseButton5",
                "MouseButton4",
                "mousebutton4",
                "MouseButton3",
            ],
            true,
        );
        assert_eq!(c.mouse_buttons, ["MouseButton4", "MouseButton5"]);
        assert!(c.suppress_globe_action);
    }

    #[test]
    fn starting_applies_the_configuration_only_once_the_tap_works() {
        let (mut l, fn_usage, marker) = listener(2);
        l.set_configuration(config(&["MouseButton4"], true));
        assert!(
            l.tap().mouse_calls.is_empty(),
            "nothing applies before start"
        );
        assert!(fn_usage.updates().is_empty());

        l.start().unwrap();
        assert_eq!(l.tap().mouse_calls, [vec!["MouseButton4".to_string()]]);
        assert_eq!(fn_usage.value(), 0);
        assert!(marker.contents().is_some());

        l.stop();
        assert_eq!(fn_usage.value(), 2, "stop hands the Globe action back");
        assert_eq!(marker.contents(), None);
        assert!(!l.is_running());
    }

    #[test]
    fn a_failed_start_leaves_the_globe_preference_alone() {
        let (mut l, fn_usage, _) = listener(2);
        l.tap.fail = true;
        l.set_configuration(config(&[], true));
        assert!(l.start().is_err());
        assert!(fn_usage.updates().is_empty());
        assert!(l.is_wanted() && !l.is_running());
        // A later attempt (e.g. after Accessibility is granted) retries.
        l.tap.fail = false;
        l.start().unwrap();
        assert_eq!(l.tap().starts, 2);
        assert_eq!(fn_usage.value(), 0);
    }

    #[test]
    fn a_configuration_change_reconfigures_the_running_listener_without_restarting() {
        let (mut l, fn_usage, _) = listener(2);
        l.start().unwrap();
        assert!(l.set_configuration(config(&["MouseButton4"], true)));
        assert_eq!(l.tap().starts, 1, "listener should not be restarted");
        assert_eq!(l.tap().mouse_calls.last().unwrap(), &["MouseButton4"]);
        assert_eq!(fn_usage.value(), 0);
    }

    #[test]
    fn an_unchanged_configuration_does_nothing() {
        let (mut l, fn_usage, _) = listener(2);
        l.set_configuration(config(&["MouseButton4"], true));
        l.start().unwrap();
        let calls = l.tap().mouse_calls.len();
        assert!(!l.set_configuration(config(&["MouseButton4", "MouseButton4"], true)));
        assert_eq!(l.tap().mouse_calls.len(), calls);
        assert_eq!(fn_usage.updates(), [0]);
    }

    #[test]
    fn successive_changes_each_reach_the_listener_as_full_state() {
        let (mut l, fn_usage, _) = listener(2);
        l.start().unwrap();
        l.set_configuration(config(&[], true));
        l.set_configuration(config(&["MouseButton5"], true));
        l.set_configuration(config(&[], false));
        let calls: Vec<Vec<String>> = l.tap().mouse_calls.clone();
        assert_eq!(
            calls,
            [vec![], vec![], vec!["MouseButton5".to_string()], vec![]]
        );
        assert_eq!(fn_usage.updates(), [0, 2], "suppressed once, restored once");
    }

    #[test]
    fn a_restart_picks_up_the_latest_configuration() {
        let (mut l, fn_usage, _) = listener(2);
        l.start().unwrap();
        l.stop();
        l.set_configuration(config(&["MouseButton4"], true));
        l.start().unwrap();
        assert_eq!(l.tap().starts, 2);
        assert_eq!(l.tap().mouse_calls.last().unwrap(), &["MouseButton4"]);
        assert_eq!(fn_usage.value(), 0);
    }

    #[test]
    fn leftover_preference_recovery_does_not_start_the_listener() {
        let fn_usage = FakeFnUsage::new(0, true);
        let marker = FakeMarker::with(r#"{"version":1,"originalValue":2,"keyExisted":true}"#);
        let pref = GlobePreference::new(fn_usage.clone(), Some(marker.clone()));
        let mut l = NativeListener::new(FakeTap::default(), pref);
        l.restore_leftover_preference();
        assert_eq!(fn_usage.value(), 2);
        assert_eq!(marker.contents(), None);
        assert_eq!(l.tap().starts, 0);
    }

    #[test]
    fn flags_changed_reports_globe_and_right_modifier_transitions() {
        let mut tracker = FlagsTracker::default();
        let fn_flags = ModifierFlags {
            function: true,
            ..ModifierFlags::default()
        };
        assert_eq!(
            tracker.on_flags_changed(63, fn_flags),
            [NativeKeyEvent::GlobeDown]
        );
        assert!(
            tracker.on_flags_changed(63, fn_flags).is_empty(),
            "no repeat while held"
        );
        assert_eq!(
            tracker.on_flags_changed(63, ModifierFlags::default()),
            [NativeKeyEvent::GlobeUp]
        );

        let command = ModifierFlags {
            command: true,
            ..ModifierFlags::default()
        };
        assert_eq!(
            tracker.on_flags_changed(54, command),
            [NativeKeyEvent::RightModifierDown("RightCommand")]
        );
        assert_eq!(
            tracker.on_flags_changed(54, ModifierFlags::default()),
            [NativeKeyEvent::RightModifierUp("RightCommand")]
        );
        // The left Command key (55) is not a right-side modifier.
        assert!(tracker.on_flags_changed(55, command).is_empty());
        let option = ModifierFlags {
            option: true,
            ..ModifierFlags::default()
        };
        assert_eq!(
            tracker.on_flags_changed(61, option),
            [NativeKeyEvent::RightModifierDown("RightOption")]
        );
    }

    #[test]
    fn side_buttons_are_named_and_others_ignored() {
        assert_eq!(mouse_button_name(3), Some("MouseButton4"));
        assert_eq!(mouse_button_name(4), Some("MouseButton5"));
        assert_eq!(mouse_button_name(2), None);
    }
}
