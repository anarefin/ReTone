//! Anthropic Messages API request and response handling (the request body
//! the `process-anthropic-reasoning` command sends, and its response).

use serde_json::{Map, Value, json};

pub const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2023-06-01";
pub const PROVIDER_NAME: &str = "Anthropic";

/// The request body. Claude models from Opus 4.7 onward reject `temperature`
/// with a 400, so it is sent only when the renderer says the model supports it.
pub fn build_request_body(text: &str, model: &str, config: &Value) -> Value {
    let max_tokens = config
        .get("maxTokens")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            // `text.length` in JS counts UTF-16 code units.
            let len = text.encode_utf16().count() as u64;
            (len * 2).clamp(100, 4096)
        });
    let system = config
        .get("systemPrompt")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert(
        "messages".into(),
        json!([{ "role": "user", "content": text }]),
    );
    body.insert("system".into(), json!(system));
    body.insert("max_tokens".into(), json!(max_tokens));
    if config.get("supportsTemperature") == Some(&Value::Bool(true)) {
        let temperature = config
            .get("temperature")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(json!(0.3));
        body.insert("temperature".into(), temperature);
    }
    Value::Object(body)
}

/// The trimmed concatenation of every text block, or `None` when the response
/// has no text block at all. `content` can be empty (max_tokens hit first) or
/// lead with thinking/tool_use blocks, so `content[0].text` is not safe.
pub fn extract_text(data: &Value) -> Option<String> {
    let blocks = data.get("content").and_then(Value::as_array)?;
    let parts: Vec<&str> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.concat().trim().to_string())
}

pub fn describe_missing_text(data: &Value) -> String {
    match data.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => {
            "Anthropic returned no text before hitting the max_tokens limit".to_string()
        }
        Some(reason) if !reason.is_empty() => {
            format!("Anthropic response contained no text content (stop_reason: {reason})")
        }
        _ => "Anthropic response contained no text content".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_trimmed_text_for_a_single_block() {
        let data = json!({ "content": [{ "type": "text", "text": "  rewritten text  " }] });
        assert_eq!(extract_text(&data).as_deref(), Some("rewritten text"));
    }

    #[test]
    fn joins_multiple_text_blocks() {
        let data = json!({ "content": [
            { "type": "text", "text": "first " },
            { "type": "text", "text": "second" },
        ]});
        assert_eq!(extract_text(&data).as_deref(), Some("first second"));
    }

    #[test]
    fn skips_a_leading_thinking_block() {
        let data = json!({ "content": [
            { "type": "thinking", "thinking": "let me consider..." },
            { "type": "text", "text": "the answer" },
        ]});
        assert_eq!(extract_text(&data).as_deref(), Some("the answer"));
    }

    #[test]
    fn none_when_there_is_no_text_block() {
        assert_eq!(
            extract_text(&json!({ "content": [], "stop_reason": "max_tokens" })),
            None
        );
        assert_eq!(
            extract_text(&json!({ "content": [{ "type": "thinking", "thinking": "..." }] })),
            None
        );
        assert_eq!(extract_text(&json!({})), None);
        assert_eq!(extract_text(&Value::Null), None);
        assert_eq!(extract_text(&json!({ "content": "nope" })), None);
        assert_eq!(
            extract_text(&json!({ "content": [{ "type": "text" }] })),
            None
        );
    }

    #[test]
    fn whitespace_only_text_is_an_empty_success() {
        let data = json!({ "content": [{ "type": "text", "text": "   " }] });
        assert_eq!(extract_text(&data).as_deref(), Some(""));
    }

    #[test]
    fn describes_missing_text() {
        assert!(
            describe_missing_text(&json!({ "stop_reason": "max_tokens" }))
                .contains("max_tokens limit")
        );
        assert!(describe_missing_text(&json!({ "stop_reason": "refusal" })).contains("refusal"));
        assert_eq!(
            describe_missing_text(&json!({})),
            "Anthropic response contained no text content"
        );
    }

    #[test]
    fn request_body_sizes_max_tokens_and_gates_temperature() {
        let body = build_request_body("hello", "claude-x", &json!({ "systemPrompt": "sys" }));
        assert_eq!(
            body,
            json!({
                "model": "claude-x",
                "messages": [{ "role": "user", "content": "hello" }],
                "system": "sys",
                "max_tokens": 100,
            })
        );
        let long = "a".repeat(3000);
        assert_eq!(
            build_request_body(&long, "m", &json!({}))["max_tokens"],
            4096
        );
        assert_eq!(
            build_request_body(&"a".repeat(700), "m", &json!({}))["max_tokens"],
            1400
        );
        assert_eq!(
            build_request_body("x", "m", &json!({ "maxTokens": 50 }))["max_tokens"],
            50
        );
        let warm = build_request_body("x", "m", &json!({ "supportsTemperature": true }));
        assert_eq!(warm["temperature"], 0.3);
        let set = build_request_body(
            "x",
            "m",
            &json!({ "supportsTemperature": true, "temperature": 0.9 }),
        );
        assert_eq!(set["temperature"], 0.9);
        let off = build_request_body("x", "m", &json!({ "temperature": 0.9 }));
        assert!(off.get("temperature").is_none());
        assert_eq!(off["system"], "");
    }
}
