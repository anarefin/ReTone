//! Selection sessions: a capture
//! that found a selection opens a session holding the text it read; replacing
//! it later re-reads the selection the same way and pastes only while it is
//! still the same text in the same app.
//!
//! Both operations take the [`InputLayer`] and must run inside the app's
//! clipboard-operation lock (they read through the clipboard and paste).

use crate::input::{
    InputLayer, PasteOptions, ReadOptions, SelectionRead, TargetApp, UnavailableCode,
};
use crate::payload::Capture;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A captured selection stays replaceable this long.
pub const SESSION_TTL: Duration = Duration::from_secs(5 * 60);
/// The largest selection the picker rewrites, in code points.
pub const MAX_SELECTION_EDIT_CODE_POINTS: usize = 6000;

#[derive(Debug, Clone)]
struct Session {
    text: String,
    target: TargetApp,
    /// The replace re-check reads the selection the same way the capture did,
    /// so the texts compare equal.
    confirm_none: bool,
    expires_at: Instant,
}

/// Why a replacement was not pasted. `as_str` is the code the picker shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceError {
    InvalidReplacement,
    SessionExpired,
    TargetChanged,
    SelectionChanged,
    SelectionUnavailable,
    ModifiersHeld,
    PasteFailed,
}

impl ReplaceError {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplaceError::InvalidReplacement => "invalid_replacement",
            ReplaceError::SessionExpired => "session_expired",
            ReplaceError::TargetChanged => "target_changed",
            ReplaceError::SelectionChanged => "selection_changed",
            ReplaceError::SelectionUnavailable => "selection_unavailable",
            ReplaceError::ModifiersHeld => "modifiers_held",
            ReplaceError::PasteFailed => "paste_failed",
        }
    }
}

/// Open selection sessions.
#[derive(Default)]
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn prune(&self, now: Instant) {
        self.lock().retain(|_, session| session.expires_at > now);
    }

    /// Reads the captured target's selection. A selection within the size cap
    /// opens a session; `confirm_none` confirms an unreliable "nothing
    /// selected" with a synthetic copy (the caller acts on it).
    pub fn capture(&self, layer: &InputLayer, confirm_none: bool) -> Capture {
        let now = layer.ports().clock.now();
        self.prune(now);
        let Some(pid) = layer.last_target_pid() else {
            return Capture::Unavailable {
                code: UnavailableCode::TargetUnavailable.as_str().into(),
            };
        };
        let (text, target) = match layer.read_selection(
            Some(pid),
            ReadOptions {
                activate: false,
                confirm_none,
            },
        ) {
            SelectionRead::Selected { text, target } => (text, target),
            SelectionRead::None { .. } => return Capture::None,
            SelectionRead::TargetChanged => return Capture::TargetChanged,
            SelectionRead::Unavailable { code } => {
                return Capture::Unavailable {
                    code: code.as_str().into(),
                };
            }
        };

        let character_count = text.chars().count();
        if character_count > MAX_SELECTION_EDIT_CODE_POINTS {
            return Capture::TooLarge {
                character_count,
                max_characters: MAX_SELECTION_EDIT_CODE_POINTS,
            };
        }
        let session_id = new_session_id();
        self.lock().insert(
            session_id.clone(),
            Session {
                text: text.clone(),
                target,
                confirm_none,
                expires_at: now + SESSION_TTL,
            },
        );
        Capture::Selected {
            session_id,
            text,
            character_count,
        }
    }

    /// Replaces the session's selection with `replacement`: the session is
    /// used up, the target is brought frontmost and its selection re-read, and
    /// the paste happens only when it is still the captured text. The user's
    /// clipboard is restored after the paste when `restore_clipboard` is set.
    pub fn replace(
        &self,
        layer: &InputLayer,
        session_id: &str,
        replacement: &str,
        restore_clipboard: bool,
    ) -> Result<(), ReplaceError> {
        if replacement.is_empty() {
            return Err(ReplaceError::InvalidReplacement);
        }
        self.prune(layer.ports().clock.now());
        let session = self
            .lock()
            .remove(session_id)
            .ok_or(ReplaceError::SessionExpired)?;

        let current = layer.read_selection(
            Some(session.target.pid),
            ReadOptions {
                activate: true,
                confirm_none: session.confirm_none,
            },
        );
        match current {
            SelectionRead::TargetChanged => return Err(ReplaceError::TargetChanged),
            SelectionRead::Unavailable {
                code: UnavailableCode::ModifiersHeld,
            } => return Err(ReplaceError::ModifiersHeld),
            SelectionRead::Unavailable { .. } => return Err(ReplaceError::SelectionUnavailable),
            SelectionRead::Selected { ref text, .. } if *text == session.text => {}
            SelectionRead::Selected { .. } | SelectionRead::None { .. } => {
                return Err(ReplaceError::SelectionChanged);
            }
        }

        layer
            .paste_text(
                replacement,
                PasteOptions {
                    restore_clipboard,
                    allow_clipboard_fallback: false,
                    silent_accessibility_check: false,
                },
            )
            .map(|_| ())
            .map_err(|error| {
                tracing::warn!(target: "clipboard", %error, "Selection replacement paste failed");
                ReplaceError::PasteFailed
            })
    }
}

