//! Hotkey wiring: the `toneRewrite` slot over `tauri-plugin-global-shortcut`,
//! the CGEventTap listener for Globe/Fn, right-side modifiers and mouse
//! buttons, hotkey capture (listening mode) and the startup restore.
//!
//! Threading: hotkey configuration runs on the main thread (sync commands,
//! setup, teardown), where the global-shortcut plugin and the Globe preference
//! calls run inline. Shortcut presses arrive on the main thread; tap events are
//! queued from the tap thread to a dispatcher thread, which reads a snapshot of
//! the slot's hotkeys and never takes the manager lock.

use crate::clipboard::{applescript_string, open_accessibility_settings};
use crate::env::TONE_REWRITE_KEY;
use crate::{i18n, tone_rewrite, windows};
use retone_core::hotkey::accelerator::parse_accelerator;
use retone_core::hotkey::globe_preference::{FileMarker, GlobePreference};
use retone_core::hotkey::kinds::is_globe_like_hotkey;
use retone_core::hotkey::native::{ListenerConfig, NativeKeyEvent, NativeListener};
use retone_core::hotkey::repeat_gate::RepeatGate;
use retone_core::hotkey::{
    HotkeyManager, PressHandler, RegisterError, RegisterOutcome, ShortcutBackend, SlotCallback,
    TONE_REWRITE_SLOT, Translate,
};
use retone_macos::event_tap::MacEventTap;
use retone_macos::fn_usage::MacFnUsage;
use retone_macos::process::run_with_timeout;
use serde_json::{Value, json};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// The picker is preloaded this long after launch when a hotkey is saved.
const PREWARM_DELAY: Duration = Duration::from_secs(5);
const GLOBE_PREFERENCE_FILE: &str = "globe-preference-state.json";
const DIALOG_TIMEOUT: Duration = Duration::from_secs(600);

type Listener = NativeListener<MacEventTap, MacFnUsage, FileMarker>;

/// Global shortcuts through `tauri-plugin-global-shortcut`. Accelerator
/// strings are parsed by `retone_core::hotkey::accelerator`.
pub struct TauriShortcutBackend {
    app: AppHandle,
    registered: Vec<(String, Shortcut)>,
}

impl TauriShortcutBackend {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            registered: Vec::new(),
        }
    }
}

/// An accelerator string as a global-shortcut key combination.
pub fn to_shortcut(accelerator: &str) -> Result<Shortcut, RegisterError> {
    let parsed =
        parse_accelerator(accelerator).map_err(|e| RegisterError::Invalid(e.to_string()))?;
    let code = Code::from_str(parsed.code)
        .map_err(|_| RegisterError::Invalid(format!("unsupported key {}", parsed.code)))?;
    let mut modifiers = Modifiers::empty();
    let m = parsed.modifiers;
    modifiers.set(Modifiers::SUPER, m.command);
    modifiers.set(Modifiers::CONTROL, m.control);
    modifiers.set(Modifiers::ALT, m.alt);
    modifiers.set(Modifiers::SHIFT, m.shift);
    Ok(Shortcut::new(Some(modifiers), code))
}

impl ShortcutBackend for TauriShortcutBackend {
    fn register(&mut self, accelerator: &str, on_press: PressHandler) -> Result<(), RegisterError> {
        let shortcut = to_shortcut(accelerator)?;
        if self.registered.iter().any(|(_, s)| s.id() == shortcut.id()) {
            return Err(RegisterError::Refused);
        }
        let on_press: Arc<dyn Fn() + Send + Sync> = Arc::from(on_press);
        self.app
            .global_shortcut()
            .on_shortcut(shortcut, move |_app, _shortcut, event| {
                if event.state == ShortcutState::Pressed {
                    on_press();
                }
            })
            .map_err(|error| {
                tracing::warn!(target: "hotkey", accelerator, %error, "Global shortcut refused");
                RegisterError::Refused
            })?;
        self.registered.push((accelerator.to_string(), shortcut));
        Ok(())
    }

