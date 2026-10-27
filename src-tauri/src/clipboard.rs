//! The macOS input layer as the app uses it.
//!
//! [`InputService`] owns one [`InputLayer`] over the real OS ports and the
//! clipboard-operation lock. Every clipboard-touching operation (selection
//! read, synthetic copy, paste with restore, select-all) runs through
//! [`InputService::run_clipboard_operation`]: one at a time, each on a blocking
//! thread, and a paste holds the lock until its clipboard restore is done.
//!
//! The ReTone flow (`tone_rewrite::selection`) calls this API.

use crate::i18n;
use retone_core::input::{
    InputLayer, PasteError, PasteOptions, PasteOutcome, ReadOptions, SelectAllError, SelectionRead,
};
use retone_macos::process::run_with_timeout;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
/// The denial dialog waits for the user; this only bounds a stuck osascript.
const DIALOG_TIMEOUT: Duration = Duration::from_secs(600);

pub struct InputService {
    layer: Arc<InputLayer>,
    operation: tokio::sync::Mutex<()>,
}

impl Default for InputService {
    fn default() -> Self {
        Self::new()
    }
}

impl InputService {
    /// The input layer over the real macOS ports. The denial dialog is shown
    /// from non-silent Accessibility checks.
    pub fn new() -> Self {
        let layer = InputLayer::new(retone_macos::input_ports())
            .with_accessibility_denied_handler(show_accessibility_dialog);
        Self::with_layer(layer)
    }

    pub fn with_layer(layer: InputLayer) -> Self {
        InputService {
            layer: Arc::new(layer),
            operation: tokio::sync::Mutex::new(()),
        }
    }

    pub fn layer(&self) -> &Arc<InputLayer> {
        &self.layer
    }

    /// Runs `operation` on a blocking thread while holding the clipboard lock,
    /// so clipboard work never interleaves. Operations run in call order.
    pub async fn run_clipboard_operation<R, F>(&self, operation: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&InputLayer) -> R + Send + 'static,
    {
        let _guard = self.operation.lock().await;
        self.run_blocking(operation).await
    }

    async fn run_blocking<R, F>(&self, operation: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&InputLayer) -> R + Send + 'static,
    {
        let layer = self.layer.clone();
        match tokio::task::spawn_blocking(move || operation(&layer)).await {
            Ok(result) => result,
            Err(error) => std::panic::resume_unwind(error.into_panic()),
        }
    }

    /// Captures the frontmost app at hotkey time (reused for 250 ms). Not
    /// serialized: it doesn't touch the clipboard.
    pub async fn capture_target(&self) -> Option<i32> {
        self.run_blocking(|layer| layer.capture_target()).await
    }

    pub fn last_target_pid(&self) -> Option<i32> {
        self.layer.last_target_pid()
    }

    /// Brings `pid` frontmost (no-op if it already is), confirming 6×25 ms.
    pub async fn activate_pid(&self, pid: i32) -> bool {
        self.run_blocking(move |layer| layer.activate_pid(pid))
            .await
    }

    // Part of the input API; the ReTone flow composes these steps under one
    // lock through `run_clipboard_operation` instead.
    #[allow(dead_code)]
    /// The target's selection: Accessibility, then a synthetic ⌘C. Serialized.
    pub async fn read_selection(&self, pid: Option<i32>, options: ReadOptions) -> SelectionRead {
        self.run_clipboard_operation(move |layer| layer.read_selection(pid, options))
            .await
    }

    // Part of the input API; the ReTone flow composes these steps under one
    // lock through `run_clipboard_operation` instead.
    #[allow(dead_code)]
    /// Pastes `text` into the frontmost app and restores the clipboard.
    /// Serialized; returns after the restore.
    pub async fn paste_text(
        &self,
        text: String,
        options: PasteOptions,
    ) -> Result<PasteOutcome, PasteError> {
        self.run_clipboard_operation(move |layer| layer.paste_text(&text, options))
            .await
    }

    /// ⌘A in the target (terminals declined). Serialized.
    pub async fn select_all(&self, pid: Option<i32>) -> Result<(), SelectAllError> {
        self.run_clipboard_operation(move |layer| layer.select_all(pid))
            .await
    }

    /// A plain clipboard write (the renderer's copy buttons, and the rewrite
    /// left behind when a paste is held back).
    pub fn write_clipboard(&self, text: &str) -> Result<(), PasteError> {
        self.layer.write_clipboard(text)
    }

