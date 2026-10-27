//! The macOS input layer's logic: target capture, selection reads with the
//! synthetic-copy fallback, pasting with clipboard save/restore, and the
//! Accessibility check: the read/copy/paste mechanics (sessions live in
//! `crate::session`, the flow in `crate::payload`).
//!
//! Every method blocks (sleeps go through [`Clock`]). The app crate runs them
//! on a blocking thread, serialized by its clipboard-operation lock.

use super::ports::{
    Accessibility, AxSelection, Clock, KeySynth, KeySynthError, NativeRead, Pasteboard,
    PasteboardSnapshot, RunningApp, SelectionReader, Shortcut, Workspace,
};
use super::signatures::{
    is_line_copy_editor, is_terminal_signature, looks_like_line_copy, target_names,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Wait between writing the text and posting ⌘V.
pub const PASTE_DELAY: Duration = Duration::from_millis(120);
/// Wait between ⌘V and putting the user's clipboard back.
pub const RESTORE_DELAY: Duration = Duration::from_millis(450);
/// Wait before the one retry of a failed paste.
pub const PASTE_RETRY_DELAY: Duration = Duration::from_millis(200);
/// A non-silent Accessibility verdict is reused this long (it debounces the
/// denial dialog).
pub const ACCESSIBILITY_CHECK_TTL: Duration = Duration::from_secs(5);
/// One hotkey press captures the target from more than one call site; a
/// capture this recent is reused.
pub const TARGET_CAPTURE_FRESHNESS: Duration = Duration::from_millis(250);
/// Deadline of one native `AXSelectedText` read (it retries the focused
/// element for ~1.2 s) and of the AppleScript fallback.
pub const SELECTED_TEXT_TIMEOUT: Duration = Duration::from_millis(1600);
/// Activation is confirmed by polling the frontmost app this many times…
pub const ACTIVATE_CONFIRM_RETRIES: u32 = 6;
/// …this far apart.
pub const ACTIVATE_CONFIRM_DELAY: Duration = Duration::from_millis(25);
/// How long the synthetic ⌘C waits for the hotkey's keys to be released.
pub const MODIFIER_RELEASE_WAIT: Duration = Duration::from_millis(1000);
/// Ceiling for one synthetic-copy round trip.
pub const COPY_TIMEOUT: Duration = Duration::from_millis(1200);
/// Pasteboard poll interval while waiting for the copy to land.
pub const CLIPBOARD_POLL: Duration = Duration::from_millis(20);

/// The OS boundaries the layer runs on.
#[derive(Clone)]
pub struct InputPorts {
    pub pasteboard: Arc<dyn Pasteboard>,
    pub accessibility: Arc<dyn Accessibility>,
    pub keys: Arc<dyn KeySynth>,
    pub workspace: Arc<dyn Workspace>,
    pub reader: Arc<dyn SelectionReader>,
    pub clock: Arc<dyn Clock>,
}

/// The app a selection was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetApp {
    pub pid: i32,
    /// Known when the selection came from a synthetic copy.
    pub name: Option<String>,
}

impl TargetApp {
    fn pid(pid: i32) -> Self {
        TargetApp { pid, name: None }
    }
}

impl From<RunningApp> for TargetApp {
    fn from(app: RunningApp) -> Self {
        TargetApp {
            pid: app.pid,
            name: app.name,
        }
    }
}

/// Why a selection could not be read. `as_str` is the error code the renderer
/// and the session code use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableCode {
    TargetUnavailable,
    ActivationFailed,
    AccessibilityUnavailable,
    ModifiersHeld,
    SelectionUnreadable,
}

impl UnavailableCode {
    pub fn as_str(self) -> &'static str {
        match self {
            UnavailableCode::TargetUnavailable => "target_unavailable",
            UnavailableCode::ActivationFailed => "activation_failed",
            UnavailableCode::AccessibilityUnavailable => "accessibility_unavailable",
            UnavailableCode::ModifiersHeld => "modifiers_held",
            UnavailableCode::SelectionUnreadable => "selection_unreadable",
        }
    }
}