    fn unregister(&mut self, accelerator: &str) {
        let Some(index) = self.registered.iter().position(|(a, _)| a == accelerator) else {
            return;
        };
        let (_, shortcut) = self.registered.remove(index);
        if let Err(error) = self.app.global_shortcut().unregister(shortcut) {
            tracing::debug!(target: "hotkey", accelerator, %error, "Unregister failed");
        }
    }

    fn is_registered(&self, accelerator: &str) -> bool {
        to_shortcut(accelerator)
            .is_ok_and(|shortcut| self.registered.iter().any(|(_, s)| s.id() == shortcut.id()))
    }

    fn unregister_all(&mut self) {
        for (accelerator, shortcut) in std::mem::take(&mut self.registered) {
            if let Err(error) = self.app.global_shortcut().unregister(shortcut) {
                tracing::debug!(target: "hotkey", accelerator, %error, "Unregister failed");
            }
        }
    }
}

pub struct HotkeyService {
    app: AppHandle,
    manager: Mutex<HotkeyManager<TauriShortcutBackend>>,
    native: Mutex<Listener>,
    /// The slot's hotkeys, for the tap-event dispatcher.
    snapshot: RwLock<Vec<String>>,
    /// Set while the settings window records a hotkey: no hotkey fires.
    listening: AtomicBool,
    /// The renderer's capture state, for `set_hotkey_listening_mode` idempotence.
    capture_mode: AtomicBool,
    callback: SlotCallback,
    /// The listener-failure dialog is shown once per session.
    native_alert_shown: AtomicBool,
}

fn translate() -> Translate {
    Arc::new(i18n::t_with)
}

impl HotkeyService {
    fn new(app: &AppHandle, events: mpsc::Sender<NativeKeyEvent>) -> Self {
        let user_data = app.state::<crate::app::AppState>().user_data.clone();
        let marker = FileMarker::new(user_data.join(GLOBE_PREFERENCE_FILE));
        let events = Mutex::new(events);
        let tap = MacEventTap::new(Arc::new(move |event| {
            if let Ok(sender) = events.lock() {
                let _ = sender.send(event);
            }
        }));
        let native = NativeListener::new(tap, GlobePreference::new(MacFnUsage, Some(marker)));

        // The hotkey opens the tone picker over the selection. Tap-only, so
        // autorepeat is gated.
        let gate = Mutex::new(RepeatGate::default());
        let epoch = Instant::now();
        let handle = app.clone();
        let callback: SlotCallback = Arc::new(move |_hotkey: &str| {
            let is_press = gate
                .lock()
                .map(|mut g| g.is_press(epoch.elapsed()))
                .unwrap_or(true);
            if is_press {
                send_toggle_tone_rewrite(&handle);
            } else {
                tracing::debug!(target: "hotkey", "Hotkey fire within the repeat window; ignored as autorepeat");
            }
        });

        Self {
            app: app.clone(),
            manager: Mutex::new(HotkeyManager::new(
                TauriShortcutBackend::new(app.clone()),
                translate(),
            )),
            native: Mutex::new(native),
            snapshot: RwLock::new(Vec::new()),
            listening: AtomicBool::new(false),
            capture_mode: AtomicBool::new(false),
            callback,
            native_alert_shown: AtomicBool::new(false),
        }
    }

    pub fn is_listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    fn tone_hotkeys(&self) -> Vec<String> {
        self.snapshot.read().map(|s| s.clone()).unwrap_or_default()
    }

    fn refresh_snapshot(&self, manager: &HotkeyManager<TauriShortcutBackend>) {
        if let Ok(mut snapshot) = self.snapshot.write() {
            *snapshot = manager.slot_hotkeys(TONE_REWRITE_SLOT);
        }
    }

    fn register(&self, hotkey: &str, atomic: bool) -> RegisterOutcome {
        let mut manager = self.manager.lock().expect("hotkey manager lock");
        let result =
            manager.register_slot(TONE_REWRITE_SLOT, hotkey, self.callback.clone(), atomic);
        self.refresh_snapshot(&manager);
        result
    }

