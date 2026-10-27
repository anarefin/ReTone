//! The selection side of the flow over the macOS input layer: target capture,
//! selection sessions, select-all, refocus and replace. Every clipboard step
//! runs inside the app's clipboard-operation lock.

use super::controller::SelectionSide;
use crate::app::AppState;
use retone_core::payload::{Capture, CapturePort};
use retone_core::session::{ReplaceError, SessionStore};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub struct TauriSelection {
    app: AppHandle,
    sessions: Arc<SessionStore>,
}

impl TauriSelection {
    pub fn new(app: AppHandle) -> Self {
        TauriSelection {
            app,
            sessions: Arc::new(SessionStore::new()),
        }
    }

    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }
}

impl CapturePort for TauriSelection {
    async fn capture_target_pid(&self) -> Option<i32> {
        self.state().input.capture_target().await
    }

    fn is_dormant_tree(&self, pid: i32) -> bool {
        self.state()
            .input
            .layer()
            .is_native_selection_unsupported(pid)
    }

    async fn capture_selected_text(&self) -> Capture {
        let sessions = self.sessions.clone();
        // Nothing selected means the whole field may get selected, so an
        // unreliable "none" is confirmed with a copy first.
        self.state()
            .input
            .run_clipboard_operation(move |layer| sessions.capture(layer, true))
            .await
    }

    async fn select_all(&self) -> Result<(), String> {
        self.state()
            .input
            .select_all(None)
            .await
            .map_err(|error| error.as_str().to_string())
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

impl SelectionSide for TauriSelection {
    /// Showing the picker activates ReTone. Hiding it while the app is still
    /// active can bring another of its windows forward, so the target app is
    /// made frontmost first.
    async fn activate_target(&self) {
        let input = &self.state().input;
        let Some(pid) = input.last_target_pid() else {
            return;
        };
        if !input.activate_pid(pid).await {
            tracing::debug!(target: "tone-rewrite", pid, "Tone rewrite could not refocus target");
        }
    }

    async fn replace(&self, session_id: String, text: String) -> Result<(), ReplaceError> {
        let sessions = self.sessions.clone();
        self.state()
            .input
            .run_clipboard_operation(move |layer| sessions.replace(layer, &session_id, &text, true))
            .await
    }

    fn write_clipboard(&self, text: &str) {
        if let Err(error) = self.state().input.write_clipboard(text) {
            tracing::warn!(target: "tone-rewrite", %error, "Could not copy the rewrite");
        }
    }

    fn cancel_tagged(&self, prefix: &str) {
        // A closed picker drops its rewrite: abort the local request it tagged.
        self.state().local.bridge.cancel_tagged(prefix);
    }
}
