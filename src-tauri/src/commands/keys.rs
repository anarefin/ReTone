//! BYOK keys (Keychain-backed, see `secrets.rs`) and the provider connection
//! check (`retone_core::providers::connection_test`).

use crate::app::AppState;
use crate::http::ReqwestTransport;
use crate::secrets::spec_for_env;
use retone_core::providers::connection_test::{
    PROBE_TIMEOUT, test_provider_connection as run_check,
};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, EventTarget, Manager, State, WebviewWindow};

fn get_key(state: &AppState, env_name: &str) -> String {
    state.secrets.get(env_name)
}

async fn save_key(
    app: AppHandle,
    sender_label: String,
    env_name: &'static str,
    key: String,
) -> Value {
    let store_key = spec_for_env(env_name)
        .map(|s| s.store_key)
        .unwrap_or_default();
    let worker = app.clone();
    // The Keychain call can block (or prompt), so it runs off the async workers.
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        worker.state::<AppState>().secrets.save(env_name, &key)
    })
    .await;
    let outcome = match outcome {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => return json!({ "success": false, "error": error }),
        Err(error) => return json!({ "success": false, "error": error.to_string() }),
    };
    if let Some(error) = &outcome.persist_error {
        // The key still applies for this session.
        tracing::error!(target: "environment", key = env_name, %error, "Failed to persist secret");
    }
    if outcome.changed {
        // Notify the other windows by setting name only; the editor's pending
        // input is left alone.
        let _ = app.emit_filter("api-key-updated", store_key, |target| match target {
            EventTarget::WebviewWindow { label }
            | EventTarget::Webview { label }
            | EventTarget::Window { label } => *label != sender_label,
            _ => false,
        });
    }
    json!({ "success": true })
}

macro_rules! byok_key_commands {
    ($($get:ident, $save:ident => $env:literal;)*) => {
        $(
            #[tauri::command]
            pub fn $get(state: State<'_, AppState>) -> String {
                get_key(&state, $env)
            }

            #[tauri::command]
            pub async fn $save(app: AppHandle, window: WebviewWindow, key: String) -> Value {
                save_key(app, window.label().to_string(), $env, key).await
            }
        )*
    };
}

byok_key_commands! {
    get_openai_key, save_openai_key => "OPENAI_API_KEY";
    get_anthropic_key, save_anthropic_key => "ANTHROPIC_API_KEY";
    get_gemini_key, save_gemini_key => "GEMINI_API_KEY";
    get_groq_key, save_groq_key => "GROQ_API_KEY";
    get_openrouter_key, save_openrouter_key => "OPENROUTER_API_KEY";
    get_cleanup_custom_key, save_cleanup_custom_key => "CUSTOM_CLEANUP_API_KEY";
}

/// Every panel showing the same credentials and model shares one answer; a
/// Connected answer is reused for ten minutes.
#[tauri::command]
pub async fn test_provider_connection(app: AppHandle, config: Value) -> Value {
    let cache = app.state::<AppState>().connection_checks.clone();
    let check_config = config.clone();
    let outcome = cache
        .check(&config, move || async move {
            let state = app.state::<AppState>();
            let transport = ReqwestTransport {
                clients: &state.http,
                timeout: PROBE_TIMEOUT,
            };
            let result = run_check(&check_config, &transport, |name| state.env.get(name)).await;
            let json = result.to_json();
            let provider = check_config
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("");
            let error_code = json.get("errorCode").and_then(Value::as_str).unwrap_or("");
            tracing::debug!(
                target: "providers",
                provider,
                success = result.is_success(),
                error_code,
                "Connection check"
            );
            Ok(json)
        })
        .await;
    outcome.unwrap_or_else(|_| json!({ "success": false }))
}
