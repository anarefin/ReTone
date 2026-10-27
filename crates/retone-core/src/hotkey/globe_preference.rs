//! Holding the macOS Globe action at "Do Nothing" while ReTone owns the Globe
//! key.
//!
//! macOS runs the standalone Globe action (emoji viewer, input-source switch,
//! dictation) from WindowServer ahead of every event tap, so a listener cannot
//! consume the key; the only way to stop it firing alongside a Globe hotkey is
//! to hold `com.apple.HIToolbox AppleFnUsageType` at 0 while we own the key.
//! A marker file records the user's value before it changes, so a crash is
//! undone on the next launch.

use serde::{Deserialize, Serialize};

/// `AppleFnUsageType` "Do Nothing".
pub const FN_USAGE_DO_NOTHING: i32 = 0;
const STATE_VERSION: i32 = 1;

/// The Globe preference (`TISGetFnUsageType`/`TISUpdateFnUsageType`, which
/// persist the value and broadcast it so it applies live).
pub trait FnUsagePort: Send {
    /// False when the private entry points cannot be resolved.
    fn available(&self) -> bool;
    fn get(&self) -> i32;
    fn update(&self, value: i32);
    /// Whether the raw preference key exists (vs. a macOS-computed default).
    fn raw_value_exists(&self) -> bool;
    fn clear_raw_value(&self);
}

