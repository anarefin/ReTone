//! The native hotkey listener:
//! a listen-only CGEventTap for `flagsChanged` (Globe/Fn and right-side
//! modifiers) plus, only while a mouse-button hotkey needs it, an active tap
//! that can swallow side buttons 4/5. Both run on one dedicated CFRunLoop
//! thread; a tap macOS disables on timeout is re-enabled.
//!
//! Creating a tap needs Accessibility (or Input Monitoring); without it
//! [`MacEventTap::start`] fails and the caller reports it.

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
    kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType,
};
use retone_core::hotkey::native::{
    EventTapPort, FlagsTracker, ModifierFlags, NativeKeyEvent, mouse_button_name,
};
use std::ffi::c_void;
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

/// Receives every event. Called on the tap thread: keep it quick (a slow tap
/// is disabled by macOS).
pub type EventSink = Arc<dyn Fn(NativeKeyEvent) + Send + Sync>;

const START_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the tap thread's run loop checks for a stop request.
const RUN_SLICE_SECONDS: f64 = 0.5;
const MOUSE_BUTTON_4: u8 = 1;
const MOUSE_BUTTON_5: u8 = 2;

struct Shared {
    sink: EventSink,
    tracker: Mutex<FlagsTracker>,
    /// Side buttons to swallow (bit flags).
    suppressed: AtomicU8,
    keyboard_port: AtomicPtr<CFMachPort>,
    mouse_port: AtomicPtr<CFMachPort>,
    stopping: AtomicBool,
}

/// A CF object handed between threads. CFRunLoop's stop/wake/add/remove-source
/// and CFMachPort/CGEventTapEnable are documented thread-safe; retain/release
/// are atomic.
struct SendCf<T: ?Sized>(CFRetained<T>);
// SAFETY: see the type's documentation; only thread-safe calls are made.
unsafe impl<T: ?Sized> Send for SendCf<T> {}

struct TapThread {
    run_loop: SendCf<CFRunLoop>,
    handle: JoinHandle<()>,
}

struct MouseTap {
    port: SendCf<CFMachPort>,
    source: SendCf<CFRunLoopSource>,
}

pub struct MacEventTap {
    shared: Arc<Shared>,
    thread: Option<TapThread>,
    mouse: Option<MouseTap>,
}

fn event_mask(types: &[CGEventType]) -> CGEventMask {
    types.iter().fold(0, |mask, t| mask | (1 << t.0))
}

fn common_modes() -> Option<&'static objc2_core_foundation::CFRunLoopMode> {
    // SAFETY: an immutable CoreFoundation constant.
    unsafe { kCFRunLoopCommonModes }
}

fn reenable(port: &AtomicPtr<CFMachPort>) {
    let port = port.load(Ordering::Acquire);
    if !port.is_null() {
        // SAFETY: the pointer stays valid until the tap is torn down, which
        // happens only after it is cleared here.
        CGEvent::tap_enable(unsafe { &*port }, true);
        tracing::debug!(target: "hotkey", "Re-enabled an event tap macOS disabled");
    }
}

fn is_disabled(event_type: CGEventType) -> bool {
    event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
}

fn modifier_flags(flags: CGEventFlags) -> ModifierFlags {
    ModifierFlags {
        function: flags.contains(CGEventFlags::MaskSecondaryFn),
        command: flags.contains(CGEventFlags::MaskCommand),
        option: flags.contains(CGEventFlags::MaskAlternate),
        control: flags.contains(CGEventFlags::MaskControl),
        shift: flags.contains(CGEventFlags::MaskShift),
    }
}

fn deliver(shared: &Shared, events: impl IntoIterator<Item = NativeKeyEvent>) {
    for event in events {
        let sink = shared.sink.clone();
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(event))).is_err() {
            tracing::error!(target: "hotkey", ?event, "Native key event handler panicked");
        }
    }
}

unsafe extern "C-unwind" fn keyboard_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `user_info` is the `Shared` the tap was created with, kept alive
    // by `MacEventTap` until the tap is invalidated.
    let shared = unsafe { &*(user_info as *const Shared) };
    if is_disabled(event_type) {
        reenable(&shared.keyboard_port);
    } else if event_type == CGEventType::FlagsChanged {
        // SAFETY: CoreGraphics passes a valid event for the callback's duration.
        let event_ref = unsafe { event.as_ref() };
        let key_code =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode);
        let flags = modifier_flags(CGEvent::flags(Some(event_ref)));
        let events = shared
            .tracker
            .lock()
            .map(|mut t| t.on_flags_changed(key_code as u16, flags))
            .unwrap_or_default();
        deliver(shared, events);
    }
    event.as_ptr()
}

