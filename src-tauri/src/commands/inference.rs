//! Model calls that run in the main process: the local llama-server bridge
//! (one request at a time, cancel by request id) and the Anthropic proxy
//! (`anthropic.rs`).

use crate::anthropic::{AnthropicCall, DEFAULT_TIMEOUT};
use crate::app::AppState;
use retone_core::providers::anthropic::MESSAGES_URL;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

#[tauri::command]
pub async fn process_local_reasoning(
    app: AppHandle,
    text: String,
    model_id: String,
    agent_name: Option<String>,
    config: Value,
) -> Value {
    let _ = agent_name;
    let local = app.state::<AppState>().local.clone();
    match local.bridge.process_text(&text, &model_id, &config).await {
        Ok(text) => json!({ "success": true, "text": text }),
        // code/details carry the failure to the renderer, which translates it.
        Err(error) => error.to_ipc(),
    }
}

/// Aborts the local request tagged with `request_id`, if it is still running.
#[tauri::command]
pub fn cancel_local_reasoning(app: AppHandle, request_id: String) {
    app.state::<AppState>().local.bridge.cancel(&request_id);
}

#[tauri::command]
pub async fn check_local_reasoning_available(app: AppHandle) -> bool {
    let local = app.state::<AppState>().local.clone();
    tauri::async_runtime::spawn_blocking(move || local.bridge.is_available())
        .await
        .unwrap_or(false)
}

/// Anthropic's API refuses browser-origin requests, so the call runs here.
#[tauri::command]
pub async fn process_anthropic_reasoning(
    app: AppHandle,
    text: String,
    model_id: String,
    agent_name: Option<String>,
    config: Value,
) -> Value {
    let _ = agent_name;
    let state = app.state::<AppState>();
    let api_key = state.secrets.get("ANTHROPIC_API_KEY");
    AnthropicCall {
        client: &state.http.no_redirect,
        url: MESSAGES_URL,
        api_key: &api_key,
        timeout: DEFAULT_TIMEOUT,
    }
    .run(&text, &model_id, &config)
    .await
}
