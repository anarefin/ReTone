//! The Anthropic proxy behind `process_anthropic_reasoning`. Anthropic refuses
//! browser-origin requests, so the call runs here. Resolves with
//! `{ success: true, text }` or `{ success: false, ...ipcErrorFields }`, which
//! the renderer turns back into a classified Error (`errorFromIpcResult`).

use retone_core::providers::anthropic::{
    API_VERSION, PROVIDER_NAME, build_request_body, describe_missing_text, extract_text,
};
use retone_core::providers::errors::{
    ErrorContext, KEY_MISSING, TIMEOUT, UNREACHABLE, failure_result, plain_ipc_fields,
    provider_error, provider_http_error, redact_provider_body,
};
use retone_core::providers::timeouts::LLM_REQUEST_TIMEOUT_SECONDS;
use serde_json::{Value, json};
use std::time::Duration;

pub struct AnthropicCall<'a> {
    pub client: &'a reqwest::Client,
    /// `MESSAGES_URL`, or a mock server in tests.
    pub url: &'a str,
    pub api_key: &'a str,
    pub timeout: Duration,
}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(LLM_REQUEST_TIMEOUT_SECONDS);

fn plain_failure(message: &str) -> Value {
    tracing::error!(target: "reasoning", error = message, "Anthropic reasoning error");
    failure_result(plain_ipc_fields(message))
}

fn transport_failure(error: &reqwest::Error, ctx: &ErrorContext) -> Value {
    let code = if error.is_timeout() {
        TIMEOUT
    } else {
        UNREACHABLE
    };
    tracing::error!(target: "reasoning", code, "Anthropic request failed");
    failure_result(provider_error(code, ctx, None).ipc_fields())
}

