//! Keyed provider errors, kept in sync with `src/helpers/providerHttpErrors.js`
//! and `src/helpers/ipcErrorFields.js`. The codes, message keys and English
//! sentences are the ones the renderer produces, so
//! `describeProviderError` renders an error from here exactly like one built in
//! the renderer. Raw provider bodies only ever reach `technicalDetails`, and
//! only redacted and capped at 500 characters.

use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

pub const AUTH_FAILED: &str = "PROVIDER_AUTH_FAILED";
pub const ACCESS_DENIED: &str = "PROVIDER_ACCESS_DENIED";
pub const QUOTA_EXHAUSTED: &str = "PROVIDER_QUOTA_EXHAUSTED";
pub const RATE_LIMITED: &str = "PROVIDER_RATE_LIMITED";
pub const MODEL_NOT_FOUND: &str = "PROVIDER_MODEL_NOT_FOUND";
pub const PAYLOAD_TOO_LARGE: &str = "PROVIDER_PAYLOAD_TOO_LARGE";
pub const BAD_REQUEST: &str = "PROVIDER_BAD_REQUEST";
pub const UNAVAILABLE: &str = "PROVIDER_UNAVAILABLE";
pub const TIMEOUT: &str = "PROVIDER_TIMEOUT";
pub const UNREACHABLE: &str = "PROVIDER_UNREACHABLE";
pub const NO_RESPONSE: &str = "PROVIDER_NO_RESPONSE";
pub const ERROR: &str = "PROVIDER_ERROR";
pub const KEY_MISSING: &str = "API_KEY_MISSING";

/// The name self-hosted errors carry instead of a provider.
pub const SELF_HOSTED_NAME: &str = "Your server";

const MAX_DETAIL_CHARS: usize = 500;

/// Only failures the user fixes in Settings get the deep link.
const FIXABLE: [&str; 6] = [
    AUTH_FAILED,
    ACCESS_DENIED,
    QUOTA_EXHAUSTED,
    MODEL_NOT_FOUND,
    NO_RESPONSE,
    KEY_MISSING,
];

fn message_key_for(code: &str) -> &'static str {
    match code {
        AUTH_FAILED => "providerErrors.authFailed",
        ACCESS_DENIED => "providerErrors.accessDenied",
        QUOTA_EXHAUSTED => "providerErrors.quotaExhausted",
        RATE_LIMITED => "hooks.audioRecording.errorDescriptions.providerRateLimited",
        MODEL_NOT_FOUND => "providerErrors.modelNotFound",
        PAYLOAD_TOO_LARGE => "providerErrors.payloadTooLarge",
        BAD_REQUEST => "providerErrors.badRequest",
        UNAVAILABLE => "providerErrors.unavailable",
        TIMEOUT => "providerErrors.timeout",
        UNREACHABLE => "providerErrors.unreachable",
        NO_RESPONSE => "providerErrors.noResponse",
        _ => "providerErrors.unknown",
    }
}

