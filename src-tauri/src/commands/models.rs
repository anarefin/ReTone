//! Local models and llama-server. The Vulkan commands return "unsupported":
//! macOS uses Metal.

use crate::app::AppState;
use crate::local::LocalInference;
use crate::windows;
use retone_llama::download_status::StatusUpdate;
use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

const MODEL_TYPE: &str = "llm";
const PROGRESS_EVENT: &str = "model-download-progress";

fn local(app: &AppHandle) -> Arc<LocalInference> {
    app.state::<AppState>().local.clone()
}

/// Download events go to the settings window, like `_sendToSettings`.
fn send_progress(app: &AppHandle, payload: Value) {
    if let Err(error) = app.emit_to(windows::SETTINGS_LABEL, PROGRESS_EVENT, payload) {
        tracing::debug!(target: "models", %error, "Could not send download progress");
    }
}

#[tauri::command]
pub async fn model_get_all(app: AppHandle) -> Vec<Value> {
    let local = local(&app);
    tauri::async_runtime::spawn_blocking(move || local.models.all_models())
        .await
        .unwrap_or_default()
}

#[tauri::command]
pub fn model_get_active_downloads(app: AppHandle) -> Value {
    serde_json::to_value(local(&app).downloads().active_downloads()).unwrap_or(json!([]))
}

#[tauri::command]
pub async fn model_download(app: AppHandle, model_id: String) -> Value {
    let local = local(&app);
    {
        let mut downloads = local.downloads();
        if downloads.has(MODEL_TYPE, &model_id) {
            return json!({
                "success": false,
                "error": "Model is already being downloaded",
                "code": "DOWNLOAD_IN_PROGRESS",
                "details": { "modelId": model_id },
            });
        }
        // Claimed before the manager's own checks.
        downloads.start(MODEL_TYPE, &model_id);
    }

    let report = |progress: f64, downloaded: u64, total: u64| {
        let status = local.downloads().update(
            MODEL_TYPE,
            &model_id,
            StatusUpdate {
                phase: Some("downloading".into()),
                progress: Some(progress),
                downloaded_bytes: Some(downloaded),
                total_bytes: Some(total),
            },
        );
        send_progress(
            &app,
            json!({
                "modelId": model_id,
                "type": "progress",
                "progress": progress,
                "downloadedSize": downloaded,
                "totalSize": total,
                "sequence": status.sequence,
            }),
        );
    };
    let result = local.models.download_model(&model_id, &report).await;
    let finished = local.downloads().finish(MODEL_TYPE, &model_id);
    match result {
        Ok(path) => {
            send_progress(
                &app,
                json!({
                    "modelId": model_id,
                    "type": "complete",
                    "progress": 100,
                    "downloadedSize": finished.as_ref().map(|s| s.downloaded_bytes),
                    "totalSize": finished.as_ref().map(|s| s.total_bytes),
                    "sequence": finished.as_ref().map(|s| s.sequence),
                }),
            );
            json!({ "success": true, "path": path.display().to_string() })
        }
        Err(error) => {
            if error.code != "DOWNLOAD_IN_PROGRESS" {
                send_progress(
                    &app,
                    json!({
                        "modelId": model_id,
                        "type": "error",
                        "error": error.message,
                        "code": error.code,
                        "details": error.details,
                        "sequence": finished.as_ref().map(|s| s.sequence),
                    }),
                );
            }
            error.to_ipc()
        }
    }
}

#[tauri::command]
pub fn model_delete(app: AppHandle, model_id: String) -> Value {
    match local(&app).models.delete_model(&model_id) {
        Ok(()) => json!({ "success": true }),
        Err(error) => error.to_ipc(),
    }
}

#[tauri::command]
pub fn model_cancel_download(app: AppHandle, model_id: String) -> Value {
    json!({ "success": local(&app).models.cancel_download(&model_id) })
}

/// Starts (or reuses) the server for a model: the picker's warm-up and the
/// live preview, which then streams to `http://127.0.0.1:{port}` directly
/// with `Authorization: Bearer {apiKey}` (the server's per-launch key).
#[tauri::command]
pub async fn llama_server_start(app: AppHandle, model_id: String) -> Value {
    match local(&app).models.start_server_for(&model_id).await {
        Ok(endpoint) => json!({
            "success": true,
            "port": endpoint.port,
            "apiKey": &*endpoint.api_key,
        }),
        Err(error) => json!({ "success": false, "error": error }),
    }
}

#[tauri::command]
pub fn llama_server_status(app: AppHandle) -> Value {
    serde_json::to_value(local(&app).server().status())
        .unwrap_or_else(|e| json!({ "available": false, "running": false, "error": e.to_string() }))
}

/// Forgets the detected backend and restarts the previous model.
#[tauri::command]
pub async fn llama_gpu_reset(app: AppHandle) -> Value {
    let local = local(&app);
    let previous = local.models.current_server_model();
    local.server().reset_gpu_detection();
    local.models.stop_server().await;
    if let Some(model_id) = previous {
        let local = local.clone();
        tauri::async_runtime::spawn(async move {
            local.models.prewarm(&model_id).await;
        });
    }
    json!({ "success": true })
}

#[tauri::command]
pub fn detect_vulkan_gpu() -> Value {
    json!({ "available": false })
}

#[tauri::command]
pub fn get_llama_vulkan_status() -> Value {
    json!({ "supported": false, "downloaded": false })
}

#[tauri::command]
pub fn download_llama_vulkan_binary() -> Value {
    json!({ "success": false, "error": "Vulkan is not supported on macOS" })
}

#[tauri::command]
pub fn cancel_llama_vulkan_download() -> Value {
    json!({ "success": false })
}

#[tauri::command]
pub fn delete_llama_vulkan_binary() -> Value {
    json!({ "success": true })
}