/// A random UUID-shaped id (v4 layout). Not a secret: it only matches a
/// replace to its capture.
fn new_session_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let high = RandomState::new().hash_one((nanos, count));
    let low = RandomState::new().hash_one((count, nanos, std::process::id()));
    let value = (u128::from(high) << 64) | u128::from(low);
    let value = (value & !(0xF << 76)) | (0x4 << 76); // version 4
    let value = (value & !(0x3 << 62)) | (0x2 << 62); // RFC 4122 variant
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::fakes::*;
    use crate::input::ports::{AxSelection, NativeRead, Pasteboard, Shortcut};

    fn select(h: &Harness, text: &str) {
        h.reader
            .queue_native([NativeRead::Read(AxSelection::Selected(text.into()))]);
    }

    fn captured(h: &Harness, store: &SessionStore, text: &str) -> String {
        h.layer.capture_target();
        select(h, text);
        match store.capture(&h.layer, true) {
            Capture::Selected { session_id, .. } => session_id,
            other => panic!("expected a selection, got {other:?}"),
        }
    }

    #[test]
    fn session_ids_are_unique_uuids() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn a_selection_opens_a_session_with_its_code_point_count() {
        let h = Harness::new();
        let store = SessionStore::new();
        h.layer.capture_target();
        select(&h, "héllo 👋");
        match store.capture(&h.layer, true) {
            Capture::Selected {
                text,
                character_count,
                ..
            } => {
                assert_eq!(text, "héllo 👋");
                assert_eq!(character_count, 7);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn no_captured_target_is_target_unavailable() {
        let h = Harness::with_frontmost(None);
        let store = SessionStore::new();
        assert_eq!(
            store.capture(&h.layer, true),
            Capture::Unavailable {
                code: "target_unavailable".into()
            }
        );
    }

    #[test]
    fn a_selection_over_the_cap_is_too_large_and_opens_no_session() {
        let h = Harness::new();
        let store = SessionStore::new();
        h.layer.capture_target();
        select(&h, &"ab".repeat(3001));
        assert_eq!(
            store.capture(&h.layer, true),
            Capture::TooLarge {
                character_count: 6002,
                max_characters: 6000
            }
        );
        assert!(store.is_empty());

        // Exactly at the cap is fine; the cap counts code points, not bytes.
        select(&h, &"é".repeat(6000));
        assert!(matches!(
            store.capture(&h.layer, true),
            Capture::Selected {
                character_count: 6000,
                ..
            }
        ));
    }

    #[test]
    fn nothing_selected_and_a_changed_target_map_to_their_captures() {
        let h = Harness::new();
        let store = SessionStore::new();
        h.layer.capture_target();
        h.reader.queue_native([NativeRead::Read(AxSelection::None)]);
        assert_eq!(store.capture(&h.layer, true), Capture::None);

        h.workspace
            .set_frontmost(Some(FakeWorkspace::app(7, "Safari")));
        assert_eq!(store.capture(&h.layer, true), Capture::TargetChanged);
    }

    #[test]
    fn replace_pastes_when_the_selection_is_unchanged_and_uses_up_the_session() {
        let h = Harness::new();
        h.pasteboard.external_write("user clipboard");
        let store = SessionStore::new();
        let id = captured(&h, &store, "hey there");

        select(&h, "hey there");
        assert_eq!(store.replace(&h.layer, &id, "Hello there.", true), Ok(()));
        assert!(h.keys.calls().contains(&KeyCall::Post(Shortcut::Paste)));
        // Restored after the paste.
        assert_eq!(h.pasteboard.read_text().as_deref(), Some("user clipboard"));
        // A session replaces once.
        assert_eq!(
            store.replace(&h.layer, &id, "Again", true),
            Err(ReplaceError::SessionExpired)
        );
    }

    #[test]
    fn replace_declines_a_changed_selection() {
        let h = Harness::new();
        let store = SessionStore::new();
        let id = captured(&h, &store, "hey there");
        select(&h, "something else");
        assert_eq!(
            store.replace(&h.layer, &id, "Hello.", true),
            Err(ReplaceError::SelectionChanged)
        );

        let id = captured(&h, &store, "hey there");
        h.reader.queue_native([NativeRead::Read(AxSelection::None)]);
        assert_eq!(
            store.replace(&h.layer, &id, "Hello.", true),
            Err(ReplaceError::SelectionChanged)
        );
        assert!(!h.keys.calls().contains(&KeyCall::Post(Shortcut::Paste)));
    }

    #[test]
    fn replace_declines_when_another_app_took_over_and_cannot_be_activated() {
        let h = Harness::new();
        let store = SessionStore::new();
        let id = captured(&h, &store, "hey");
        h.workspace
            .set_frontmost(Some(FakeWorkspace::app(7, "Safari")));
        assert_eq!(
            store.replace(&h.layer, &id, "Hello.", true),
            Err(ReplaceError::TargetChanged)
        );
    }

    #[test]
    fn sessions_expire_after_five_minutes() {
        let h = Harness::new();
        let store = SessionStore::new();
        let id = captured(&h, &store, "hey");
        h.clock.advance(SESSION_TTL);
        select(&h, "hey");
        assert_eq!(
            store.replace(&h.layer, &id, "Hello.", true),
            Err(ReplaceError::SessionExpired)
        );
        assert!(store.is_empty());

        let id = captured(&h, &store, "hey");
        h.clock.advance(SESSION_TTL - Duration::from_secs(1));
        select(&h, "hey");
        assert_eq!(store.replace(&h.layer, &id, "Hello.", true), Ok(()));
    }

    #[test]
    fn an_empty_replacement_or_unknown_session_is_refused() {
        let h = Harness::new();
        let store = SessionStore::new();
        let id = captured(&h, &store, "hey");
        assert_eq!(
            store.replace(&h.layer, &id, "", true),
            Err(ReplaceError::InvalidReplacement)
        );
        // The refused call did not use the session up.
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.replace(&h.layer, "nope", "Hello.", true),
            Err(ReplaceError::SessionExpired)
        );
    }

    #[test]
    fn a_paste_without_accessibility_fails_with_the_text_on_the_clipboard() {
        let h = Harness::new();
        let store = SessionStore::new();
        let id = captured(&h, &store, "hey");
        h.accessibility
            .trusted
            .store(false, std::sync::atomic::Ordering::SeqCst);
        select(&h, "hey");
        assert_eq!(
            store.replace(&h.layer, &id, "Hello.", true),
            Err(ReplaceError::PasteFailed)
        );
    }

    #[test]
    fn error_codes_are_stable() {
        let codes: Vec<_> = [
            ReplaceError::InvalidReplacement,
            ReplaceError::SessionExpired,
            ReplaceError::TargetChanged,
            ReplaceError::SelectionChanged,
            ReplaceError::SelectionUnavailable,
            ReplaceError::ModifiersHeld,
            ReplaceError::PasteFailed,
        ]
        .iter()
        .map(|e| e.as_str())
        .collect();
        assert_eq!(
            codes,
            [
                "invalid_replacement",
                "session_expired",
                "target_changed",
                "selection_changed",
                "selection_unavailable",
                "modifiers_held",
                "paste_failed"
            ]
        );
    }
}