unsafe extern "C-unwind" fn mouse_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: as in `keyboard_callback`.
    let shared = unsafe { &*(user_info as *const Shared) };
    if is_disabled(event_type) {
        reenable(&shared.mouse_port);
        return event.as_ptr();
    }
    let down = event_type == CGEventType::OtherMouseDown;
    if !down && event_type != CGEventType::OtherMouseUp {
        return event.as_ptr();
    }
    // SAFETY: CoreGraphics passes a valid event for the callback's duration.
    let event_ref = unsafe { event.as_ref() };
    let number =
        CGEvent::integer_value_field(Some(event_ref), CGEventField::MouseEventButtonNumber);
    let Some(name) = mouse_button_name(number) else {
        return event.as_ptr();
    };
    deliver(
        shared,
        [if down {
            NativeKeyEvent::MouseButtonDown(name)
        } else {
            NativeKeyEvent::MouseButtonUp(name)
        }],
    );
    let bit = if name == "MouseButton4" {
        MOUSE_BUTTON_4
    } else {
        MOUSE_BUTTON_5
    };
    if shared.suppressed.load(Ordering::Acquire) & bit != 0 {
        return null_mut();
    }
    event.as_ptr()
}

impl MacEventTap {
    pub fn new(sink: EventSink) -> Self {
        Self {
            shared: Arc::new(Shared {
                sink,
                tracker: Mutex::new(FlagsTracker::default()),
                suppressed: AtomicU8::new(0),
                keyboard_port: AtomicPtr::new(null_mut()),
                mouse_port: AtomicPtr::new(null_mut()),
                stopping: AtomicBool::new(false),
            }),
            thread: None,
            mouse: None,
        }
    }

    fn user_info(&self) -> *mut c_void {
        Arc::as_ptr(&self.shared) as *mut c_void
    }

    fn remove_mouse_tap(&mut self) {
        let Some(mouse) = self.mouse.take() else {
            return;
        };
        self.shared.mouse_port.store(null_mut(), Ordering::Release);
        CGEvent::tap_enable(&mouse.port.0, false);
        if let Some(thread) = &self.thread {
            thread
                .run_loop
                .0
                .remove_source(Some(&mouse.source.0), common_modes());
        }
        mouse.port.0.invalidate();
    }

    fn add_mouse_tap(&mut self) {
        let Some(thread) = &self.thread else {
            return;
        };
        let mask = event_mask(&[CGEventType::OtherMouseDown, CGEventType::OtherMouseUp]);
        // SAFETY: a valid callback; `user_info` outlives the tap (see `Shared`).
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                mask,
                Some(mouse_callback),
                self.user_info(),
            )
        };
        let Some(port) = port else {
            tracing::warn!(target: "hotkey", "Failed to create mouse event tap");
            return;
        };
        let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
            port.invalidate();
            tracing::warn!(target: "hotkey", "Failed to create mouse event tap source");
            return;
        };
        self.shared
            .mouse_port
            .store(CFRetained::as_ptr(&port).as_ptr(), Ordering::Release);
        thread.run_loop.0.add_source(Some(&source), common_modes());
        thread.run_loop.0.wake_up();
        CGEvent::tap_enable(&port, true);
        self.mouse = Some(MouseTap {
            port: SendCf(port),
            source: SendCf(source),
        });
    }
}

/// The tap thread: creates the keyboard tap, reports the outcome, then runs its
/// run loop until asked to stop.
fn run_tap_thread(shared: usize, ready: mpsc::Sender<Result<SendCf<CFRunLoop>, String>>) {
    // SAFETY: the `Shared` behind this address outlives the thread (joined in stop).
    let shared_ref = unsafe { &*(shared as *const Shared) };
    let mask = event_mask(&[CGEventType::FlagsChanged]);
    // SAFETY: a valid callback; `shared` outlives the tap.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            mask,
            Some(keyboard_callback),
            shared as *mut c_void,
        )
    };
    let Some(port) = port else {
        let _ = ready.send(Err(
            "Failed to create event tap (Accessibility or Input Monitoring not granted)".into(),
        ));
        return;
    };
    let (Some(source), Some(run_loop)) = (
        CFMachPort::new_run_loop_source(None, Some(&port), 0),
        CFRunLoop::current(),
    ) else {
        port.invalidate();
        let _ = ready.send(Err("Failed to attach the event tap to a run loop".into()));
        return;
    };
    run_loop.add_source(Some(&source), common_modes());
    CGEvent::tap_enable(&port, true);
    shared_ref
        .keyboard_port
        .store(CFRetained::as_ptr(&port).as_ptr(), Ordering::Release);
    if ready.send(Ok(SendCf(run_loop.clone()))).is_err() {
        shared_ref.stopping.store(true, Ordering::Release);
    }

    // SAFETY: an immutable CoreFoundation constant.
    let default_mode = unsafe { kCFRunLoopDefaultMode };
    while !shared_ref.stopping.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(default_mode, RUN_SLICE_SECONDS, false);
    }

    shared_ref
        .keyboard_port
        .store(null_mut(), Ordering::Release);
    CGEvent::tap_enable(&port, false);
    run_loop.remove_source(Some(&source), common_modes());
    port.invalidate();
}