    fn native_config(&self) -> ListenerConfig {
        self.manager
            .lock()
            .expect("hotkey manager lock")
            .mac_native_listener_config(&[TONE_REWRITE_SLOT])
    }

    fn needs_native_listener(&self) -> bool {
        self.manager
            .lock()
            .expect("hotkey manager lock")
            .slot_needs_native_listener(TONE_REWRITE_SLOT)
    }

    /// Starts the listener if it isn't running. A failure (no Accessibility) is
    /// logged and explained once; a later call retries.
    fn start_native_listener(&self) {
        let result = self.native.lock().expect("native listener lock").start();
        if let Err(error) = result {
            self.report_native_error(&error);
        }
    }

    fn sync_native_config(&self) {
        let config = self.native_config();
        self.native
            .lock()
            .expect("native listener lock")
            .set_configuration(config);
    }

    /// After any change to the slot: start the listener when a hotkey needs
    /// it, and keep its configuration in line.
    fn on_hotkey_changed(&self) {
        let wanted = self
            .native
            .lock()
            .expect("native listener lock")
            .is_wanted();
        if self.needs_native_listener() || wanted {
            self.sync_native_config();
        }
        if self.needs_native_listener() {
            self.start_native_listener();
        }
    }

    fn report_native_error(&self, error: &str) {
        tracing::error!(target: "hotkey", error, "Native key listener failed");
        if self.native_alert_shown.swap(true, Ordering::SeqCst) {
            return;
        }
        show_native_listener_dialog(error);
    }

    /// `set-hotkey-listening-mode`: while the settings window records a hotkey
    /// every backend is released so nothing intercepts the keys; afterwards
    /// every slot is re-armed.
    pub fn set_listening_mode(&self, enabled: bool) -> Value {
        if self.capture_mode.swap(enabled, Ordering::SeqCst) == enabled {
            return json!({ "success": true, "skipped": true });
        }
        self.listening.store(enabled, Ordering::SeqCst);
        tracing::debug!(target: "hotkey", enabled, "Hotkey listening mode");

        if enabled {
            // Let mouse buttons through so they can be captured, but keep the
            // macOS Globe action down so choosing Globe cannot flash the emoji viewer.
            self.native
                .lock()
                .expect("native listener lock")
                .set_configuration(ListenerConfig::new::<&str>(&[], true));
            self.start_native_listener();
            self.manager
                .lock()
                .expect("hotkey manager lock")
                .suspend_for_capture();
            return json!({ "success": true });
        }

        if self
            .native
            .lock()
            .expect("native listener lock")
            .is_wanted()
        {
            self.sync_native_config();
        }
        let mut manager = self.manager.lock().expect("hotkey manager lock");
        manager.rearm_after_capture();
        self.refresh_snapshot(&manager);
        json!({ "success": true })
    }

    /// `update-tone-rewrite-hotkey`: registers atomically before saving; an
    /// empty hotkey clears the slot.
    pub fn update_tone_rewrite_hotkey(&self, hotkey: &str) -> Value {
        let state = self.app.state::<crate::app::AppState>();
        if hotkey.is_empty() {
            {
                let mut manager = self.manager.lock().expect("hotkey manager lock");
                manager.unregister_slot(TONE_REWRITE_SLOT);
                self.refresh_snapshot(&manager);
            }
            if let Err(error) = state.env.set(TONE_REWRITE_KEY, None) {
                tracing::warn!(target: "hotkey", %error, "Failed to clear the tone rewrite hotkey");
            }
            tone_rewrite::destroy(&self.app);
            self.on_hotkey_changed();
            return json!({ "success": true, "message": "Tone rewrite hotkey cleared" });
        }

        let result = self.register(hotkey, true);
        if !result.success {
            return json!({ "success": false, "message": result.error });
        }
        if let Err(error) = state.env.set(TONE_REWRITE_KEY, Some(hotkey)) {
            tracing::warn!(target: "hotkey", %error, "Failed to save the tone rewrite hotkey");
        }
        tone_rewrite::prewarm(&self.app);
        self.on_hotkey_changed();
        tracing::info!(target: "hotkey", hotkey, "Tone rewrite hotkey updated");
        json!({ "success": true, "message": format!("Tone rewrite hotkey updated to: {hotkey}") })
    }