/// The current selection of the target app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionRead {
    Selected {
        text: String,
        target: TargetApp,
    },
    /// Nothing selected (or a terminal / line-copy selection, which counts as
    /// nothing).
    None {
        target: TargetApp,
    },
    /// Another app is frontmost, or the copy landed in another app.
    TargetChanged,
    Unavailable {
        code: UnavailableCode,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadOptions {
    /// Bring the target frontmost first (the replace path; the picker holds
    /// focus then).
    pub activate: bool,
    /// The caller acts on "nothing selected", so a "none" from an app whose
    /// tree was found dormant is confirmed with a synthetic copy.
    pub confirm_none: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteOptions {
    /// Put the user's clipboard back after the paste (if nobody replaced it).
    pub restore_clipboard: bool,
    /// Without Accessibility, leave the text on the clipboard and report
    /// [`PasteOutcome::ClipboardOnly`] instead of failing. Silent.
    pub allow_clipboard_fallback: bool,
    /// Check Accessibility without the denial dialog.
    pub silent_accessibility_check: bool,
}

impl Default for PasteOptions {
    fn default() -> Self {
        PasteOptions {
            restore_clipboard: true,
            allow_clipboard_fallback: false,
            silent_accessibility_check: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteOutcome {
    Pasted,
    /// Left on the clipboard for a manual paste (`allow_clipboard_fallback`).
    ClipboardOnly,
}

/// A failed paste. The text stays on the clipboard in every case but
/// `Clipboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteError {
    /// The clipboard write itself failed. Never an Accessibility problem.
    Clipboard(String),
    AccessibilityRequired,
    LayoutUnresolved,
    Failed(String),
}

impl PasteError {
    /// `ACCESSIBILITY_PERMISSION_REQUIRED` for a denial.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            PasteError::AccessibilityRequired => Some("ACCESSIBILITY_PERMISSION_REQUIRED"),
            _ => None,
        }
    }

    /// Whether the text is on the clipboard for a manual paste.
    pub fn clipboard_copied(&self) -> bool {
        !matches!(self, PasteError::Clipboard(_))
    }
}

impl std::fmt::Display for PasteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasteError::Clipboard(message) => f.write_str(message),
            PasteError::AccessibilityRequired => f.write_str(
                "Accessibility permissions required for automatic pasting. Text has been copied to clipboard - please paste manually with Cmd+V.",
            ),
            PasteError::LayoutUnresolved => f.write_str(
                "Paste could not resolve the active keyboard layout. Text is copied to clipboard - please paste manually with Cmd+V.",
            ),
            PasteError::Failed(reason) => write!(
                f,
                "Paste failed ({reason}). Text is copied to clipboard - please paste manually with Cmd+V."
            ),
        }
    }
}

impl std::error::Error for PasteError {}

/// Why a select-all was declined. `as_str` is the error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectAllError {
    TargetUnavailable,
    TerminalTarget,
    TargetChanged,
    SelectAllFailed,
}

impl SelectAllError {
    pub fn as_str(self) -> &'static str {
        match self {
            SelectAllError::TargetUnavailable => "target_unavailable",
            SelectAllError::TerminalTarget => "terminal_target",
            SelectAllError::TargetChanged => "target_changed",
            SelectAllError::SelectAllFailed => "select_all_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyFailure {
    Untrusted,
    ModifiersHeld,
    Unreadable,
}

impl CopyFailure {
    fn code(self) -> UnavailableCode {
        match self {
            CopyFailure::Untrusted => UnavailableCode::AccessibilityUnavailable,
            CopyFailure::ModifiersHeld => UnavailableCode::ModifiersHeld,
            // A layout without ⌘C, no frontmost app or a failed post: trusted,
            // but unread.
            CopyFailure::Unreadable => UnavailableCode::SelectionUnreadable,
        }
    }
}

#[derive(Default)]
struct TargetState {
    pid: Option<i32>,
    captured_at: Option<Instant>,
}

type DeniedHandler = Box<dyn Fn() + Send + Sync>;

/// The input layer. One per app.
pub struct InputLayer {
    ports: InputPorts,
    accessibility_cache: Mutex<Option<(bool, Instant)>>,
    // Held for the whole frontmost lookup, so concurrent captures share one
    // lookup and an older lookup can never overwrite a newer target.
    target: Mutex<TargetState>,
    dormant_pids: Mutex<HashSet<i32>>,
    on_accessibility_denied: DeniedHandler,
}

impl InputLayer {
    pub fn new(ports: InputPorts) -> Self {
        InputLayer {
            ports,
            accessibility_cache: Mutex::new(None),
            target: Mutex::new(TargetState::default()),
            dormant_pids: Mutex::new(HashSet::new()),
            on_accessibility_denied: Box::new(|| {}),
        }
    }