impl EventTapPort for MacEventTap {
    fn start(&mut self) -> Result<(), String> {
        if self.thread.is_some() {
            return Ok(());
        }
        self.shared.stopping.store(false, Ordering::Release);
        if let Ok(mut tracker) = self.shared.tracker.lock() {
            *tracker = FlagsTracker::default();
        }
        let (ready, outcome) = mpsc::channel();
        let shared = Arc::as_ptr(&self.shared) as usize;
        let handle = std::thread::Builder::new()
            .name("retone-event-tap".into())
            .spawn(move || run_tap_thread(shared, ready))
            .map_err(|e| format!("Failed to start the event tap thread: {e}"))?;
        match outcome.recv_timeout(START_TIMEOUT) {
            Ok(Ok(run_loop)) => {
                self.thread = Some(TapThread { run_loop, handle });
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(_) => {
                // The thread is stuck creating the tap; tell it to stop once it
                // gets there and leave it detached.
                self.shared.stopping.store(true, Ordering::Release);
                Err("Timed out creating the event tap".into())
            }
        }
    }

    fn stop(&mut self) {
        self.remove_mouse_tap();
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.shared.stopping.store(true, Ordering::Release);
        thread.run_loop.0.stop();
        thread.run_loop.0.wake_up();
        if thread.handle.join().is_err() {
            tracing::warn!(target: "hotkey", "Event tap thread panicked");
        }
    }

    fn is_running(&self) -> bool {
        self.thread.is_some()
    }

    fn set_mouse_buttons(&mut self, buttons: &[String]) {
        let bits = buttons.iter().fold(0u8, |bits, b| match b.as_str() {
            "MouseButton4" => bits | MOUSE_BUTTON_4,
            "MouseButton5" => bits | MOUSE_BUTTON_5,
            _ => bits,
        });
        self.shared.suppressed.store(bits, Ordering::Release);
        // The active tap can swallow side buttons and is Accessibility-
        // protected, so it exists only while a mouse-button hotkey needs it.
        if bits == 0 {
            self.remove_mouse_tap();
        } else if self.mouse.is_none() {
            self.add_mouse_tap();
        }
    }
}

impl Drop for MacEventTap {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_modifier_flags() {
        let flags = CGEventFlags::MaskSecondaryFn | CGEventFlags::MaskCommand;
        let decoded = modifier_flags(flags);
        assert!(decoded.function && decoded.command);
        assert!(!decoded.option && !decoded.control && !decoded.shift);
    }

    #[test]
    fn masks_cover_the_requested_event_types() {
        assert_eq!(event_mask(&[CGEventType::FlagsChanged]), 1 << 12);
        assert_eq!(
            event_mask(&[CGEventType::OtherMouseDown, CGEventType::OtherMouseUp]),
            (1 << 25) | (1 << 26)
        );
    }

    #[test]
    fn a_stopped_tap_is_not_running_and_stop_is_idempotent() {
        let mut tap = MacEventTap::new(Arc::new(|_| {}));
        assert!(!tap.is_running());
        tap.set_mouse_buttons(&["MouseButton4".into()]);
        assert!(
            tap.mouse.is_none(),
            "no mouse tap without a running listener"
        );
        tap.stop();
        tap.stop();
    }

    /// Creates the real taps (needs Accessibility for the host process), then
    /// tears them down. No events are synthesized.
    #[test]
    #[ignore = "creates a real CGEventTap"]
    fn smoke_starts_and_stops_the_real_taps() {
        let mut tap = MacEventTap::new(Arc::new(|_| {}));
        match tap.start() {
            Ok(()) => {
                assert!(tap.is_running());
                tap.set_mouse_buttons(&["MouseButton4".into()]);
                eprintln!("mouse tap created: {}", tap.mouse.is_some());
                tap.set_mouse_buttons(&[]);
                assert!(tap.mouse.is_none());
                tap.stop();
                assert!(!tap.is_running());
                eprintln!("event tap started and stopped");
            }
            Err(error) => eprintln!("event tap unavailable here: {error}"),
        }
    }
}