    /// Restores the saved hotkey at startup and tells the user which saved
    /// hotkeys no longer register.
    fn restore_saved_hotkey(&self) {
        let saved = self
            .app
            .state::<crate::app::AppState>()
            .env
            .tone_rewrite_key();
        if saved.is_empty() {
            return;
        }
        let result = self.register(&saved, false);
        if result.success {
            tracing::info!(target: "hotkey", hotkey = %saved, "Restored tone rewrite hotkey");
            // Load the picker off the startup path so the first press shows it at once.
            let app = self.app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(PREWARM_DELAY);
                tone_rewrite::prewarm(&app);
            });
        } else {
            tracing::warn!(target: "hotkey", hotkey = %saved, "Failed to restore tone rewrite hotkey");
        }
        let notices = self
            .manager
            .lock()
            .expect("hotkey manager lock")
            .restore_failure_notices(&saved, &result);
        for notice in notices {
            let _ = self.app.emit("hotkey-registration-failed", notice);
        }
    }

    /// Releases every hotkey, stops the listener and hands the Globe action back.
    pub fn shutdown(&self) {
        if let Ok(mut manager) = self.manager.lock() {
            manager.unregister_all();
        }
        if let Ok(mut native) = self.native.lock() {
            native.stop();
        }
    }

    fn handle_native_event(&self, event: NativeKeyEvent) {
        match event {
            NativeKeyEvent::GlobeDown => {
                // Forwarded to the settings window for hotkey capture.
                let _ = self
                    .app
                    .emit_to(windows::SETTINGS_LABEL, "globe-key-pressed", ());
                if self.tone_hotkeys().iter().any(|h| is_globe_like_hotkey(h)) {
                    send_toggle_tone_rewrite(&self.app);
                }
            }
            NativeKeyEvent::GlobeUp => {
                let _ = self
                    .app
                    .emit_to(windows::SETTINGS_LABEL, "globe-key-released", ());
            }
            NativeKeyEvent::RightModifierDown(modifier) => {
                if self.tone_hotkeys().iter().any(|h| h == modifier) {
                    send_toggle_tone_rewrite(&self.app);
                }
            }
            NativeKeyEvent::MouseButtonDown(button) => {
                if self.is_listening() {
                    return;
                }
                if self.tone_hotkeys().iter().any(|h| h == button) {
                    send_toggle_tone_rewrite(&self.app);
                }
            }
            NativeKeyEvent::RightModifierUp(_) | NativeKeyEvent::MouseButtonUp(_) => {}
        }
    }
}

/// A hotkey press. Ignored while a hotkey is being recorded.
pub fn send_toggle_tone_rewrite(app: &AppHandle) {
    if app
        .try_state::<HotkeyService>()
        .is_some_and(|service| service.is_listening())
    {
        tracing::debug!(target: "hotkey", "Hotkey pressed while recording a hotkey; ignored");
        return;
    }
    tone_rewrite::toggle(app);
}

