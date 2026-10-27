//! In-memory fakes of the input-layer ports, for headless tests.

use super::layer::{InputLayer, InputPorts};
use super::ports::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type SleepHook = Box<dyn FnMut(Duration) + Send>;

/// Virtual time: `sleep` advances `now` and records the duration.
pub struct FakeClock {
    now: Mutex<Instant>,
    pub sleeps: Mutex<Vec<Duration>>,
    on_sleep: Mutex<Option<SleepHook>>,
}

impl FakeClock {
    pub fn new() -> Arc<Self> {
        Arc::new(FakeClock {
            now: Mutex::new(Instant::now()),
            sleeps: Mutex::new(Vec::new()),
            on_sleep: Mutex::new(None),
        })
    }

    pub fn advance(&self, duration: Duration) {
        *self.now.lock().unwrap() += duration;
    }

    pub fn on_sleep(&self, hook: impl FnMut(Duration) + Send + 'static) {
        *self.on_sleep.lock().unwrap() = Some(Box::new(hook));
    }

    pub fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.lock().unwrap().clone()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
        self.sleeps.lock().unwrap().push(duration);
        if let Some(hook) = self.on_sleep.lock().unwrap().as_mut() {
            hook(duration);
        }
    }
}

#[derive(Default)]
struct BoardState {
    contents: PasteboardSnapshot,
    change_count: i64,
}

#[derive(Default)]
pub struct FakePasteboard {
    state: Mutex<BoardState>,
    pub fail_writes: AtomicBool,
    pub restores: AtomicUsize,
}

impl FakePasteboard {
    pub fn new() -> Arc<Self> {
        Arc::new(FakePasteboard::default())
    }

    pub fn text_item(text: &str) -> PasteboardItem {
        PasteboardItem {
            representations: vec![(PLAIN_TEXT_TYPE.to_string(), text.as_bytes().to_vec())],
        }
    }

    pub fn set_contents(&self, contents: PasteboardSnapshot) {
        let mut state = self.state.lock().unwrap();
        state.contents = contents;
        state.change_count += 1;
    }

    /// Another app (or the user) writes text.
    pub fn external_write(&self, text: &str) {
        self.set_contents(PasteboardSnapshot {
            items: vec![Self::text_item(text)],
        });
    }

    pub fn contents(&self) -> PasteboardSnapshot {
        self.state.lock().unwrap().contents.clone()
    }
}

impl Pasteboard for FakePasteboard {
    fn change_count(&self) -> i64 {
        self.state.lock().unwrap().change_count
    }

    fn read_text(&self) -> Option<String> {
        self.state.lock().unwrap().contents.plain_text()
    }

    fn write_text(&self, text: &str) -> Result<i64, PasteboardError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(PasteboardError("clipboard unavailable".into()));
        }
        self.external_write(text);
        Ok(self.change_count())
    }

    fn save(&self) -> PasteboardSnapshot {
        self.contents()
    }

    fn restore(&self, snapshot: &PasteboardSnapshot) -> Result<i64, PasteboardError> {
        self.restores.fetch_add(1, Ordering::SeqCst);
        self.set_contents(snapshot.clone());
        Ok(self.change_count())
    }
}

pub struct FakeAccessibility {
    pub trusted: AtomicBool,
    pub checks: AtomicUsize,
    pub prompts: AtomicUsize,
}

impl FakeAccessibility {
    pub fn new(trusted: bool) -> Arc<Self> {
        Arc::new(FakeAccessibility {
            trusted: AtomicBool::new(trusted),
            checks: AtomicUsize::new(0),
            prompts: AtomicUsize::new(0),
        })
    }
}