/// English twins of the `en` locale strings, by message key.
fn english_template(message_key: &str) -> &'static str {
    match message_key {
        "providerErrors.authFailed" => "{{provider}} rejected your API key.",
        "providerErrors.accessDenied" => {
            "{{provider}} denied access. Your key may not include this model."
        }
        "providerErrors.quotaExhausted" => "Your {{provider}} account is out of credit.",
        "providerErrors.rateLimited" => {
            "{{provider}} rate-limited the request. Wait a moment and try again."
        }
        "providerErrors.modelNotFound" => "{{provider}} doesn't recognize the model “{{model}}”.",
        "providerErrors.modelNotFoundNoModel" => {
            "{{provider}} doesn't recognize the selected model."
        }
        "providerErrors.payloadTooLarge" => "This recording is too large for {{provider}}.",
        "providerErrors.requestTooLarge" => "This request is too large for {{provider}}.",
        "providerErrors.badRequest" => "{{provider}} couldn't process this request.",
        "providerErrors.unavailable" => {
            "{{provider}} is having problems right now. Try again shortly."
        }
        "providerErrors.timeout" => "{{provider}} took too long to respond.",
        "providerErrors.unreachable" => "Couldn't reach {{provider}}. Check your connection.",
        "providerErrors.noResponse" => {
            "Couldn't get a response from {{provider}}. Check your API key and connection."
        }
        "providerErrors.keyMissing" => {
            "No API key is set for {{provider}}. Add it in Settings → Language Models."
        }
        "providerErrors.selfHosted.authFailed" => "Your server rejected the API key.",
        "providerErrors.selfHosted.accessDenied" => {
            "Your server denied access. The key may not include this model."
        }
        "providerErrors.selfHosted.quotaExhausted" => {
            "Your server says the account is out of credit."
        }
        "providerErrors.selfHosted.rateLimited" => {
            "Your server rate-limited the request. Wait a moment and try again."
        }
        "providerErrors.selfHosted.modelNotFound" => {
            "Your server doesn't recognize the model “{{model}}”."
        }
        "providerErrors.selfHosted.modelNotFoundNoModel" => {
            "Your server doesn't recognize the selected model."
        }
        "providerErrors.selfHosted.payloadTooLarge" => {
            "This recording is too large for your server."
        }
        "providerErrors.selfHosted.requestTooLarge" => "This request is too large for your server.",
        "providerErrors.selfHosted.badRequest" => "Your server couldn't process this request.",
        "providerErrors.selfHosted.unavailable" => {
            "Your server is having problems right now. Try again shortly."
        }
        "providerErrors.selfHosted.timeout" => "Your server took too long to respond.",
        "providerErrors.selfHosted.unreachable" => {
            "Couldn't reach your server. Check that it's running."
        }
        "providerErrors.selfHosted.noResponse" => {
            "Couldn't get a response from your server. Check its API key and that it's running."
        }
        "providerErrors.selfHosted.unknown" => "Your server returned an unexpected error.",
        "hooks.audioRecording.errorDescriptions.providerRateLimited" => {
            "{{provider}} rate-limited the request. Wait a moment and try again."
        }
        "hooks.audioRecording.errorDescriptions.providerKeyMissing" => {
            "No API key is set for {{provider}}."
        }
        _ => "{{provider}} returned an unexpected error.",
    }
}

static INVALID_KEY_SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)invalid[ _-]?(x-)?api[ _-]?key|incorrect api key|api_key_invalid|authentication_error|unauthorized",
    )
    .expect("invalid key pattern")
});
// Only signals that mean the account is out of credit; rate-limit 429s use
// broader words ("check your plan and billing details").
static QUOTA_SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)insufficient_quota|credit balance|insufficient (credits|balance|funds)|out of credits|payment required",
    )
    .expect("quota pattern")
});
static MODEL_SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)model_not_found|model[^.\n]{0,80}(not found|does not exist|not available|is not supported)|(unknown|invalid|unsupported) model",
    )
    .expect("model pattern")
});
static HTML_BODY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<\s*(!doctype|html|body|head)\b").expect("html pattern"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("ws pattern"));
static SECRET_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{4,}",
            "Bearer [redacted]",
        ),
        (r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_*.-]{4,}", "[redacted]"),
        (r"\bgsk_[A-Za-z0-9*]{4,}", "[redacted]"),
        (r"\bAIza[A-Za-z0-9_-]{4,}", "[redacted]"),
        (r"\bxai-[A-Za-z0-9*]{4,}", "[redacted]"),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("secret pattern"), r))
    .collect()
});

/// Redacts echoed keys, collapses whitespace, replaces HTML pages with a
/// marker and caps the text at 500 characters.
pub fn redact_provider_body(body: &str) -> String {
    if HTML_BODY.is_match(body) {
        return "<HTML response>".to_string();
    }
    let mut text = body.to_string();
    for (pattern, replacement) in SECRET_PATTERNS.iter() {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    let text = WHITESPACE.replace_all(&text, " ").trim().to_string();
    if text.chars().count() > MAX_DETAIL_CHARS {
        let cut: String = text.chars().take(MAX_DETAIL_CHARS).collect();
        format!("{cut}…")
    } else {
        text
    }
}

fn code_for_status(status: u16, text: &str, self_hosted: bool) -> &'static str {
    if status == 401 {
        return AUTH_FAILED;
    }
    if (status == 400 || status == 403) && INVALID_KEY_SIGNAL.is_match(text) {
        return AUTH_FAILED;
    }
    if status == 402 {
        return QUOTA_EXHAUSTED;
    }
    if matches!(status, 400 | 403 | 429) && QUOTA_SIGNAL.is_match(text) {
        return QUOTA_EXHAUSTED;
    }
    if status == 403 {
        return ACCESS_DENIED;
    }
    if status == 429 {
        return RATE_LIMITED;
    }
    if status == 404 {
        // A self-hosted 404 is far more often a wrong URL than a wrong model.
        return if self_hosted && !MODEL_SIGNAL.is_match(text) {
            ERROR
        } else {
            MODEL_NOT_FOUND
        };
    }
    if status == 400 && MODEL_SIGNAL.is_match(text) {
        return MODEL_NOT_FOUND;
    }
    if status == 413 {
        return PAYLOAD_TOO_LARGE;
    }
    if matches!(status, 400 | 415 | 422) {
        return BAD_REQUEST;
    }
    if status == 408 || status == 504 {
        return TIMEOUT;
    }
    if (500..600).contains(&status) {
        return UNAVAILABLE;
    }
    ERROR
}

