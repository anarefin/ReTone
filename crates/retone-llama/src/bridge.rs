//! One-at-a-time local inference for the renderer: a second request while one runs gets
//! `LOCAL_MODEL_BUSY`, a request is cancelled only by the id it was tagged
//! with, and a caller whose renderer can vanish (the tone picker) cancels by
//! tag prefix.

use crate::models::{InferenceOptions, ModelError, ModelManager};
use serde_json::Value;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Default)]
struct Slot {
    processing: bool,
    request_id: Option<String>,
}

pub struct LocalReasoningBridge {
    manager: Arc<ModelManager>,
    slot: Mutex<Slot>,
}

/// Frees the slot however the request ends (including a dropped future).
struct SlotGuard<'a>(&'a Mutex<Slot>);

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.processing = false;
        slot.request_id = None;
    }
}

/// `max(512, min(len * 2, 2048))`, with the length in UTF-16 units like JS.
pub fn calculate_max_tokens(text: &str) -> u64 {
    let length = text.encode_utf16().count() as u64;
    (length * 2).clamp(512, 2048)
}

fn find_ci(haystack_lower: &str, needle: &str, from: usize) -> Option<usize> {
    haystack_lower[from..].find(needle).map(|i| i + from)
}

/// Removes `<think>…</think>` blocks (innermost closed pairs first, so nested
/// blocks leave no stray tag), then a trailing unterminated `<think>`, then
/// any orphan `</think>`, and trims. Case-insensitive.
pub fn strip_thinking_tags(text: &str) -> String {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut out = text.to_string();
    loop {
        // ASCII lowercasing keeps byte offsets aligned with `out`.
        let lower = out.to_ascii_lowercase();
        let mut search = 0;
        let mut removed = false;
        while let Some(open) = find_ci(&lower, OPEN, search) {
            let Some(close) = find_ci(&lower, CLOSE, open + OPEN.len()) else {
                break;
            };
            let nested = find_ci(&lower, OPEN, open + OPEN.len()).is_some_and(|next| next < close);
            if !nested {
                out.replace_range(open..close + CLOSE.len(), "");
                removed = true;
                break;
            }
            search = open + OPEN.len();
        }
        if !removed {
            break;
        }
    }
    let lower = out.to_ascii_lowercase();
    if let Some(open) = lower.find(OPEN) {
        out.truncate(open);
    }
    loop {
        let lower = out.to_ascii_lowercase();
        let Some(close) = lower.find(CLOSE) else {
            break;
        };
        out.replace_range(close..close + CLOSE.len(), "");
    }
    out.trim().to_string()
}

fn options_from(text: &str, config: &Value) -> InferenceOptions {
    let number = |key: &str| config.get(key).and_then(Value::as_f64);
    InferenceOptions {
        system_prompt: config
            .get("systemPrompt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        max_tokens: Some(
            number("maxTokens")
                .map(|n| n.max(0.0) as u64)
                .unwrap_or_else(|| calculate_max_tokens(text)),
        ),
        temperature: Some(number("temperature").unwrap_or(0.7)),
        // The bridge passes `disableThinking !== false`.
        disable_thinking: Some(
            config.get("disableThinking").and_then(Value::as_bool) != Some(false),
        ),
        require_complete_output: config
            .get("requireCompleteOutput")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        refuse_clipped_by_window: config.get("refuseClippedByWindow").and_then(Value::as_bool)
            == Some(true),
        response_format: config
            .get("responseFormat")
            .filter(|v| !v.is_null())
            .cloned(),
        context_size: number("contextSize").filter(|n| *n > 0.0).map(|n| n as u64),
    }
}

impl LocalReasoningBridge {
    pub fn new(manager: Arc<ModelManager>) -> Self {
        Self {
            manager,
            slot: Mutex::new(Slot::default()),
        }
    }

    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn manager(&self) -> &Arc<ModelManager> {
        &self.manager
    }

    /// A local model can run: the binary exists and a model is downloaded.
    pub fn is_available(&self) -> bool {
        self.manager.server().is_available() && self.manager.any_downloaded()
    }

