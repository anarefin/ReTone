//! Renderer logging.

use crate::logging;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub struct LogEntry {
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    message: Option<Value>,
    #[serde(default)]
    meta: Option<Value>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[tauri::command]
pub fn get_log_level() -> String {
    logging::logger().level().to_string()
}

#[tauri::command]
pub fn app_log(entry: LogEntry) -> Value {
    let message = match &entry.message {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    logging::log_entry(
        entry.level.as_deref().unwrap_or("info"),
        &message,
        entry.meta.as_ref(),
        entry.scope.as_deref(),
        entry.source.as_deref(),
    );
    json!({ "success": true })
}