/// `globe-preference-state.json`.
pub trait MarkerStore: Send {
    fn read(&self) -> Option<String>;
    fn write(&self, contents: &str) -> Result<(), String>;
    fn remove(&self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobePreferenceState {
    pub version: i32,
    pub original_value: i32,
    pub key_existed: bool,
}

pub struct GlobePreference<F: FnUsagePort, M: MarkerStore> {
    fn_usage: F,
    marker: Option<M>,
    owned: Option<GlobePreferenceState>,
}

impl<F: FnUsagePort, M: MarkerStore> GlobePreference<F, M> {
    pub fn new(fn_usage: F, marker: Option<M>) -> Self {
        Self {
            fn_usage,
            marker,
            owned: None,
        }
    }

    pub fn fn_usage(&self) -> &F {
        &self.fn_usage
    }

    pub fn is_suppressing(&self) -> bool {
        self.owned.is_some()
    }

    pub fn has_marker(&self) -> bool {
        self.marker.as_ref().is_some_and(|m| m.read().is_some())
    }

    /// Adopts a marker left by a crashed run, so its recorded original value is
    /// used instead of mistaking our own override for the user's preference.
    pub fn recover_leftover_state(&mut self) {
        let Some(state) = self.read_marker() else {
            return;
        };
        if !self.fn_usage.available() {
            return;
        }
        if self.fn_usage.get() == FN_USAGE_DO_NOTHING {
            self.owned = Some(state);
        } else {
            // The user picked a new action since we died — theirs wins.
            self.remove_marker();
        }
    }

    /// Takes the Globe action over (sets "Do Nothing"), recording how to get back first.
    pub fn apply(&mut self) {
        if self.owned.is_some() {
            return;
        }
        if !self.fn_usage.available() {
            tracing::warn!(target: "hotkey", "Cannot suppress the macOS Globe action: TISUpdateFnUsageType unavailable");
            return;
        }
        let original = self.fn_usage.get();
        if original == FN_USAGE_DO_NOTHING {
            return;
        }
        let state = GlobePreferenceState {
            version: STATE_VERSION,
            original_value: original,
            key_existed: self.fn_usage.raw_value_exists(),
        };
        let Some(marker) = &self.marker else {
            tracing::warn!(target: "hotkey", "Cannot suppress the macOS Globe action: no state path provided");
            return;
        };
        let contents = serde_json::to_string(&state).expect("plain struct serializes");
        if let Err(error) = marker.write(&contents) {
            tracing::warn!(target: "hotkey", %error, "Cannot suppress the macOS Globe action");
            return;
        }
        self.owned = Some(state);
        self.fn_usage.update(FN_USAGE_DO_NOTHING);
        tracing::info!(target: "hotkey", original, "Suppressing the macOS Globe action");
    }

    /// Hands the Globe action back. Only puts the value back while it is still
    /// ours: if the user picked a new action meanwhile, that choice wins.
    pub fn restore(&mut self) {
        let Some(state) = self.owned.take() else {
            return;
        };
        if self.fn_usage.available() && self.fn_usage.get() == FN_USAGE_DO_NOTHING {
            self.fn_usage.update(state.original_value);
            // The value was a macOS-computed default before we touched it, so
            // clear the key again to keep it tracking hardware and input sources.
            if !state.key_existed {
                self.fn_usage.clear_raw_value();
            }
            tracing::info!(target: "hotkey", original = state.original_value, "Restored the macOS Globe action");
        }
        self.remove_marker();
    }

    /// One-shot crash recovery at startup: restore what a previous run left.
    pub fn restore_leftover(&mut self) {
        if !self.has_marker() {
            return;
        }
        tracing::info!(target: "hotkey", "Restoring the Globe preference left by a previous run");
        self.recover_leftover_state();
        self.restore();
    }

    fn read_marker(&self) -> Option<GlobePreferenceState> {
        let contents = self.marker.as_ref()?.read()?;
        match serde_json::from_str::<GlobePreferenceState>(&contents) {
            Ok(state) if state.version == STATE_VERSION => Some(state),
            _ => {
                self.remove_marker();
                None
            }
        }
    }

    fn remove_marker(&self) {
        if let Some(marker) = &self.marker {
            marker.remove();
        }
    }
}

/// A marker file on disk, written atomically.
pub struct FileMarker {
    path: std::path::PathBuf,
}

impl FileMarker {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl MarkerStore for FileMarker {
    fn read(&self) -> Option<String> {
        std::fs::read_to_string(&self.path).ok()
    }

    fn write(&self, contents: &str) -> Result<(), String> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, contents).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    pub struct FnState {
        pub value: i32,
        pub raw_exists: bool,
        pub available: bool,
        pub updates: Vec<i32>,
        pub cleared: usize,
    }

    #[derive(Clone)]
    pub struct FakeFnUsage(pub Arc<Mutex<FnState>>);

    impl FakeFnUsage {
        pub fn new(value: i32, raw_exists: bool) -> Self {
            Self(Arc::new(Mutex::new(FnState {
                value,
                raw_exists,
                available: true,
                ..FnState::default()
            })))
        }
        pub fn value(&self) -> i32 {
            self.0.lock().unwrap().value
        }
        pub fn updates(&self) -> Vec<i32> {
            self.0.lock().unwrap().updates.clone()
        }
    }

    impl FnUsagePort for FakeFnUsage {
        fn available(&self) -> bool {
            self.0.lock().unwrap().available
        }
        fn get(&self) -> i32 {
            self.0.lock().unwrap().value
        }
        fn update(&self, value: i32) {
            let mut s = self.0.lock().unwrap();
            s.value = value;
            s.raw_exists = true;
            s.updates.push(value);
        }
        fn raw_value_exists(&self) -> bool {
            self.0.lock().unwrap().raw_exists
        }
        fn clear_raw_value(&self) {
            let mut s = self.0.lock().unwrap();
            s.raw_exists = false;
            s.cleared += 1;
        }
    }

    #[derive(Clone, Default)]
    pub struct FakeMarker(pub Arc<Mutex<Option<String>>>);

    impl FakeMarker {
        pub fn with(contents: &str) -> Self {
            Self(Arc::new(Mutex::new(Some(contents.to_string()))))
        }
        pub fn contents(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl MarkerStore for FakeMarker {
        fn read(&self) -> Option<String> {
            self.contents()
        }
        fn write(&self, contents: &str) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(contents.to_string());
            Ok(())
        }
        fn remove(&self) {
            *self.0.lock().unwrap() = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::*;
    use super::*;

    fn preference(
        value: i32,
        raw: bool,
        marker: &FakeMarker,
    ) -> (GlobePreference<FakeFnUsage, FakeMarker>, FakeFnUsage) {
        let fn_usage = FakeFnUsage::new(value, raw);
        (
            GlobePreference::new(fn_usage.clone(), Some(marker.clone())),
            fn_usage,
        )
    }

    #[test]
    fn apply_records_the_original_before_changing_it_and_restore_puts_it_back() {
        let marker = FakeMarker::default();
        let (mut pref, fn_usage) = preference(2, true, &marker);

        pref.apply();
        assert_eq!(fn_usage.value(), FN_USAGE_DO_NOTHING);
        assert_eq!(
            marker.contents().as_deref(),
            Some(r#"{"version":1,"originalValue":2,"keyExisted":true}"#)
        );
        pref.apply();
        assert_eq!(fn_usage.updates(), [0], "a second apply is a no-op");

        pref.restore();
        assert_eq!(fn_usage.value(), 2);
        assert_eq!(marker.contents(), None);
        assert_eq!(fn_usage.0.lock().unwrap().cleared, 0);
    }

    #[test]
    fn a_computed_default_is_cleared_again_on_restore() {
        let marker = FakeMarker::default();
        let (mut pref, fn_usage) = preference(1, false, &marker);
        pref.apply();
        pref.restore();
        assert_eq!(fn_usage.value(), 1);
        assert!(!fn_usage.0.lock().unwrap().raw_exists);
    }

    #[test]
    fn nothing_changes_when_globe_already_does_nothing() {
        let marker = FakeMarker::default();
        let (mut pref, fn_usage) = preference(0, true, &marker);
        pref.apply();
        pref.restore();
        assert!(fn_usage.updates().is_empty());
        assert_eq!(marker.contents(), None);
    }

    #[test]
    fn the_users_new_choice_wins_over_restore() {
        let marker = FakeMarker::default();
        let (mut pref, fn_usage) = preference(2, true, &marker);
        pref.apply();
        fn_usage.0.lock().unwrap().value = 3; // changed in System Settings meanwhile
        pref.restore();
        assert_eq!(fn_usage.value(), 3);
        assert_eq!(marker.contents(), None);
    }

    #[test]
    fn no_marker_path_or_missing_entry_points_never_touch_the_preference() {
        let fn_usage = FakeFnUsage::new(2, true);
        let mut pref = GlobePreference::<_, FakeMarker>::new(fn_usage.clone(), None);
        pref.apply();
        assert!(fn_usage.updates().is_empty());

        let marker = FakeMarker::default();
        let (mut pref, fn_usage) = preference(2, true, &marker);
        fn_usage.0.lock().unwrap().available = false;
        pref.apply();
        assert!(fn_usage.updates().is_empty());
        assert_eq!(marker.contents(), None);
    }

    #[test]
    fn leftover_marker_from_a_crash_is_restored_at_startup() {
        let marker = FakeMarker::with(r#"{"version":1,"originalValue":2,"keyExisted":false}"#);
        let (mut pref, fn_usage) = preference(0, true, &marker);
        pref.restore_leftover();
        assert_eq!(fn_usage.value(), 2);
        assert!(!fn_usage.0.lock().unwrap().raw_exists);
        assert_eq!(marker.contents(), None);
        assert!(!pref.is_suppressing());
    }

    #[test]
    fn leftover_marker_is_dropped_when_the_user_changed_the_action() {
        let marker = FakeMarker::with(r#"{"version":1,"originalValue":2,"keyExisted":true}"#);
        let (mut pref, fn_usage) = preference(3, true, &marker);
        pref.restore_leftover();
        assert!(fn_usage.updates().is_empty());
        assert_eq!(marker.contents(), None);
    }

    #[test]
    fn malformed_or_foreign_version_markers_are_removed() {
        for contents in [
            "not json",
            r#"{"version":2,"originalValue":2,"keyExisted":true}"#,
        ] {
            let marker = FakeMarker::with(contents);
            let (mut pref, fn_usage) = preference(0, true, &marker);
            pref.restore_leftover();
            assert!(fn_usage.updates().is_empty(), "{contents}");
            assert_eq!(marker.contents(), None, "{contents}");
        }
    }

    #[test]
    fn recovery_without_a_marker_does_nothing() {
        let marker = FakeMarker::default();
        let (mut pref, fn_usage) = preference(0, true, &marker);
        pref.restore_leftover();
        assert!(fn_usage.updates().is_empty());
    }

    #[test]
    fn adopted_leftover_state_is_restored_by_a_later_restore() {
        // Recovering adopts the crashed run's original value; a later apply is a
        // no-op and the eventual restore puts the user's value back.
        let marker = FakeMarker::with(r#"{"version":1,"originalValue":2,"keyExisted":true}"#);
        let (mut pref, fn_usage) = preference(0, true, &marker);
        pref.recover_leftover_state();
        pref.apply();
        assert!(fn_usage.updates().is_empty());
        pref.restore();
        assert_eq!(fn_usage.value(), 2);
    }

    #[test]
    fn file_marker_round_trips() {
        let dir = std::env::temp_dir().join(format!("retone-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = FileMarker::new(dir.join("globe-preference-state.json"));
        assert_eq!(marker.read(), None);
        marker.write("{}").unwrap();
        assert_eq!(marker.read().as_deref(), Some("{}"));
        marker.remove();
        assert_eq!(marker.read(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