impl Accessibility for FakeAccessibility {
    fn is_trusted(&self) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.trusted.load(Ordering::SeqCst)
    }

    fn prompt(&self) -> bool {
        self.prompts.fetch_add(1, Ordering::SeqCst);
        self.trusted.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCall {
    AwaitModifiers,
    Post(Shortcut),
    Osascript(Shortcut),
}

/// Records keystrokes. A posted ⌘C writes `copy_text` to the pasteboard, as the
/// target app would.
pub struct FakeKeys {
    pasteboard: Arc<FakePasteboard>,
    pub calls: Mutex<Vec<KeyCall>>,
    pub modifiers_released: AtomicBool,
    pub post_results: Mutex<VecDeque<Result<(), KeySynthError>>>,
    pub osascript_results: Mutex<VecDeque<Result<(), KeySynthError>>>,
    pub copy_text: Mutex<Option<String>>,
}

impl FakeKeys {
    pub fn new(pasteboard: Arc<FakePasteboard>) -> Arc<Self> {
        Arc::new(FakeKeys {
            pasteboard,
            calls: Mutex::new(Vec::new()),
            modifiers_released: AtomicBool::new(true),
            post_results: Mutex::new(VecDeque::new()),
            osascript_results: Mutex::new(VecDeque::new()),
            copy_text: Mutex::new(None),
        })
    }

    pub fn calls(&self) -> Vec<KeyCall> {
        self.calls.lock().unwrap().clone()
    }

    pub fn queue_posts(&self, results: impl IntoIterator<Item = Result<(), KeySynthError>>) {
        self.post_results.lock().unwrap().extend(results);
    }

    pub fn queue_osascript(&self, results: impl IntoIterator<Item = Result<(), KeySynthError>>) {
        self.osascript_results.lock().unwrap().extend(results);
    }

    pub fn set_copy_text(&self, text: Option<&str>) {
        *self.copy_text.lock().unwrap() = text.map(str::to_string);
    }
}

impl KeySynth for FakeKeys {
    fn await_modifier_release(&self, _timeout: Duration) -> bool {
        self.calls.lock().unwrap().push(KeyCall::AwaitModifiers);
        self.modifiers_released.load(Ordering::SeqCst)
    }

    fn post_shortcut(&self, shortcut: Shortcut) -> Result<(), KeySynthError> {
        self.calls.lock().unwrap().push(KeyCall::Post(shortcut));
        let result = self
            .post_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()));
        if result.is_ok()
            && shortcut == Shortcut::Copy
            && let Some(text) = self.copy_text.lock().unwrap().clone()
        {
            self.pasteboard.external_write(&text);
        }
        result
    }

    fn osascript_shortcut(&self, shortcut: Shortcut) -> Result<(), KeySynthError> {
        self.calls
            .lock()
            .unwrap()
            .push(KeyCall::Osascript(shortcut));
        self.osascript_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
    }
}

pub struct FakeWorkspace {
    pub frontmost: Mutex<Option<RunningApp>>,
    pub apps: Mutex<Vec<RunningApp>>,
    pub frontmost_lookups: AtomicUsize,
    pub activations: Mutex<Vec<i32>>,
    /// Frontmost reads that still report the old app after `activate`.
    pub activation_lag: AtomicUsize,
    pub pending_activation: Mutex<Option<i32>>,
    /// Blocks `frontmost_app` until released (shared-lookup tests).
    pub gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl FakeWorkspace {
    pub fn new(frontmost: Option<RunningApp>) -> Arc<Self> {
        Arc::new(FakeWorkspace {
            frontmost: Mutex::new(frontmost.clone()),
            apps: Mutex::new(frontmost.into_iter().collect()),
            frontmost_lookups: AtomicUsize::new(0),
            activations: Mutex::new(Vec::new()),
            activation_lag: AtomicUsize::new(0),
            pending_activation: Mutex::new(None),
            gate: Mutex::new(None),
        })
    }

    pub fn app(pid: i32, name: &str) -> RunningApp {
        RunningApp {
            pid,
            name: Some(name.to_string()),
            executable_path: Some(format!("/Applications/{name}.app/Contents/MacOS/{name}")),
        }
    }

    pub fn set_frontmost(&self, app: Option<RunningApp>) {
        if let Some(app) = &app {
            self.add_app(app.clone());
        }
        *self.frontmost.lock().unwrap() = app;
    }

    pub fn add_app(&self, app: RunningApp) {
        let mut apps = self.apps.lock().unwrap();
        apps.retain(|a| a.pid != app.pid);
        apps.push(app);
    }
}

impl Workspace for FakeWorkspace {
    fn frontmost_app(&self) -> Option<RunningApp> {
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.recv();
        }
        self.frontmost_lookups.fetch_add(1, Ordering::SeqCst);
        let pending = *self.pending_activation.lock().unwrap();
        if let Some(pid) = pending {
            if self.activation_lag.load(Ordering::SeqCst) == 0 {
                let app = self.app_for_pid(pid);
                *self.frontmost.lock().unwrap() = app;
                *self.pending_activation.lock().unwrap() = None;
            } else {
                self.activation_lag.fetch_sub(1, Ordering::SeqCst);
            }
        }
        self.frontmost.lock().unwrap().clone()
    }