    /// Called when a non-silent check finds Accessibility denied (the app shows
    /// its dialog). At most once per [`ACCESSIBILITY_CHECK_TTL`].
    pub fn with_accessibility_denied_handler(
        mut self,
        handler: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.on_accessibility_denied = Box::new(handler);
        self
    }

    pub fn ports(&self) -> &InputPorts {
        &self.ports
    }

    // ---- Accessibility ----------------------------------------------------

    /// Whether the app is trusted for Accessibility. A non-silent check reuses a
    /// verdict for 5 s and calls the denial handler when it is `false`; a silent
    /// one always asks the OS and touches neither the cache nor the handler.
    pub fn check_accessibility(&self, silent: bool) -> bool {
        let now = self.ports.clock.now();
        if !silent
            && let Some((value, expires_at)) = *lock(&self.accessibility_cache)
            && now < expires_at
        {
            return value;
        }

        let allowed = self.ports.accessibility.is_trusted();
        if !silent {
            *lock(&self.accessibility_cache) = Some((allowed, now + ACCESSIBILITY_CHECK_TTL));
            if !allowed {
                (self.on_accessibility_denied)();
            }
        }
        allowed
    }

    /// Asks macOS to show its Accessibility prompt if the app is not trusted.
    pub fn prompt_accessibility(&self) -> bool {
        self.invalidate_accessibility_cache();
        self.ports.accessibility.prompt()
    }

    /// Checked early (when a hotkey is saved) so the first paste doesn't pay
    /// for it. Silent: no dialog.
    pub fn pre_warm_accessibility(&self) -> bool {
        self.check_accessibility(true)
    }

    fn invalidate_accessibility_cache(&self) {
        *lock(&self.accessibility_cache) = None;
    }

    // ---- Target ------------------------------------------------------------

    /// The frontmost app's pid, captured at hotkey time before the picker takes
    /// focus. A capture from the last 250 ms is reused; a failed one is not.
    pub fn capture_target(&self) -> Option<i32> {
        let mut state = lock(&self.target);
        let now = self.ports.clock.now();
        if let (Some(pid), Some(at)) = (state.pid, state.captured_at)
            && now.saturating_duration_since(at) < TARGET_CAPTURE_FRESHNESS
        {
            return Some(pid);
        }
        state.pid = None;
        let pid = self.ports.workspace.frontmost_app().map(|app| app.pid);
        state.captured_at = Some(self.ports.clock.now());
        state.pid = pid;
        tracing::debug!(target: "tone-rewrite", ?pid, "Captured target pid");
        pid
    }

    /// The last captured target pid.
    pub fn last_target_pid(&self) -> Option<i32> {
        lock(&self.target).pid
    }

    pub fn frontmost_pid(&self) -> Option<i32> {
        self.ports.workspace.frontmost_app().map(|app| app.pid)
    }

    /// Makes `pid` frontmost so a ⌘V lands in its focused field. Does nothing
    /// when it already is (re-activating an active Chromium app drops its
    /// field's focus). Otherwise activates and polls 6×25 ms until macOS
    /// reports it frontmost.
    pub fn activate_pid(&self, pid: i32) -> bool {
        if pid <= 0 {
            return false;
        }
        if self.frontmost_pid() == Some(pid) {
            return true;
        }
        self.ports.workspace.activate(pid);
        for _ in 0..ACTIVATE_CONFIRM_RETRIES {
            self.ports.clock.sleep(ACTIVATE_CONFIRM_DELAY);
            if self.frontmost_pid() == Some(pid) {
                tracing::debug!(target: "tone-rewrite", pid, "Activated target pid");
                return true;
            }
        }
        tracing::debug!(target: "tone-rewrite", pid, "Target did not become frontmost");
        false
    }

    /// Whether an app's name or executable says it is a terminal.
    pub fn is_terminal_pid(&self, pid: i32) -> bool {
        let Some(app) = self.ports.workspace.app_for_pid(pid) else {
            return false;
        };
        let names = app
            .executable_path
            .as_deref()
            .map(target_names)
            .unwrap_or_default();
        is_terminal_signature(Some(&names)) || is_terminal_signature(app.name.as_deref())
    }

    // ---- Selection ---------------------------------------------------------

    /// Whether this pid's accessibility tree was found dormant, so its
    /// selection is read through AppleScript or a synthetic copy.
    pub fn is_native_selection_unsupported(&self, pid: i32) -> bool {
        lock(&self.dormant_pids).contains(&pid)
    }