impl AnthropicCall<'_> {
    pub async fn run(&self, text: &str, model_id: &str, config: &Value) -> Value {
        let ctx = ErrorContext::llm(PROVIDER_NAME, Some(model_id));
        if self.api_key.is_empty() {
            return failure_result(
                provider_error(KEY_MISSING, &ErrorContext::llm(PROVIDER_NAME, None), None)
                    .ipc_fields(),
            );
        }
        if model_id.is_empty() {
            return plain_failure("No model specified for Anthropic API call");
        }

        let body = build_request_body(text, model_id, config);
        let response = match self
            .client
            .post(self.url)
            .timeout(self.timeout)
            .header("Content-Type", "application/json")
            .header("X-API-Key", self.api_key)
            .header("anthropic-version", API_VERSION)
            .body(body.to_string())
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => return transport_failure(&error, &ctx),
        };

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            tracing::warn!(
                target: "reasoning",
                status,
                body = %redact_provider_body(&error_text),
                "Anthropic API error"
            );
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            return failure_result(
                provider_http_error(&ctx, status, &error_text, header).ipc_fields(),
            );
        }

        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => return transport_failure(&error, &ctx),
        };
        let Ok(data) = serde_json::from_slice::<Value>(&bytes) else {
            return plain_failure("Anthropic returned a response that is not JSON");
        };
        if config.get("requireCompleteOutput") == Some(&Value::Bool(true))
            && data.get("stop_reason").and_then(Value::as_str) == Some("max_tokens")
        {
            return plain_failure("Model output was truncated");
        }
        match extract_text(&data) {
            Some(text) => json!({ "success": true, "text": text }),
            None => plain_failure(&describe_missing_text(&data)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{Reply, closed_port_url, serve};

    fn call<'a>(client: &'a reqwest::Client, url: &'a str, key: &'a str) -> AnthropicCall<'a> {
        AnthropicCall {
            client,
            url,
            api_key: key,
            timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn sends_the_messages_request_and_returns_the_text() {
        let server = serve(|_| {
            Reply::json(
                200,
                json!({ "content": [{ "type": "thinking", "thinking": "…" }, { "type": "text", "text": " Hello there. " }], "stop_reason": "end_turn" }),
            )
        })
        .await;
        let client = reqwest::Client::new();
        let url = format!("{}/v1/messages", server.url);
        let result = call(&client, &url, "sk-ant-key")
            .run(
                "hi",
                "claude-haiku-4-5",
                &json!({ "systemPrompt": "Be formal." }),
            )
            .await;
        assert_eq!(result, json!({ "success": true, "text": "Hello there." }));

        let request = &server.requests()[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/messages");
        assert_eq!(request.header("x-api-key"), Some("sk-ant-key"));
        assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
        assert_eq!(request.header("content-type"), Some("application/json"));
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["model"], "claude-haiku-4-5");
        assert_eq!(body["system"], "Be formal.");
        assert_eq!(
            body["messages"],
            json!([{ "role": "user", "content": "hi" }])
        );
        assert!(body.get("temperature").is_none());
    }

    #[tokio::test]
    async fn an_http_failure_is_classified_and_never_leaks_the_body_into_the_message() {
        let server = serve(|_| {
            Reply::text(
                401,
                r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key sk-ant-api03-secretsecret"}}"#,
                &[("request-id", "req_abc")],
            )
        })
        .await;
        let client = reqwest::Client::new();
        let url = format!("{}/v1/messages", server.url);
        let result = call(&client, &url, "sk-ant-api03-secretsecret")
            .run("hi", "claude-x", &json!({}))
            .await;
        assert_eq!(result["success"], false);
        assert_eq!(result["code"], "PROVIDER_AUTH_FAILED");
        assert_eq!(result["messageKey"], "providerErrors.authFailed");
        assert_eq!(result["error"], "Anthropic rejected your API key.");
        assert_eq!(result["settingsTarget"], "llms");
        assert_eq!(result["surface"], "llm");
        assert_eq!(result["status"], 401);
        assert_eq!(result["technicalDetails"]["requestId"], "req_abc");
        assert!(!result.to_string().contains("secretsecret"));
    }

    #[tokio::test]
    async fn missing_key_model_and_unreachable_server() {
        let client = reqwest::Client::new();
        let missing = call(&client, "http://127.0.0.1:9/", "")
            .run("hi", "claude-x", &json!({}))
            .await;
        assert_eq!(missing["code"], "API_KEY_MISSING");
        assert_eq!(missing["messageKey"], "providerErrors.keyMissing");

        let no_model = call(&client, "http://127.0.0.1:9/", "k")
            .run("hi", "", &json!({}))
            .await;
        assert_eq!(
            no_model,
            json!({ "success": false, "error": "No model specified for Anthropic API call" })
        );

        let closed = closed_port_url().await;
        let down = call(&client, &closed, "k")
            .run("hi", "claude-x", &json!({}))
            .await;
        assert_eq!(down["code"], "PROVIDER_UNREACHABLE");
        assert_eq!(down["messageKey"], "providerErrors.unreachable");
    }

    #[tokio::test]
    async fn a_stalled_answer_times_out() {
        let server = serve(|_| Reply::StallBody { status: 200 }).await;
        let client = reqwest::Client::new();
        let result = AnthropicCall {
            client: &client,
            url: &server.url,
            api_key: "k",
            timeout: Duration::from_millis(300),
        }
        .run("hi", "claude-x", &json!({}))
        .await;
        assert_eq!(result["code"], "PROVIDER_TIMEOUT");
    }

    #[tokio::test]
    async fn truncated_or_textless_output_is_an_error() {
        let server =
            serve(|_| Reply::json(200, json!({ "content": [], "stop_reason": "max_tokens" })))
                .await;
        let client = reqwest::Client::new();
        let truncated = call(&client, &server.url, "k")
            .run("hi", "m", &json!({ "requireCompleteOutput": true }))
            .await;
        assert_eq!(truncated["error"], "Model output was truncated");
        let empty = call(&client, &server.url, "k")
            .run("hi", "m", &json!({}))
            .await;
        assert_eq!(
            empty["error"],
            "Anthropic returned no text before hitting the max_tokens limit"
        );
    }
}