    fn app_for_pid(&self, pid: i32) -> Option<RunningApp> {
        self.apps
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.pid == pid)
            .cloned()
    }

    fn activate(&self, pid: i32) -> bool {
        self.activations.lock().unwrap().push(pid);
        *self.pending_activation.lock().unwrap() = Some(pid);
        true
    }
}

pub struct FakeReader {
    pub native: Mutex<VecDeque<NativeRead>>,
    pub applescript: Mutex<VecDeque<AxSelection>>,
    pub native_calls: AtomicUsize,
    pub applescript_calls: AtomicUsize,
}

impl FakeReader {
    pub fn new() -> Arc<Self> {
        Arc::new(FakeReader {
            native: Mutex::new(VecDeque::new()),
            applescript: Mutex::new(VecDeque::new()),
            native_calls: AtomicUsize::new(0),
            applescript_calls: AtomicUsize::new(0),
        })
    }

    pub fn queue_native(&self, reads: impl IntoIterator<Item = NativeRead>) {
        self.native.lock().unwrap().extend(reads);
    }

    pub fn queue_applescript(&self, reads: impl IntoIterator<Item = AxSelection>) {
        self.applescript.lock().unwrap().extend(reads);
    }
}

impl SelectionReader for FakeReader {
    fn read_native(&self, _pid: i32, _timeout: Duration) -> NativeRead {
        self.native_calls.fetch_add(1, Ordering::SeqCst);
        self.native
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(NativeRead::Read(AxSelection::None))
    }

    fn read_applescript(&self, _pid: i32, _timeout: Duration) -> AxSelection {
        self.applescript_calls.fetch_add(1, Ordering::SeqCst);
        self.applescript
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(AxSelection::Unavailable)
    }
}

/// Every fake, wired into one layer.
pub struct Harness {
    pub clock: Arc<FakeClock>,
    pub pasteboard: Arc<FakePasteboard>,
    pub accessibility: Arc<FakeAccessibility>,
    pub keys: Arc<FakeKeys>,
    pub workspace: Arc<FakeWorkspace>,
    pub reader: Arc<FakeReader>,
    pub denials: Arc<AtomicUsize>,
    pub layer: InputLayer,
}

impl Harness {
    pub fn new() -> Self {
        Self::with_frontmost(Some(FakeWorkspace::app(4242, "TextEdit")))
    }

    pub fn with_frontmost(frontmost: Option<RunningApp>) -> Self {
        let clock = FakeClock::new();
        let pasteboard = FakePasteboard::new();
        let accessibility = FakeAccessibility::new(true);
        let keys = FakeKeys::new(pasteboard.clone());
        let workspace = FakeWorkspace::new(frontmost);
        let reader = FakeReader::new();
        let denials = Arc::new(AtomicUsize::new(0));
        let counter = denials.clone();
        let layer = InputLayer::new(InputPorts {
            pasteboard: pasteboard.clone(),
            accessibility: accessibility.clone(),
            keys: keys.clone(),
            workspace: workspace.clone(),
            reader: reader.clone(),
            clock: clock.clone(),
        })
        .with_accessibility_denied_handler(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        Harness {
            clock,
            pasteboard,
            accessibility,
            keys,
            workspace,
            reader,
            denials,
            layer,
        }
    }
}