    /// The focused element's selection via Accessibility: the in-process read
    /// first (skipped for a dormant pid), AppleScript when it fails.
    pub fn read_ax_selection(&self, pid: i32) -> AxSelection {
        if pid <= 0 {
            return AxSelection::Unavailable;
        }
        if !self.is_native_selection_unsupported(pid) {
            match self.ports.reader.read_native(pid, SELECTED_TEXT_TIMEOUT) {
                NativeRead::Read(AxSelection::Unavailable) => {}
                NativeRead::Read(selection) => return selection,
                NativeRead::Failed {
                    persistent_no_value,
                } => {
                    if persistent_no_value {
                        lock(&self.dormant_pids).insert(pid);
                    }
                }
                // A read cut off by the deadline never finished its retries (the
                // tree could have woken later), so its verdict doesn't count.
                NativeRead::TimedOut => {}
            }
            tracing::debug!(
                target: "clipboard",
                pid,
                native_skipped_next_time = self.is_native_selection_unsupported(pid),
                "Native selected-text read unavailable; trying AppleScript"
            );
        }
        self.ports
            .reader
            .read_applescript(pid, SELECTED_TEXT_TIMEOUT)
    }

    /// The target's current selection. Accessibility first; when it can't tell
    /// (dormant Chromium tree, unreadable), a synthetic ⌘C through the
    /// clipboard, which is restored afterwards. `pid` defaults to the last
    /// captured target.
    ///
    /// Call inside the app's clipboard-operation lock.
    pub fn read_selection(&self, pid: Option<i32>, options: ReadOptions) -> SelectionRead {
        let Some(pid) = pid.or_else(|| self.last_target_pid()) else {
            return SelectionRead::Unavailable {
                code: UnavailableCode::TargetUnavailable,
            };
        };
        if let Some(frontmost) = self.frontmost_pid()
            && frontmost != pid
        {
            return SelectionRead::TargetChanged;
        }
        if options.activate && !self.activate_pid(pid) {
            return SelectionRead::Unavailable {
                code: UnavailableCode::ActivationFailed,
            };
        }

        match self.read_ax_selection(pid) {
            AxSelection::Selected(text) => SelectionRead::Selected {
                text,
                target: TargetApp::pid(pid),
            },
            // AppleScript can report an empty selection right after a dormant
            // app regains focus; a synthetic copy still finds it.
            AxSelection::None
                if options.confirm_none && self.is_native_selection_unsupported(pid) =>
            {
                self.read_selection_via_clipboard(pid)
            }
            AxSelection::None => SelectionRead::None {
                target: TargetApp::pid(pid),
            },
            AxSelection::Unknown | AxSelection::Unavailable => {
                self.read_selection_via_clipboard(pid)
            }
        }
    }

    /// The selection through a synthetic ⌘C. A terminal's selection reads as
    /// nothing selected (a pasted newline would execute).
    pub fn read_selection_via_clipboard(&self, pid: i32) -> SelectionRead {
        let read = self.capture_via_clipboard(pid);
        if let SelectionRead::Selected { target, .. } = &read
            && is_terminal_signature(target.name.as_deref())
        {
            return SelectionRead::None {
                target: target.clone(),
            };
        }
        read
    }