/// Who failed and where the error will be shown.
#[derive(Debug, Clone, Default)]
pub struct ErrorContext {
    /// Display name, e.g. "Anthropic".
    pub provider: String,
    pub model: Option<String>,
    /// `"llm"` or `"transcription"`; ReTone only uses `"llm"`.
    pub surface: Option<String>,
    pub self_hosted: bool,
}

impl ErrorContext {
    pub fn llm(provider: &str, model: Option<&str>) -> Self {
        Self {
            provider: provider.to_string(),
            model: model.filter(|m| !m.is_empty()).map(str::to_string),
            surface: Some("llm".to_string()),
            self_hosted: false,
        }
    }
}

/// A classified provider failure: what `providerHttpError` / `providerError`
/// return, minus the JS `Error` wrapper.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderError {
    pub message: String,
    pub code: &'static str,
    pub message_key: String,
    pub message_params: Map<String, Value>,
    pub surface: Option<String>,
    pub settings_target: Option<&'static str>,
    pub technical_details: Map<String, Value>,
    pub status: Option<u16>,
}

fn english_message(message_key: &str, params: &Map<String, Value>) -> String {
    // Parameters are inserted literally, so a model named `we$&ird` stays as is.
    let template = english_template(message_key);
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = &after[..end];
                if let Some(value) = params.get(name).and_then(Value::as_str) {
                    out.push_str(value);
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn build_classification(
    code: &'static str,
    ctx: &ErrorContext,
    technical_details: Map<String, Value>,
    status: Option<u16>,
) -> ProviderError {
    let provider_name = if ctx.self_hosted {
        SELF_HOSTED_NAME.to_string()
    } else {
        ctx.provider.clone()
    };
    let mut params = Map::new();
    params.insert("provider".into(), Value::String(provider_name.clone()));
    let mut message_key = message_key_for(code).to_string();
    if code == MODEL_NOT_FOUND {
        match &ctx.model {
            Some(model) => {
                params.insert("model".into(), Value::String(model.clone()));
            }
            None => message_key = "providerErrors.modelNotFoundNoModel".into(),
        }
    }
    let surface = ctx.surface.as_deref();
    if code == KEY_MISSING {
        message_key = if surface == Some("transcription") {
            "hooks.audioRecording.errorDescriptions.providerKeyMissing".into()
        } else {
            "providerErrors.keyMissing".into()
        };
    }
    if surface == Some("llm") {
        if code == RATE_LIMITED {
            message_key = "providerErrors.rateLimited".into();
        }
        if code == PAYLOAD_TOO_LARGE {
            message_key = "providerErrors.requestTooLarge".into();
        }
    }
    if ctx.self_hosted && message_key.starts_with("providerErrors.") {
        message_key = message_key.replacen("providerErrors.", "providerErrors.selfHosted.", 1);
    }
    let settings_target = if FIXABLE.contains(&code) {
        match surface {
            Some("transcription") => Some("speechToText"),
            Some("llm") => Some("llms"),
            _ => None,
        }
    } else {
        None
    };
    let mut details = Map::new();
    details.insert("provider".into(), Value::String(provider_name));
    details.extend(technical_details);
    ProviderError {
        message: english_message(&message_key, &params),
        code,
        message_key,
        message_params: params,
        surface: ctx.surface.clone(),
        settings_target,
        technical_details: details,
        status,
    }
}

/// Classifies an HTTP failure. `header` looks a response header up by its
/// lowercase name (for the request id).
pub fn provider_http_error(
    ctx: &ErrorContext,
    status: u16,
    body: &str,
    header: impl Fn(&str) -> Option<String>,
) -> ProviderError {
    let underlying = redact_provider_body(body);
    let request_id = header("x-request-id")
        .or_else(|| header("request-id"))
        .or_else(|| header("cf-ray"))
        .filter(|v| !v.is_empty());
    let mut details = Map::new();
    details.insert("status".into(), json!(status));
    if let Some(id) = request_id {
        details.insert("requestId".into(), Value::String(id));
    }
    if !underlying.is_empty() {
        details.insert("underlyingError".into(), Value::String(underlying));
    }
    build_classification(
        code_for_status(status, body, ctx.self_hosted),
        ctx,
        details,
        Some(status),
    )
}

/// A failure with no HTTP answer (key missing, timeout, unreachable).
/// `underlying` is a short transport code, never a body.
pub fn provider_error(
    code: &'static str,
    ctx: &ErrorContext,
    underlying: Option<&str>,
) -> ProviderError {
    let mut details = Map::new();
    if let Some(u) = underlying {
        details.insert("underlyingError".into(), Value::String(u.to_string()));
    }
    build_classification(code, ctx, details, None)
}

/// The fields that cross IPC (`IPC_ERROR_FIELDS`): the renderer's
/// `errorFromIpcResult` rebuilds the Error from them.
pub const IPC_ERROR_FIELDS: [&str; 7] = [
    "code",
    "messageKey",
    "messageParams",
    "settingsTarget",
    "technicalDetails",
    "status",
    "surface",
];

impl ProviderError {
    /// `ipcErrorFields(error)`: `{ error, code, messageKey, … }`, dropping unset ones.
    pub fn ipc_fields(&self) -> Map<String, Value> {
        let mut fields = Map::new();
        fields.insert("error".into(), Value::String(self.message.clone()));
        fields.insert("code".into(), Value::String(self.code.to_string()));
        fields.insert("messageKey".into(), Value::String(self.message_key.clone()));
        fields.insert(
            "messageParams".into(),
            Value::Object(self.message_params.clone()),
        );
        if let Some(target) = self.settings_target {
            fields.insert("settingsTarget".into(), Value::String(target.to_string()));
        }
        fields.insert(
            "technicalDetails".into(),
            Value::Object(self.technical_details.clone()),
        );
        if let Some(status) = self.status {
            fields.insert("status".into(), json!(status));
        }
        if let Some(surface) = &self.surface {
            fields.insert("surface".into(), Value::String(surface.clone()));
        }
        fields
    }
}

/// `ipcErrorFields` of a plain (unclassified) error: `{ error: message }`.
pub fn plain_ipc_fields(message: &str) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("error".into(), Value::String(message.to_string()));
    fields
}

/// `{ success: false, ...fields }`, the shape main-process handlers resolve with.
pub fn failure_result(fields: Map<String, Value>) -> Value {
    let mut out = Map::new();
    out.insert("success".into(), Value::Bool(false));
    out.extend(fields);
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(provider: &str, surface: &str, status: u16, body: &str) -> ProviderError {
        let ctx = ErrorContext {
            provider: provider.into(),
            surface: Some(surface.into()),
            ..Default::default()
        };
        provider_http_error(&ctx, status, body, |_| None)
    }

    #[test]
    fn auth_failures_point_at_settings() {
        let c = classify(
            "Mistral",
            "transcription",
            401,
            r#"{"detail":"Invalid API Key"}"#,
        );
        assert_eq!(c.code, AUTH_FAILED);
        assert_eq!(c.message_key, "providerErrors.authFailed");
        assert_eq!(
            c.message_params,
            json!({"provider": "Mistral"}).as_object().unwrap().clone()
        );
        assert_eq!(c.settings_target, Some("speechToText"));
        assert_eq!(classify("X", "llm", 401, "").settings_target, Some("llms"));
        let gemini = classify(
            "Gemini",
            "llm",
            400,
            r#"{"error":{"status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#,
        );
        assert_eq!(gemini.code, AUTH_FAILED);
        assert_eq!(
            classify("X", "llm", 403, "invalid x-api-key").code,
            AUTH_FAILED
        );
        let denied = classify(
            "X",
            "llm",
            403,
            r#"{"error":"model not allowed for this project"}"#,
        );
        assert_eq!(denied.code, ACCESS_DENIED);
    }

    #[test]
    fn quota_and_rate_limits() {
        assert_eq!(classify("X", "llm", 402, "").code, QUOTA_EXHAUSTED);
        let anthropic = classify(
            "Anthropic",
            "llm",
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#,
        );
        assert_eq!(anthropic.code, QUOTA_EXHAUSTED);
        let openai = classify(
            "OpenAI",
            "llm",
            429,
            r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota"}}"#,
        );
        assert_eq!(openai.code, QUOTA_EXHAUSTED);
        let gemini = classify(
            "Gemini",
            "llm",
            429,
            r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","status":"RESOURCE_EXHAUSTED"}}"#,
        );
        assert_eq!(gemini.code, RATE_LIMITED);
        assert_eq!(gemini.message_key, "providerErrors.rateLimited");
        assert_eq!(gemini.settings_target, None);
    }

    #[test]
    fn models_statuses_and_self_hosted() {
        let ctx = ErrorContext::llm("Mistral", Some("voxtral-max"));
        let not_found = provider_http_error(&ctx, 404, "", |_| None);
        assert_eq!(not_found.code, MODEL_NOT_FOUND);
        assert_eq!(not_found.message_params["model"], "voxtral-max");
        let bad_model = classify(
            "OpenAI",
            "llm",
            400,
            r#"{"error":{"message":"The model `gpt-9` does not exist","code":"model_not_found"}}"#,
        );
        assert_eq!(bad_model.message_key, "providerErrors.modelNotFoundNoModel");
        let self_hosted = ErrorContext {
            provider: "self-hosted".into(),
            self_hosted: true,
            surface: Some("llm".into()),
            ..Default::default()
        };
        let generic = provider_http_error(&self_hosted, 404, "<html>Not Found</html>", |_| None);
        assert_eq!(generic.code, ERROR);
        assert_eq!(generic.message_key, "providerErrors.selfHosted.unknown");
        assert_eq!(generic.message_params["provider"], SELF_HOSTED_NAME);
        let quota = provider_http_error(&self_hosted, 402, "", |_| None);
        assert_eq!(
            quota.message,
            "Your server says the account is out of credit."
        );

        assert_eq!(classify("X", "llm", 413, "").code, PAYLOAD_TOO_LARGE);
        assert_eq!(
            classify("X", "llm", 413, "").message_key,
            "providerErrors.requestTooLarge"
        );
        for status in [400, 415, 422] {
            assert_eq!(classify("X", "llm", status, "bad audio").code, BAD_REQUEST);
        }
        for status in [408, 504] {
            assert_eq!(classify("X", "llm", status, "").code, TIMEOUT);
        }
        for status in [500, 502, 503, 529] {
            assert_eq!(classify("X", "llm", status, "").code, UNAVAILABLE);
        }
        assert_eq!(classify("X", "llm", 418, "").code, ERROR);
    }

    #[test]
    fn technical_details_carry_request_id_and_a_trimmed_body() {
        let ctx = ErrorContext::llm("Mistral", None);
        let body = "x".repeat(800);
        let c = provider_http_error(&ctx, 500, &body, |name| {
            (name == "x-request-id").then(|| "req_123".to_string())
        });
        assert_eq!(c.technical_details["provider"], "Mistral");
        assert_eq!(c.technical_details["status"], 500);
        assert_eq!(c.technical_details["requestId"], "req_123");
        assert_eq!(
            c.technical_details["underlyingError"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            501
        );
        let plain = provider_http_error(&ctx, 500, "", |name| {
            (name == "request-id").then(|| "abc".to_string())
        });
        assert_eq!(plain.technical_details["requestId"], "abc");
        assert!(!plain.technical_details.contains_key("underlyingError"));
        let html = provider_http_error(
            &ctx,
            502,
            "<!DOCTYPE html><html><body>Bad gateway</body></html>",
            |_| None,
        );
        assert_eq!(html.technical_details["underlyingError"], "<HTML response>");
    }

    #[test]
    fn keys_echoed_back_in_a_body_are_redacted() {
        let out = redact_provider_body(
            "Incorrect API key provided: sk-proj-ab***xyz. Also sk-ant-api03-zzzz gsk_abcd1234 AIzaSyA1234 xai-qwerty Authorization: Bearer eyJhbGciOi.abc",
        );
        for leaked in [
            "sk-proj-ab",
            "sk-ant-api03",
            "gsk_abcd",
            "AIzaSy",
            "xai-qwerty",
            "eyJhbGciOi",
        ] {
            assert!(!out.contains(leaked), "{leaked} leaked: {out}");
        }
        assert!(out.contains("Bearer [redacted]"));
    }

    #[test]
    fn messages_are_short_english_sentences() {
        let c = classify("Mistral", "transcription", 401, "{}");
        assert_eq!(c.message, "Mistral rejected your API key.");
        assert!(!c.message.contains("{}"));
        let ctx = ErrorContext::llm("OpenAI", Some("we$&ird-$'model"));
        let weird = provider_http_error(&ctx, 404, "", |_| None);
        assert_eq!(
            weird.message,
            "OpenAI doesn't recognize the model “we$&ird-$'model”."
        );
        let missing = provider_error(KEY_MISSING, &ErrorContext::llm("Anthropic", None), None);
        assert_eq!(missing.code, KEY_MISSING);
        assert_eq!(missing.message_key, "providerErrors.keyMissing");
        assert_eq!(missing.settings_target, Some("llms"));
        assert_eq!(
            missing.message,
            "No API key is set for Anthropic. Add it in Settings → Language Models."
        );
    }

    #[test]
    fn every_message_key_exists_in_the_en_locale() {
        let en: Value =
            serde_json::from_str(include_str!("../../../../src/locales/en/translation.json"))
                .unwrap();
        let lookup = |key: &str| {
            key.split('.')
                .try_fold(&en, |node, part| node.get(part))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let llm = ErrorContext::llm("OpenAI", Some("m"));
        let self_hosted = ErrorContext {
            self_hosted: true,
            ..llm.clone()
        };
        let mut errors = Vec::new();
        for ctx in [&llm, &self_hosted] {
            for status in [401, 402, 403, 404, 413, 418, 422, 429, 500, 504] {
                errors.push(provider_http_error(ctx, status, "", |_| None));
            }
            for code in [TIMEOUT, UNREACHABLE, NO_RESPONSE] {
                errors.push(provider_error(code, ctx, None));
            }
        }
        // Key-missing has no self-hosted sentence (the JS map has none either).
        errors.push(provider_error(KEY_MISSING, &llm, None));
        for error in errors {
            let template = lookup(&error.message_key)
                .unwrap_or_else(|| panic!("{} missing from en", error.message_key));
            let rendered = english_message(&error.message_key, &error.message_params);
            let from_locale = {
                let mut s = template.clone();
                for (k, v) in &error.message_params {
                    s = s.replace(&format!("{{{{{k}}}}}"), v.as_str().unwrap());
                }
                s
            };
            assert_eq!(rendered, from_locale, "{}", error.message_key);
        }
    }

    #[test]
    fn ipc_fields_keep_every_classified_field_and_drop_unset_ones() {
        let ctx = ErrorContext {
            provider: "Mistral".into(),
            surface: Some("transcription".into()),
            ..Default::default()
        };
        let error = provider_http_error(&ctx, 401, "", |_| None);
        assert_eq!(
            Value::Object(error.ipc_fields()),
            json!({
                "error": "Mistral rejected your API key.",
                "code": "PROVIDER_AUTH_FAILED",
                "messageKey": "providerErrors.authFailed",
                "messageParams": { "provider": "Mistral" },
                "settingsTarget": "speechToText",
                "technicalDetails": { "provider": "Mistral", "status": 401 },
                "status": 401,
                "surface": "transcription",
            })
        );
        assert_eq!(
            Value::Object(plain_ipc_fields("plain")),
            json!({ "error": "plain" })
        );
        let unreachable = provider_error(UNREACHABLE, &ErrorContext::llm("Groq", None), None);
        let fields = unreachable.ipc_fields();
        assert!(!fields.contains_key("status"));
        assert!(!fields.contains_key("settingsTarget"));
        assert_eq!(fields["technicalDetails"], json!({ "provider": "Groq" }));
        for key in fields.keys().filter(|k| *k != "error") {
            assert!(IPC_ERROR_FIELDS.contains(&key.as_str()), "{key}");
        }
    }
}
