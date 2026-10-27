//! Error alerts: startup failures, links that would not open, a settings
//! page that never loaded. Shown with `osascript`, which works before the
//! event loop runs and from any thread; the alert's single OK button is the
//! system's own, so it is already localized.

use crate::clipboard::applescript_string;
use retone_macos::process::run_with_timeout;
use std::time::Duration;

/// Bounds a stuck `osascript`; the alert itself waits for the user.
const ALERT_TIMEOUT: Duration = Duration::from_secs(600);

/// `display alert` with a critical icon.
pub fn error_alert_script(title: &str, message: &str) -> String {
    format!(
        "display alert {} message {} as critical",
        applescript_string(title),
        applescript_string(message)
    )
}

fn run_alert(script: &str) {
    match run_with_timeout("/usr/bin/osascript", &["-e", script], ALERT_TIMEOUT) {
        Ok(output) if output.code == Some(0) => {}
        Ok(output) => {
            tracing::warn!(target: "app", code = ?output.code, "Error alert exited abnormally")
        }
        Err(error) => tracing::warn!(target: "app", %error, "Error alert failed"),
    }
}

/// Shows an error alert on its own thread; the caller doesn't wait.
pub fn show_error(title: String, message: String) {
    let script = error_alert_script(&title, &message);
    let spawned = std::thread::Builder::new()
        .name("error-alert".into())
        .spawn(move || run_alert(&script));
    if let Err(error) = spawned {
        tracing::warn!(target: "app", %error, "Could not show an error alert");
    }
}

/// Shows an error alert and waits until it is dismissed (startup failures,
/// right before the process exits).
pub fn show_error_and_wait(title: &str, message: &str) {
    run_alert(&error_alert_script(title, message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_alert_quotes_its_text() {
        assert_eq!(
            error_alert_script("ReTone \"Startup\" Error", "a\\b"),
            r#"display alert "ReTone \"Startup\" Error" message "a\\b" as critical"#
        );
    }
}