    /// Seeds the clipboard with a sentinel, sends ⌘C, polls up to 1.2 s for
    /// something new and puts the user's clipboard back if it still holds what
    /// we wrote.
    pub fn capture_via_clipboard(&self, expected_pid: i32) -> SelectionRead {
        let pasteboard = &self.ports.pasteboard;
        let original = pasteboard.save();
        let sentinel = new_sentinel();
        if let Err(error) = pasteboard.write_text(&sentinel) {
            tracing::debug!(target: "clipboard", %error, "Sentinel write failed");
            return SelectionRead::Unavailable {
                code: UnavailableCode::SelectionUnreadable,
            };
        }
        // macOS has one clipboard the sentinel always replaces, so only what it
        // reads back is the baseline: a copy matching the old clipboard is
        // still the selection.
        let baseline: HashSet<String> = pasteboard.read_text().into_iter().collect();

        let target = match self.synthetic_copy() {
            Ok(target) => target,
            Err(failure) => {
                self.restore_clipboard_if_ours(&original, &[&sentinel], &baseline);
                tracing::debug!(target: "clipboard", ?failure, "macOS synthetic copy failed");
                return SelectionRead::Unavailable {
                    code: failure.code(),
                };
            }
        };
        if target.pid != expected_pid {
            self.restore_clipboard_if_ours(&original, &[&sentinel], &baseline);
            return SelectionRead::TargetChanged;
        }

        let clock = &self.ports.clock;
        let deadline = clock.now() + COPY_TIMEOUT;
        let copied = loop {
            let found = pasteboard
                .read_text()
                .filter(|text| !text.is_empty() && *text != sentinel && !baseline.contains(text));
            if found.is_some() || clock.now() >= deadline {
                break found;
            }
            clock.sleep(CLIPBOARD_POLL);
        };

        let mut written = vec![sentinel.as_str()];
        if let Some(text) = copied.as_deref() {
            written.push(text);
        }
        self.restore_clipboard_if_ours(&original, &written, &baseline);

        let Some(text) = copied else {
            return SelectionRead::None { target };
        };
        // ⌘C on a bare caret in VS Code, JetBrains etc. copies the whole line;
        // that shape from such an editor is "nothing selected".
        if looks_like_line_copy(&text) && is_line_copy_editor(&[target.name.as_deref()]) {
            return SelectionRead::None { target };
        }
        SelectionRead::Selected { text, target }
    }

    /// ⌘C into the frontmost app, after the hotkey's modifiers are released.
    /// Returns the app that received it.
    fn synthetic_copy(&self) -> Result<TargetApp, CopyFailure> {
        if !self.ports.accessibility.is_trusted() {
            return Err(CopyFailure::Untrusted);
        }
        // The hotkey fires on key-down, so its keys can still be held: a ⌘C
        // posted into a held Shift reaches the app as ⌘⇧C.
        if !self
            .ports
            .keys
            .await_modifier_release(MODIFIER_RELEASE_WAIT)
        {
            return Err(CopyFailure::ModifiersHeld);
        }
        // Resolved before the keystroke: this is the app that receives it.
        let target = self
            .ports
            .workspace
            .frontmost_app()
            .ok_or(CopyFailure::Unreadable)?;
        match self.ports.keys.post_shortcut(Shortcut::Copy) {
            Ok(()) => Ok(target.into()),
            Err(KeySynthError::Untrusted) => Err(CopyFailure::Untrusted),
            Err(_) => Err(CopyFailure::Unreadable),
        }
    }

    /// Restores the snapshot only while the clipboard still holds text we
    /// wrote. If the user copied something meanwhile, theirs stays.
    fn restore_clipboard_if_ours(
        &self,
        original: &PasteboardSnapshot,
        written: &[&str],
        baseline: &HashSet<String>,
    ) {
        let pasteboard = &self.ports.pasteboard;
        let Some(current) = pasteboard.read_text() else {
            return;
        };
        let ours = written.contains(&current.as_str());
        if !ours {
            if !current.is_empty() && !baseline.contains(&current) {
                tracing::debug!(target: "clipboard", "Keeping a clipboard the user changed during capture");
            }
            return;
        }
        if let Err(error) = pasteboard.restore(original) {
            tracing::warn!(target: "clipboard", %error, "Clipboard restore failed");
        }
    }

    /// Selects everything in the target (⌘A through System Events). Terminals
    /// are declined: ⌘A there doesn't select a field.
    pub fn select_all(&self, pid: Option<i32>) -> Result<(), SelectAllError> {
        let pid = pid
            .or_else(|| self.last_target_pid())
            .ok_or(SelectAllError::TargetUnavailable)?;
        if self.is_terminal_pid(pid) {
            return Err(SelectAllError::TerminalTarget);
        }
        if let Some(frontmost) = self.frontmost_pid()
            && frontmost != pid
        {
            return Err(SelectAllError::TargetChanged);
        }
        self.ports
            .keys
            .osascript_shortcut(Shortcut::SelectAll)
            .map_err(|_| SelectAllError::SelectAllFailed)
    }

    // ---- Clipboard ---------------------------------------------------------

    /// Plain clipboard write (the renderer's copy buttons).
    pub fn write_clipboard(&self, text: &str) -> Result<(), PasteError> {
        self.ports
            .pasteboard
            .write_text(text)
            .map(|_| ())
            .map_err(|error| PasteError::Clipboard(error.0))
    }