    pub fn is_processing(&self) -> bool {
        self.slot().processing
    }

    /// Runs `text` through `model_id` with the renderer's `config`
    /// (`ReasoningConfig`).
    pub async fn process_text(
        &self,
        text: &str,
        model_id: &str,
        config: &Value,
    ) -> Result<String, ModelError> {
        {
            let mut slot = self.slot();
            if slot.processing {
                return Err(ModelError::new(
                    "Already processing a request",
                    "LOCAL_MODEL_BUSY",
                    Value::Null,
                ));
            }
            slot.processing = true;
            slot.request_id = config
                .get("requestId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        let _guard = SlotGuard(&self.slot);

        let options = options_from(text, config);
        tracing::debug!(target: "llama", model_id, text_len = text.len(), max_tokens = options.max_tokens, "LOCAL_BRIDGE_INFERENCE");
        let result = self.manager.run_inference(model_id, text, &options).await?;
        // Thinking tags inside a JSON string are document data.
        if options.response_format.is_some() {
            return Ok(result);
        }
        Ok(if options.disable_thinking == Some(false) {
            result.trim().to_string()
        } else {
            strip_thinking_tags(&result)
        })
    }

    /// Aborts the running request only if it carries `request_id`.
    pub fn cancel(&self, request_id: &str) {
        let matches =
            !request_id.is_empty() && self.slot().request_id.as_deref() == Some(request_id);
        if matches {
            self.manager.cancel_inference();
        }
    }

    /// Aborts the running request only if its id starts with `prefix`.
    pub fn cancel_tagged(&self, prefix: &str) {
        let matches = !prefix.is_empty()
            && self
                .slot()
                .request_id
                .as_deref()
                .is_some_and(|id| id.starts_with(prefix));
        if matches {
            self.manager.cancel_inference();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_tokens_scale_with_the_text_within_bounds() {
        assert_eq!(calculate_max_tokens("short"), 512);
        assert_eq!(calculate_max_tokens(&"a".repeat(600)), 1200);
        assert_eq!(calculate_max_tokens(&"a".repeat(5000)), 2048);
        // UTF-16 units: an astral character counts twice, as in JS.
        assert_eq!(calculate_max_tokens(&"😀".repeat(300)), 1200);
    }

    #[test]
    fn strips_closed_nested_unterminated_and_orphan_think_tags() {
        assert_eq!(strip_thinking_tags("<think>plan</think>Answer"), "Answer");
        assert_eq!(strip_thinking_tags("<THINK>plan</Think> Answer "), "Answer");
        assert_eq!(
            strip_thinking_tags("<think>a<think>b</think>c</think>Done"),
            "Done"
        );
        assert_eq!(strip_thinking_tags("Answer<think>still going"), "Answer");
        assert_eq!(strip_thinking_tags("Answer</think> tail"), "Answer tail");
        assert_eq!(strip_thinking_tags("x</think><think>y</think>z"), "xz");
        assert_eq!(
            strip_thinking_tags("<think>a</think>B<think>c</think>D"),
            "BD"
        );
        assert_eq!(strip_thinking_tags("  plain  "), "plain");
    }

    #[test]
    fn config_maps_onto_inference_options() {
        let options = options_from(
            "hello",
            &serde_json::json!({
                "systemPrompt": "Rewrite.", "maxTokens": 4096, "temperature": 0.3,
                "contextSize": 16384, "requireCompleteOutput": true, "requestId": "tone-rewrite-1"
            }),
        );
        assert_eq!(options.system_prompt, "Rewrite.");
        assert_eq!(options.max_tokens, Some(4096));
        assert_eq!(options.temperature, Some(0.3));
        assert_eq!(options.context_size, Some(16384));
        assert!(options.require_complete_output);
        assert_eq!(options.disable_thinking, Some(true));
        assert!(options.response_format.is_none());

        let defaults = options_from("hi", &serde_json::json!({ "disableThinking": false }));
        assert_eq!(defaults.max_tokens, Some(512));
        assert_eq!(defaults.temperature, Some(0.7));
        assert_eq!(defaults.disable_thinking, Some(false));
    }
}
