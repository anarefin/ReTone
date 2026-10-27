//! The OS boundaries of the input layer. `retone-macos` implements them over
//! NSPasteboard, CGEvent, NSWorkspace and the Accessibility API; the tests in
//! this module use in-memory fakes.

use std::time::{Duration, Instant};

/// Monotonic time and blocking sleeps. Every wait in the input layer goes
/// through this, so tests run on virtual time.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// UTI of plain UTF-8 text (`NSPasteboardTypeString`).
pub const PLAIN_TEXT_TYPE: &str = "public.utf8-plain-text";

/// One pasteboard item: every type it carried, with its raw bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PasteboardItem {
    pub representations: Vec<(String, Vec<u8>)>,
}

/// Everything the pasteboard held: every item and every type of each, so a
/// restore brings back rich text, images and files, not just the plain text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PasteboardSnapshot {
    pub items: Vec<PasteboardItem>,
}

impl PasteboardSnapshot {
    pub fn is_empty(&self) -> bool {
        self.items
            .iter()
            .all(|item| item.representations.is_empty())
    }

    /// The first item's plain text, if it had any.
    pub fn plain_text(&self) -> Option<String> {
        self.items.iter().find_map(|item| {
            item.representations
                .iter()
                .find(|(kind, _)| kind == PLAIN_TEXT_TYPE)
                .and_then(|(_, data)| String::from_utf8(data.clone()).ok())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteboardError(pub String);

impl std::fmt::Display for PasteboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PasteboardError {}

/// The general pasteboard.
pub trait Pasteboard: Send + Sync {
    /// `NSPasteboard.changeCount`: bumped by every write, by anyone.
    fn change_count(&self) -> i64;
    /// The plain text on the pasteboard, if any.
    fn read_text(&self) -> Option<String>;
    /// Replaces the contents with plain text. Returns the new change count.
    fn write_text(&self, text: &str) -> Result<i64, PasteboardError>;
    /// Reads every item and type.
    fn save(&self) -> PasteboardSnapshot;
    /// Replaces the contents with a snapshot (an empty one clears it). Returns
    /// the new change count.
    fn restore(&self, snapshot: &PasteboardSnapshot) -> Result<i64, PasteboardError>;
}

/// `AXIsProcessTrustedWithOptions`.
pub trait Accessibility: Send + Sync {
    /// Whether this process is trusted, without prompting.
    fn is_trusted(&self) -> bool;
    /// Whether this process is trusted; asks macOS to show its prompt if not.
    fn prompt(&self) -> bool;
}

/// A Command shortcut the input layer synthesizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Copy,
    Paste,
    SelectAll,
}

impl Shortcut {
    /// The character the shortcut's key types with ⌘ held, looked up in the
    /// active keyboard layout.
    pub fn character(self) -> &'static str {
        match self {
            Shortcut::Copy => "c",
            Shortcut::Paste => "v",
            Shortcut::SelectAll => "a",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySynthError {
    /// The process lacks Accessibility trust, so posted events are dropped.
    Untrusted,
    /// The active layout has no key typing the shortcut's character.
    LayoutUnresolved,
    Failed(String),
}

impl std::fmt::Display for KeySynthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeySynthError::Untrusted => f.write_str("not trusted for Accessibility"),
            KeySynthError::LayoutUnresolved => {
                f.write_str("could not resolve the active keyboard layout")
            }
            KeySynthError::Failed(message) => f.write_str(message),
        }
    }
}

/// Synthetic keystrokes into the frontmost app.
pub trait KeySynth: Send + Sync {
    /// Waits up to `timeout` for Shift, Control, Option and Command to be
    /// released. `false` if one is still held at the deadline.
    fn await_modifier_release(&self, timeout: Duration) -> bool;
    /// Posts the shortcut as CGEvents (key down, key up, both with ⌘).
    fn post_shortcut(&self, shortcut: Shortcut) -> Result<(), KeySynthError>;
    /// Sends the shortcut through `osascript` and System Events.
    fn osascript_shortcut(&self, shortcut: Shortcut) -> Result<(), KeySynthError>;
}

/// A running app (`NSRunningApplication`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunningApp {
    pub pid: i32,
    pub name: Option<String>,
    pub executable_path: Option<String>,
}

/// `NSWorkspace` / `NSRunningApplication`.
pub trait Workspace: Send + Sync {
    fn frontmost_app(&self) -> Option<RunningApp>;
    fn app_for_pid(&self, pid: i32) -> Option<RunningApp>;
    /// Requests activation (all windows, ignoring other apps). Returns whether
    /// the request was accepted, not whether it has landed yet.
    fn activate(&self, pid: i32) -> bool;
}

/// The selection as an Accessibility read saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxSelection {
    Selected(String),
    /// The focused element has no selection.
    None,
    /// The focused element is the bare window: the dormant-tree signature
    /// (Chromium/Electron), so the selection state is unreadable, not empty.
    Unknown,
    Unavailable,
}

/// The outcome of one in-process `AXSelectedText` read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeRead {
    Read(AxSelection),
    /// The read failed. `persistent_no_value` is set when every attempt failed
    /// with `kAXErrorNoValue` (-25212) and the focused element never resolved
    /// late: the app's tree is dormant, so later reads go to AppleScript.
    Failed {
        persistent_no_value: bool,
    },
    /// The deadline passed before the read finished. Its verdict is unknown.
    TimedOut,
}

/// Reads the focused element's selected text of an app.
pub trait SelectionReader: Send + Sync {
    fn read_native(&self, pid: i32, timeout: Duration) -> NativeRead;
    /// The System Events fallback. Answers `Selected`, `None` or `Unavailable`.
    fn read_applescript(&self, pid: i32, timeout: Duration) -> AxSelection;
}