    /// Pastes `text` into the frontmost app: saves the clipboard, writes the
    /// text, checks Accessibility, waits 120 ms, posts ⌘V (osascript if the
    /// CGEvent post fails), and 450 ms later restores the clipboard if it still
    /// holds our text. A failed paste is retried once after 200 ms. Returns
    /// after the restore.
    ///
    /// Call inside the app's clipboard-operation lock.
    pub fn paste_text(
        &self,
        text: &str,
        options: PasteOptions,
    ) -> Result<PasteOutcome, PasteError> {
        let started = self.ports.clock.now();
        let pasteboard = &self.ports.pasteboard;
        let snapshot = options.restore_clipboard.then(|| pasteboard.save());
        let mut change_count = pasteboard
            .write_text(text)
            .map_err(|error| PasteError::Clipboard(error.0))?;

        let silent = options.allow_clipboard_fallback || options.silent_accessibility_check;
        if !self.check_accessibility(silent) {
            if options.allow_clipboard_fallback {
                tracing::debug!(target: "clipboard", "No Accessibility; text left on the clipboard");
                return Ok(PasteOutcome::ClipboardOnly);
            }
            return Err(PasteError::AccessibilityRequired);
        }

        let result = match self.paste_macos(snapshot.as_ref(), text, change_count) {
            Ok(()) => Ok(()),
            Err(first) => {
                tracing::debug!(target: "clipboard", error = %first, "First paste attempt failed, retrying");
                change_count = pasteboard
                    .write_text(text)
                    .map_err(|error| PasteError::Clipboard(error.0))?;
                self.ports.clock.sleep(PASTE_RETRY_DELAY);
                self.paste_macos(snapshot.as_ref(), text, change_count)
            }
        };
        let elapsed = self.ports.clock.now().saturating_duration_since(started);
        match &result {
            Ok(()) => tracing::debug!(
                target: "clipboard",
                elapsed_ms = elapsed.as_millis() as u64,
                text_length = text.len(),
                "Paste complete"
            ),
            Err(error) => tracing::warn!(
                target: "clipboard",
                elapsed_ms = elapsed.as_millis() as u64,
                %error,
                "Paste failed"
            ),
        }
        result.map(|()| PasteOutcome::Pasted)
    }

    fn paste_macos(
        &self,
        snapshot: Option<&PasteboardSnapshot>,
        text: &str,
        change_count: i64,
    ) -> Result<(), PasteError> {
        self.ports.clock.sleep(PASTE_DELAY);
        match self.ports.keys.post_shortcut(Shortcut::Paste) {
            Ok(()) => tracing::debug!(target: "clipboard", "Pasted via CGEvent"),
            // Posting the US-ANSI key instead could run another shortcut.
            Err(KeySynthError::LayoutUnresolved) => return Err(PasteError::LayoutUnresolved),
            Err(error) => {
                tracing::debug!(target: "clipboard", %error, "CGEvent paste failed, falling back to osascript");
                self.invalidate_accessibility_cache();
                self.ports
                    .keys
                    .osascript_shortcut(Shortcut::Paste)
                    .map_err(|error| {
                        self.invalidate_accessibility_cache();
                        PasteError::Failed(error.to_string())
                    })?;
                tracing::debug!(target: "clipboard", "Pasted via osascript");
            }
        }
        self.restore_after_delay(snapshot, text, change_count);
        Ok(())
    }

    fn restore_after_delay(
        &self,
        snapshot: Option<&PasteboardSnapshot>,
        expected_text: &str,
        expected_change_count: i64,
    ) {
        let Some(snapshot) = snapshot else {
            return;
        };
        self.ports.clock.sleep(RESTORE_DELAY);
        let pasteboard = &self.ports.pasteboard;
        let unchanged = pasteboard.change_count() == expected_change_count
            || pasteboard.read_text().as_deref() == Some(expected_text);
        if !unchanged {
            tracing::debug!(target: "clipboard", "Skipping clipboard restore because the clipboard changed");
            return;
        }
        match pasteboard.restore(snapshot) {
            Ok(_) => tracing::debug!(target: "clipboard", "Clipboard restored"),
            Err(error) => tracing::warn!(target: "clipboard", %error, "Clipboard restore failed"),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A clipboard value no app would hold on its own.
fn new_sentinel() -> String {
    use std::hash::BuildHasher;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let state = std::collections::hash_map::RandomState::new();
    let a = state.hash_one((nanos, count));
    let b = state.hash_one((count, nanos, std::process::id()));
    format!("__RETONE_SELECTION_{a:016x}{b:016x}__")
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