/// The dialog shown when the listener cannot start (the
/// `startup.globeHotkey` alert), with a button to the Accessibility settings.
fn show_native_listener_dialog(error: &str) {
    let title = applescript_string(&i18n::t("startup.globeHotkey.title"));
    let message = applescript_string(&format!(
        "{}\n\n{}\n\n{}",
        i18n::t("startup.globeHotkey.message"),
        error,
        i18n::t("reTone.accessibility.description"),
    ));
    let cancel = applescript_string(&i18n::t("common.cancel"));
    let action = applescript_string(&i18n::t("reTone.accessibility.action"));
    let script = format!(
        "display dialog {message} with title {title} buttons {{{cancel}, {action}}} default button {action} cancel button {cancel} with icon caution"
    );
    let spawned = std::thread::Builder::new()
        .name("hotkey-dialog".into())
        .spawn(move || {
            match run_with_timeout("/usr/bin/osascript", &["-e", &script], DIALOG_TIMEOUT) {
                Ok(output) if output.code == Some(0) => open_accessibility_settings(),
                Ok(_) => {}
                Err(error) => tracing::warn!(target: "hotkey", %error, "Listener dialog failed"),
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(target: "hotkey", %error, "Could not show the listener dialog");
    }
}

/// Startup: puts back a Globe preference a crashed run left, restores the
/// saved hotkey, and starts the listener when that hotkey needs it. Runs on the
/// main thread (setup).
pub fn init(app: &AppHandle) {
    let (sender, events) = mpsc::channel::<NativeKeyEvent>();
    app.manage(HotkeyService::new(app, sender));

    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("hotkey-events".into())
        .spawn(move || {
            for event in events {
                if let Some(service) = handle.try_state::<HotkeyService>() {
                    service.handle_native_event(event);
                }
            }
        });
    if let Err(error) = spawned {
        tracing::error!(target: "hotkey", %error, "Could not start the hotkey event thread");
    }

    let service = app.state::<HotkeyService>();
    service
        .native
        .lock()
        .expect("native listener lock")
        .restore_leftover_preference();
    service.restore_saved_hotkey();
    if service.needs_native_listener() {
        service.sync_native_config();
        service.start_native_listener();
    }
}

/// Teardown on quit.
pub fn shutdown(app: &AppHandle) {
    if let Some(service) = app.try_state::<HotkeyService>() {
        service.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators_map_to_global_shortcuts() {
        let shortcut = to_shortcut("Control+Shift+Space").unwrap();
        assert_eq!(
            shortcut,
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
        );
        assert_eq!(
            to_shortcut("Command+Alt+K").unwrap(),
            Shortcut::new(Some(Modifiers::SUPER | Modifiers::ALT), Code::KeyK)
        );
        assert_eq!(to_shortcut("F8").unwrap(), Shortcut::new(None, Code::F8));
        assert_eq!(
            to_shortcut("Control+,").unwrap(),
            Shortcut::new(Some(Modifiers::CONTROL), Code::Comma)
        );
        assert_eq!(
            to_shortcut("Alt+numdec").unwrap(),
            Shortcut::new(Some(Modifiers::ALT), Code::NumpadDecimal)
        );
        // Same key combination, different spelling: same shortcut id.
        assert_eq!(
            to_shortcut("CmdOrCtrl+K").unwrap().id(),
            to_shortcut("Command+K").unwrap().id()
        );
    }

    #[test]
    fn every_key_the_hotkey_input_records_parses() {
        let keys = [
            "`",
            "1",
            "0",
            "-",
            "=",
            "Q",
            "[",
            "]",
            "\\",
            ";",
            "'",
            ",",
            ".",
            "/",
            "Space",
            "Esc",
            "Tab",
            "Enter",
            "Backspace",
            "F1",
            "F12",
            "F13",
            "F24",
            "Up",
            "Down",
            "Left",
            "Right",
            "Insert",
            "Delete",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Pause",
            "Scrolllock",
            "PrintScreen",
            "Numlock",
            "num0",
            "num9",
            "numadd",
            "numsub",
            "nummult",
            "numdiv",
            "numdec",
            "MediaPlayPause",
            "MediaStop",
            "MediaNextTrack",
            "MediaPreviousTrack",
        ];
        for key in keys {
            assert!(to_shortcut(&format!("Control+{key}")).is_ok(), "{key}");
        }
    }

    #[test]
    fn unparsable_accelerators_are_invalid() {
        assert!(matches!(
            to_shortcut("Control+Alt"),
            Err(RegisterError::Invalid(_))
        ));
        assert!(matches!(
            to_shortcut("LeftOption+K"),
            Err(RegisterError::Invalid(_))
        ));
    }
}