    pub fn check_accessibility(&self, silent: bool) -> bool {
        self.layer.check_accessibility(silent)
    }

    // No renderer command prompts (dropped in phase 1); kept for phase 7.
    #[allow(dead_code)]
    pub fn prompt_accessibility(&self) -> bool {
        self.layer.prompt_accessibility()
    }

    /// Silent check run when a hotkey is saved, so the first paste is fast.
    pub fn pre_warm_accessibility(&self) {
        let trusted = self.layer.pre_warm_accessibility();
        tracing::debug!(target: "clipboard", trusted, "Accessibility pre-warmed");
    }
}

/// An AppleScript string literal.
pub fn applescript_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The denial dialog's script, in the UI language.
pub fn accessibility_dialog_script() -> String {
    let title = applescript_string(&i18n::t("reTone.accessibility.title"));
    let message = applescript_string(&i18n::t("reTone.accessibility.description"));
    let cancel = applescript_string(&i18n::t("common.cancel"));
    let action = applescript_string(&i18n::t("reTone.accessibility.action"));
    format!(
        "display dialog {message} with title {title} buttons {{{cancel}, {action}}} default button {action} cancel button {cancel} with icon caution"
    )
}

/// Explains the missing Accessibility permission and offers to open its
/// settings. Runs on its own thread; the caller doesn't wait.
fn show_accessibility_dialog() {
    let script = accessibility_dialog_script();
    let spawned = std::thread::Builder::new()
        .name("accessibility-dialog".into())
        .spawn(move || {
            tracing::info!(target: "clipboard", "Accessibility not granted; showing the dialog");
            match run_with_timeout("/usr/bin/osascript", &["-e", &script], DIALOG_TIMEOUT) {
                Ok(output) if output.code == Some(0) => open_accessibility_settings(),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(target: "clipboard", %error, "Accessibility dialog failed")
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(target: "clipboard", %error, "Could not show the Accessibility dialog");
    }
}

/// Opens Privacy & Security → Accessibility, falling back to System Settings.
pub fn open_accessibility_settings() {
    let attempts: [&[&str]; 3] = [
        &[ACCESSIBILITY_SETTINGS_URL],
        &["-b", "com.apple.systempreferences"],
        &["/System/Library/PreferencePanes/Security.prefPane"],
    ];
    for args in attempts {
        if Command::new("/usr/bin/open")
            .args(args)
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }
    }
    tracing::warn!(target: "clipboard", "Could not open the Accessibility settings");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn applescript_strings_escape_quotes_and_backslashes() {
        assert_eq!(
            applescript_string(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn the_dialog_is_localized() {
        i18n::change_language("en");
        let script = accessibility_dialog_script();
        assert!(script.starts_with("display dialog \"ReTone needs Accessibility access"));
        assert!(script.contains("with title \"Accessibility access needed\""));
        assert!(script.contains(
            "buttons {\"Cancel\", \"Open Accessibility Settings\"} default button \"Open Accessibility Settings\""
        ));
    }

    // A paste waits for the prior clipboard restore: a second operation starts
    // only after the first (including its restore) finished.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clipboard_operations_run_one_at_a_time_in_order() {
        let service = Arc::new(InputService::new());
        let events = Arc::new(Mutex::new(Vec::new()));

        let first = {
            let (service, events) = (service.clone(), events.clone());
            tokio::spawn(async move {
                service
                    .run_clipboard_operation(move |_| {
                        events.lock().unwrap().push("start:first");
                        std::thread::sleep(Duration::from_millis(100));
                        events.lock().unwrap().push("end:first");
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = {
            let (service, events) = (service.clone(), events.clone());
            tokio::spawn(async move {
                service
                    .run_clipboard_operation(move |_| {
                        events.lock().unwrap().push("start:second");
                        events.lock().unwrap().push("end:second");
                    })
                    .await
            })
        };
        first.await.unwrap();
        second.await.unwrap();

        assert_eq!(
            *events.lock().unwrap(),
            vec!["start:first", "end:first", "start:second", "end:second"]
        );
    }

    #[tokio::test]
    async fn a_panicking_operation_releases_the_lock() {
        let service = InputService::new();
        let service = Arc::new(service);
        let failing = {
            let service = service.clone();
            tokio::spawn(async move { service.run_clipboard_operation(|_| panic!("boom")).await })
        };
        assert!(failing.await.is_err());
        assert_eq!(service.run_clipboard_operation(|_| 7).await, 7);
    }
}
