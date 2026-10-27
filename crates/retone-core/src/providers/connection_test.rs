//! The provider connection check behind Settings' "Connected" badge. It lists the provider's models with the key
//! (OpenRouter: describes the key) and reports a keyed failure the renderer
//! translates via `errorCode`. The English `error` stays for logs.
//!
//! HTTP is behind [`ProbeTransport`]. The transport must enforce
//! [`PROBE_TIMEOUT`] over the whole answer, body included, and honour
//! [`RedirectMode`].

use super::urls::{is_private_host, model_endpoints, normalize_base_url};
use serde_json::{Map, Value, json};
use std::future::Future;
use std::time::Duration;

/// One probe's cap, from connecting until the body is read.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(12);

fn fixed_endpoint(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "openai" => "https://api.openai.com/v1/models",
        "groq" => "https://api.groq.com/openai/v1/models",
        // The largest page each API allows, so a valid but older model is listed.
        "anthropic" => "https://api.anthropic.com/v1/models?limit=1000",
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000",
        // OpenRouter's model list is public, so it cannot vouch for a key. The
        // key route describes the key that calls it.
        "openrouter" => "https://openrouter.ai/api/v1/key",
        _ => return None,
    })
}

/// Whether a redirect may be followed. A hosted provider's fixed endpoint never
/// redirects, and following one would carry `x-api-key` / `x-goog-api-key` to
/// whichever host it names, so a redirect fails the check instead. Custom
/// servers may redirect within themselves (a trailing slash, say).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectMode {
    Error,
    Follow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    pub endpoints: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub redirect: RedirectMode,
}

impl ProviderRequest {
    pub fn endpoint(&self) -> &str {
        &self.endpoints[0]
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A check that fails before any request goes out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub error_code: &'static str,
    pub message: &'static str,
}

fn config_error(error_code: &'static str, message: &'static str) -> ConfigError {
    ConfigError {
        error_code,
        message,
    }
}

fn str_field<'a>(config: &'a Value, name: &str) -> &'a str {
    config.get(name).and_then(Value::as_str).unwrap_or("")
}

/// Builds the probe for `config` (`{ provider, apiKey, baseUrl, model }`).
/// `env` reads the process environment (`RETONE_OPENAI_BASE_URL`,
/// `OPENAI_BASE_URL`).
pub fn resolve_provider_request(
    config: &Value,
    env: impl Fn(&str) -> Option<String>,
) -> Result<ProviderRequest, ConfigError> {
    let provider = str_field(config, "provider").to_lowercase();
    let api_key = str_field(config, "apiKey").trim().to_string();
    let mut endpoints: Vec<String> = fixed_endpoint(&provider)
        .map(|e| vec![e.to_string()])
        .unwrap_or_default();
    let mut redirect = RedirectMode::Error;

    if provider == "openai" {
        let override_url = env("RETONE_OPENAI_BASE_URL")
            .filter(|v| !v.is_empty())
            .or_else(|| env("OPENAI_BASE_URL"))
            .unwrap_or_default();
        let override_url = normalize_base_url(&override_url);
        if !override_url.is_empty() {
            endpoints = model_endpoints(&override_url);
            redirect = RedirectMode::Follow;
        }
    }

    if provider == "custom" {
        let raw = str_field(config, "baseUrl").trim();
        if raw.is_empty() {
            return Err(config_error(
                "endpointRequired",
                "Enter an endpoint URL before testing.",
            ));
        }
        let candidate = if raw.contains("://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        };
        let parsed = url::Url::parse(&candidate)
            .map_err(|_| config_error("invalidUrl", "The endpoint must be a valid URL."))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(config_error(
                "invalidUrl",
                "The endpoint must use HTTP or HTTPS.",
            ));
        }
        if parsed.scheme() == "http" && !is_private_host(parsed.host_str().unwrap_or("")) {
            return Err(config_error(
                "httpsRequired",
                "Public endpoints must use HTTPS. HTTP is only allowed on private addresses.",
            ));
        }
        endpoints = model_endpoints(parsed.as_str());
        redirect = RedirectMode::Follow;
    }

    if endpoints.is_empty() {
        return Err(config_error(
            "unsupportedProvider",
            "Connection testing is not available for this provider.",
        ));
    }
    if api_key.is_empty() && provider != "custom" {
        return Err(config_error(
            "apiKeyRequired",
            "Add an API key before testing.",
        ));
    }

    let mut headers = vec![("Accept".to_string(), "application/json".to_string())];
    if !api_key.is_empty() {
        match provider.as_str() {
            "anthropic" => {
                headers.push(("x-api-key".into(), api_key));
                headers.push(("anthropic-version".into(), "2023-06-01".into()));
            }
            "gemini" => headers.push(("x-goog-api-key".into(), api_key)),
            _ => headers.push(("Authorization".into(), format!("Bearer {api_key}"))),
        }
    }

    Ok(ProviderRequest {
        endpoints,
        headers,
        redirect,
    })
}

/// A probe that got no answer: the cap fired, or the transport failed (a
/// refused redirect included). Neither is a verdict on the credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeError {
    Timeout,
    Network,
}

pub trait ProbeResponse: Send {
    fn status(&self) -> u16;
    /// The body as JSON. `Ok(None)` for an empty or non-JSON body; `Err` when
    /// the body never arrives (the cap firing, a dropped connection).
    fn json(self) -> impl Future<Output = Result<Option<Value>, ProbeError>> + Send;
}

pub trait ProbeTransport: Sync {
    type Response: ProbeResponse;
    /// A GET to `url` with the request's headers and redirect mode.
    fn get(
        &self,
        url: &str,
        request: &ProviderRequest,
    ) -> impl Future<Output = Result<Self::Response, ProbeError>> + Send;
}

/// Why a check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub error_code: &'static str,
    pub error: String,
    pub status: Option<u16>,
    /// The provider gave no verdict (slow, busy, down): the app may go ahead.
    pub unverified: bool,
}

impl Failure {
    fn rank(&self) -> u8 {
        // An answer that faults the setup outranks no answer, so a busy
        // candidate never hides a refused key or a missing model. No answer
        // outranks a candidate without a model list.
        if self.unverified {
            match self.error_code {
                "providerStatus" => 3,
                "timeout" => 2,
                "network" => 1,
                _ => 0,
            }
        } else {
            match self.error_code {
                "credentialsRejected" => 6,
                "providerStatus" => 5,
                "modelNotFound" => 4,
                _ => 0,
            }
        }
    }
}

/// The check's answer, serialized as the renderer expects:
/// `{ success: true }` or `{ success: false, errorCode, error, status?, unverified? }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionResult {
    Success,
    Failure(Failure),
}

impl ConnectionResult {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Success => json!({ "success": true }),
            Self::Failure(f) => {
                let mut out = Map::new();
                out.insert("success".into(), Value::Bool(false));
                out.insert("errorCode".into(), json!(f.error_code));
                out.insert("error".into(), json!(f.error));
                if let Some(status) = f.status {
                    out.insert("status".into(), json!(status));
                }
                if f.unverified {
                    out.insert("unverified".into(), Value::Bool(true));
                }
                Value::Object(out)
            }
        }
    }
}

fn is_unverified_status(status: u16) -> bool {
    status == 408 || status == 429 || status >= 500
}

fn describe_status_failure(status: u16, provider: &str) -> Failure {
    if status == 401 || status == 403 {
        return Failure {
            error_code: "credentialsRejected",
            error: "The provider rejected these credentials.".into(),
            status: Some(status),
            unverified: false,
        };
    }
    if status == 404 && provider == "custom" {
        return Failure {
            error_code: "endpointNotFound",
            error: "The endpoint does not expose an OpenAI-compatible model list.".into(),
            status: Some(status),
            unverified: false,
        };
    }
    Failure {
        error_code: "providerStatus",
        error: format!("The provider returned status {status}."),
        status: Some(status),
        unverified: is_unverified_status(status),
    }
}

fn model_not_found() -> Failure {
    Failure {
        error_code: "modelNotFound",
        error: "The selected model is not available from this provider.".into(),
        status: None,
        unverified: false,
    }
}

fn probe_failure(error: ProbeError) -> Failure {
    match error {
        ProbeError::Timeout => Failure {
            error_code: "timeout",
            error: "The connection test timed out.".into(),
            status: None,
            unverified: true,
        },
        ProbeError::Network => Failure {
            error_code: "network",
            error: "The provider could not be reached.".into(),
            status: None,
            unverified: true,
        },
    }
}

/// Gemini answers a made-up key with a 400, which on its own can also mean a
/// malformed request. Its error body says which it was.
fn gemini_key_rejected(body: &Option<Value>) -> bool {
    body.as_ref()
        .and_then(|b| b.pointer("/error/details"))
        .and_then(Value::as_array)
        .is_some_and(|details| {
            details
                .iter()
                .any(|d| d.get("reason").and_then(Value::as_str) == Some("API_KEY_INVALID"))
        })
}

async fn describe_response_failure<R: ProbeResponse>(
    response: R,
    provider: &str,
) -> Result<Failure, ProbeError> {
    let status = response.status();
    let failure = describe_status_failure(status, provider);
    // A 401 or 403 already says the key was refused; only a vaguer status needs
    // the body, and only Gemini's body says anything about the key.
    if failure.error_code != "credentialsRejected"
        && provider == "gemini"
        && gemini_key_rejected(&response.json().await?)
    {
        return Ok(Failure {
            status: Some(status),
            ..describe_status_failure(401, provider)
        });
    }
    Ok(failure)
}

fn normalize_model_id(value: &str) -> String {
    let trimmed = value.trim();
    match trimmed.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("models/") => trimmed[7..].to_string(),
        _ => trimmed.to_string(),
    }
}

fn response_model_ids(payload: &Option<Value>) -> Vec<String> {
    let Some(payload) = payload else {
        return Vec::new();
    };
    let models = payload
        .as_array()
        .or_else(|| payload.get("data").and_then(Value::as_array))
        .or_else(|| payload.get("models").and_then(Value::as_array));
    models
        .into_iter()
        .flatten()
        .filter_map(|model| match model {
            Value::String(s) => Some(s.as_str()),
            other => other
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| other.get("name").and_then(Value::as_str)),
        })
        .map(normalize_model_id)
        .filter(|id| !id.is_empty())
        .collect()
}

/// Anthropic resolves an alias (`claude-haiku-4-5`) to a dated snapshot and
/// lists only the snapshot (`claude-haiku-4-5-20251001`).
fn is_dated_snapshot(id: &str, model: &str) -> bool {
    id.strip_prefix(model).is_some_and(|rest| {
        rest.len() == 9 && rest.starts_with('-') && rest[1..].bytes().all(|b| b.is_ascii_digit())
    })
}

async fn response_offers_model<R: ProbeResponse>(
    response: R,
    model: &str,
    provider: &str,
) -> Result<bool, ProbeError> {
    if model.is_empty() {
        return Ok(true);
    }
    let selected = normalize_model_id(model);
    let ids = response_model_ids(&response.json().await?);
    Ok(ids
        .iter()
        .any(|id| *id == selected || (provider == "anthropic" && is_dated_snapshot(id, &selected))))
}

/// A management (formerly provisioning) key only administers other keys and
/// can never run a request. Only a 200 carries the key's description.
async fn describe_openrouter_key<R: ProbeResponse>(
    response: R,
) -> Result<Option<Failure>, ProbeError> {
    let status = response.status();
    if status != 200 {
        return Ok(Some(describe_status_failure(status, "openrouter")));
    }
    let body = response.json().await?;
    let key = body.as_ref().and_then(|b| b.get("data"));
    let flag = |name: &str| key.and_then(|k| k.get(name)).is_some_and(truthy);
    if !flag("is_management_key") && !flag("is_provisioning_key") {
        return Ok(None);
    }
    Ok(Some(Failure {
        status: Some(status),
        ..describe_status_failure(401, "openrouter")
    }))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

async fn judge<R: ProbeResponse>(
    response: R,
    provider: &str,
    model: &str,
) -> Result<Option<Failure>, ProbeError> {
    let status = response.status();
    if !(200..300).contains(&status) {
        return describe_response_failure(response, provider)
            .await
            .map(Some);
    }
    if provider == "openrouter" {
        return describe_openrouter_key(response).await;
    }
    Ok((!response_offers_model(response, model, provider).await?).then(model_not_found))
}

/// Runs the check. Every candidate endpoint is tried until one passes; when all
/// fail, the most actionable failure is reported.
pub async fn test_provider_connection<T: ProbeTransport>(
    config: &Value,
    transport: &T,
    env: impl Fn(&str) -> Option<String>,
) -> ConnectionResult {
    let request = match resolve_provider_request(config, env) {
        Ok(request) => request,
        Err(error) => {
            return ConnectionResult::Failure(Failure {
                error_code: error.error_code,
                error: error.message.to_string(),
                status: None,
                unverified: false,
            });
        }
    };
    let provider = str_field(config, "provider").to_lowercase();
    let model = str_field(config, "model").trim().to_string();
    let mut failure: Option<Failure> = None;
    for endpoint in &request.endpoints {
        let outcome = match transport.get(endpoint, &request).await {
            Ok(response) => judge(response, &provider, &model).await,
            Err(error) => Err(error),
        };
        let next = match outcome {
            Ok(None) => return ConnectionResult::Success,
            Ok(Some(f)) => f,
            Err(error) => probe_failure(error),
        };
        failure = Some(match failure {
            Some(current) if next.rank() <= current.rank() => current,
            _ => next,
        });
    }
    ConnectionResult::Failure(failure.expect("at least one endpoint"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scripted answer for one probe.
    #[derive(Clone)]
    enum Answer {
        /// Status plus body: `Ok(Some)` JSON, `Ok(None)` empty/unreadable, `Err` never arrives.
        Reply(u16, Result<Option<Value>, ProbeError>),
        Fail(ProbeError),
    }

    fn status(code: u16) -> Answer {
        Answer::Reply(code, Ok(None))
    }

    fn json_reply(code: u16, body: Value) -> Answer {
        Answer::Reply(code, Ok(Some(body)))
    }

    struct FakeResponse {
        status: u16,
        body: Result<Option<Value>, ProbeError>,
        reads: &'static AtomicUsize,
    }

    impl ProbeResponse for FakeResponse {
        fn status(&self) -> u16 {
            self.status
        }
        async fn json(self) -> Result<Option<Value>, ProbeError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.body
        }
    }

    struct Fake<F: Fn(&str) -> Answer + Sync> {
        answer: F,
        calls: Mutex<Vec<(String, ProviderRequest)>>,
        reads: &'static AtomicUsize,
    }

    fn fake<F: Fn(&str) -> Answer + Sync>(answer: F) -> Fake<F> {
        Fake {
            answer,
            calls: Mutex::new(Vec::new()),
            reads: Box::leak(Box::new(AtomicUsize::new(0))),
        }
    }

    impl<F: Fn(&str) -> Answer + Sync> ProbeTransport for Fake<F> {
        type Response = FakeResponse;
        async fn get(
            &self,
            url: &str,
            request: &ProviderRequest,
        ) -> Result<FakeResponse, ProbeError> {
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), request.clone()));
            match (self.answer)(url) {
                Answer::Reply(status, body) => Ok(FakeResponse {
                    status,
                    body,
                    reads: self.reads,
                }),
                Answer::Fail(error) => Err(error),
            }
        }
    }

    impl<F: Fn(&str) -> Answer + Sync> Fake<F> {
        fn urls(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|c| c.0.clone())
                .collect()
        }
        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn resolve(config: Value) -> ProviderRequest {
        resolve_provider_request(&config, no_env).expect("resolves")
    }

    async fn check<F: Fn(&str) -> Answer + Sync>(config: Value, transport: &Fake<F>) -> Value {
        test_provider_connection(&config, transport, no_env)
            .await
            .to_json()
    }

    /// A transport that must never be reached.
    fn unreachable_fake() -> Fake<impl Fn(&str) -> Answer + Sync> {
        fake(|url: &str| panic!("unexpected request to {url}"))
    }

    #[test]
    fn builds_provider_tests_without_credentials_in_the_url() {
        let request = resolve(json!({ "provider": "openai", "apiKey": "secret" }));
        assert_eq!(request.endpoint(), "https://api.openai.com/v1/models");
        assert!(!request.endpoint().contains("secret"));
        assert_eq!(request.header("Authorization"), Some("Bearer secret"));
        assert_eq!(request.header("Accept"), Some("application/json"));
    }

    #[test]
    fn honors_openai_base_url_overrides_from_the_environment() {
        let env = |name: &str| {
            (name == "OPENAI_BASE_URL").then(|| "https://proxy.example.com/v1/".to_string())
        };
        let request =
            resolve_provider_request(&json!({ "provider": "openai", "apiKey": "secret" }), env)
                .unwrap();
        assert_eq!(request.endpoints, ["https://proxy.example.com/v1/models"]);
        assert_eq!(request.redirect, RedirectMode::Follow);
        let retone = |name: &str| match name {
            "RETONE_OPENAI_BASE_URL" => Some("https://mine.example.com/v1".to_string()),
            "OPENAI_BASE_URL" => Some("https://other.example.com/v1".to_string()),
            _ => None,
        };
        let request =
            resolve_provider_request(&json!({ "provider": "openai", "apiKey": "k" }), retone)
                .unwrap();
        assert_eq!(request.endpoints, ["https://mine.example.com/v1/models"]);
    }

    #[test]
    fn normalizes_custom_compatible_endpoints() {
        let request =
            resolve(json!({ "provider": "custom", "baseUrl": "localhost:11434/v1", "apiKey": "" }));
        assert_eq!(request.endpoint(), "https://localhost:11434/v1/models");
    }

    #[test]
    fn probes_the_v1_sibling_for_bare_custom_origins() {
        assert_eq!(
            resolve(
                json!({ "provider": "custom", "baseUrl": "http://localhost:1234", "apiKey": "" })
            )
            .endpoints,
            [
                "http://localhost:1234/models",
                "http://localhost:1234/v1/models"
            ]
        );
        assert_eq!(
            resolve(json!({ "provider": "custom", "baseUrl": "http://localhost:1234/api/v0" }))
                .endpoints,
            [
                "http://localhost:1234/api/v0/models",
                "http://localhost:1234/v1/models"
            ]
        );
    }

    #[tokio::test]
    async fn rejects_public_http_endpoints_but_allows_private_hosts() {
        let none = unreachable_fake();
        assert_eq!(
            check(
                json!({ "provider": "custom", "baseUrl": "http://myserver.example.com/v1" }),
                &none
            )
            .await,
            json!({
                "success": false,
                "errorCode": "httpsRequired",
                "error": "Public endpoints must use HTTPS. HTTP is only allowed on private addresses.",
            })
        );
        for base_url in [
            "http://localhost:1234",
            "http://192.168.1.10:8080/v1",
            "http://100.101.102.103/v1",
            "http://ollama.ts.net/v1",
        ] {
            assert!(
                !resolve(json!({ "provider": "custom", "baseUrl": base_url }))
                    .endpoints
                    .is_empty()
            );
        }
        // A public DNS name that merely starts with a private prefix is public.
        assert_eq!(
            check(
                json!({ "provider": "custom", "baseUrl": "http://10.example.com/v1" }),
                &none
            )
            .await["errorCode"],
            "httpsRequired"
        );
    }

    #[tokio::test]
    async fn rejects_invalid_configs_with_error_codes() {
        let none = unreachable_fake();
        assert_eq!(
            check(json!({ "provider": "custom", "baseUrl": "" }), &none).await,
            json!({ "success": false, "errorCode": "endpointRequired", "error": "Enter an endpoint URL before testing." })
        );
        assert_eq!(
            check(json!({ "provider": "custom", "baseUrl": "ftp://x" }), &none).await,
            json!({ "success": false, "errorCode": "invalidUrl", "error": "The endpoint must use HTTP or HTTPS." })
        );
        assert_eq!(
            check(json!({ "provider": "unknown", "apiKey": "k" }), &none).await,
            json!({ "success": false, "errorCode": "unsupportedProvider", "error": "Connection testing is not available for this provider." })
        );
        assert_eq!(
            check(json!({ "provider": "openai", "apiKey": "" }), &none).await,
            json!({ "success": false, "errorCode": "apiKeyRequired", "error": "Add an API key before testing." })
        );
    }

    #[tokio::test]
    async fn maps_authentication_and_transport_failures_to_safe_messages() {
        assert_eq!(
            check(
                json!({ "provider": "openai", "apiKey": "bad" }),
                &fake(|_| status(401))
            )
            .await,
            json!({ "success": false, "errorCode": "credentialsRejected", "error": "The provider rejected these credentials.", "status": 401 })
        );
        assert_eq!(
            check(
                json!({ "provider": "openai", "apiKey": "bad" }),
                &fake(|_| Answer::Fail(ProbeError::Network))
            )
            .await,
            json!({ "success": false, "errorCode": "network", "error": "The provider could not be reached.", "unverified": true })
        );
        assert_eq!(
            check(
                json!({ "provider": "openai", "apiKey": "k" }),
                &fake(|_| status(500))
            )
            .await,
            json!({ "success": false, "errorCode": "providerStatus", "error": "The provider returned status 500.", "status": 500, "unverified": true })
        );
    }

    #[tokio::test]
    async fn a_slow_busy_or_down_provider_gives_no_verdict() {
        for code in [408, 429, 500, 502, 503, 504] {
            let result = check(
                json!({ "provider": "openai", "apiKey": "k" }),
                &fake(move |_| status(code)),
            )
            .await;
            assert_eq!(result["errorCode"], "providerStatus", "{code}");
            assert_eq!(result["unverified"], true, "{code}");
        }
        for (error, code) in [
            (ProbeError::Timeout, "timeout"),
            (ProbeError::Network, "network"),
        ] {
            let result = check(
                json!({ "provider": "openai", "apiKey": "k" }),
                &fake(move |_| Answer::Fail(error)),
            )
            .await;
            assert_eq!(result["errorCode"], code);
            assert_eq!(result["unverified"], true);
        }
    }

    #[tokio::test]
    async fn an_answer_that_faults_the_key_or_setup_is_never_unverified() {
        let cases: Vec<(Value, Answer, &str)> = vec![
            (
                json!({ "provider": "openai", "apiKey": "bad" }),
                status(401),
                "credentialsRejected",
            ),
            (
                json!({ "provider": "openai", "apiKey": "bad" }),
                status(403),
                "credentialsRejected",
            ),
            (
                json!({ "provider": "openai", "apiKey": "k" }),
                status(400),
                "providerStatus",
            ),
            (
                json!({ "provider": "openai", "apiKey": "k" }),
                status(402),
                "providerStatus",
            ),
            (
                json!({ "provider": "openai", "apiKey": "k" }),
                status(404),
                "providerStatus",
            ),
            (
                json!({ "provider": "openai", "apiKey": "k", "model": "gone" }),
                json_reply(200, json!({ "data": [{ "id": "other" }] })),
                "modelNotFound",
            ),
            (
                json!({ "provider": "custom", "baseUrl": "http://localhost:1234" }),
                status(404),
                "endpointNotFound",
            ),
            (
                json!({ "provider": "gemini", "apiKey": "made-up" }),
                json_reply(
                    400,
                    json!({ "error": { "details": [{ "reason": "API_KEY_INVALID" }] } }),
                ),
                "credentialsRejected",
            ),
        ];
        for (config, answer, code) in cases {
            let result = check(config.clone(), &fake(move |_| answer.clone())).await;
            assert_eq!(result["errorCode"], code, "{config}");
            assert!(result.get("unverified").is_none(), "{config}");
        }
    }

    #[tokio::test]
    async fn across_a_custom_servers_addresses_an_answer_outranks_no_answer() {
        let answer = |models: Answer, v1: Answer| {
            fake(move |url: &str| {
                if url.contains("/v1/models") {
                    v1.clone()
                } else {
                    models.clone()
                }
            })
        };
        let base = |extra: Value| {
            let mut config = json!({ "provider": "custom", "baseUrl": "http://localhost:1234" });
            config
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            config
        };
        let list_without = json_reply(200, json!({ "data": [{ "id": "other" }] }));

        let missing = check(
            base(json!({ "model": "m" })),
            &answer(status(503), list_without),
        )
        .await;
        assert_eq!(missing["errorCode"], "modelNotFound");
        assert!(missing.get("unverified").is_none());
        let refused = check(
            base(json!({ "apiKey": "bad" })),
            &answer(Answer::Fail(ProbeError::Timeout), status(401)),
        )
        .await;
        assert_eq!(refused["errorCode"], "credentialsRejected");

        for (first, second) in [(400, 503), (503, 400)] {
            let result = check(base(json!({})), &answer(status(first), status(second))).await;
            assert_eq!(result["status"], 400, "{first} then {second}");
            assert!(result.get("unverified").is_none());
        }

        let busy = check(base(json!({})), &answer(status(404), status(503))).await;
        assert_eq!(busy["status"], 503);
        assert_eq!(busy["unverified"], true);
        let slow = check(
            base(json!({})),
            &answer(status(404), Answer::Fail(ProbeError::Timeout)),
        )
        .await;
        assert_eq!(slow["errorCode"], "timeout");
        assert_eq!(slow["unverified"], true);
    }

    #[tokio::test]
    async fn succeeds_when_only_the_v1_candidate_responds() {
        let transport = fake(|url: &str| {
            if url.contains("/v1/models") {
                status(200)
            } else {
                status(404)
            }
        });
        let result = check(
            json!({ "provider": "custom", "baseUrl": "http://localhost:1234" }),
            &transport,
        )
        .await;
        assert_eq!(result, json!({ "success": true }));
        assert_eq!(
            transport.urls(),
            [
                "http://localhost:1234/models",
                "http://localhost:1234/v1/models"
            ]
        );
    }

    #[tokio::test]
    async fn rejects_a_model_list_that_omits_the_selected_model() {
        let transport = fake(|_| json_reply(200, json!({ "data": [{ "id": "real-model" }] })));
        assert_eq!(
            check(
                json!({ "provider": "custom", "baseUrl": "http://localhost:1234/v1", "model": "typo-model" }),
                &transport
            )
            .await,
            json!({ "success": false, "errorCode": "modelNotFound", "error": "The selected model is not available from this provider." })
        );
    }

    #[tokio::test]
    async fn accepts_openai_and_gemini_model_list_shapes() {
        for payload in [
            json!({ "data": [{ "id": "selected-model" }] }),
            json!({ "models": [{ "name": "models/selected-model" }] }),
            json!(["selected-model"]),
        ] {
            let transport = fake(move |_| json_reply(200, payload.clone()));
            let result = check(
                json!({ "provider": "custom", "baseUrl": "http://localhost:1234/v1", "model": "selected-model" }),
                &transport,
            )
            .await;
            assert_eq!(result, json!({ "success": true }));
        }
    }

    #[tokio::test]
    async fn accepts_an_anthropic_alias_by_its_dated_snapshot_but_not_a_prefix() {
        let with_id =
            |id: &'static str| fake(move |_| json_reply(200, json!({ "data": [{ "id": id }] })));
        let snapshot = "claude-opus-4-5-20251101";
        assert_eq!(
            check(
                json!({ "provider": "anthropic", "apiKey": "k", "model": "claude-opus-4-5" }),
                &with_id(snapshot)
            )
            .await,
            json!({ "success": true })
        );
        assert_eq!(
            check(
                json!({ "provider": "anthropic", "apiKey": "k", "model": "claude-opus-4" }),
                &with_id(snapshot)
            )
            .await["errorCode"],
            "modelNotFound"
        );
        let custom = json!({ "provider": "custom", "baseUrl": "http://localhost:1234/v1", "model": "foo", "apiKey": "k" });
        assert_eq!(
            check(custom, &with_id("foo-20251001")).await["errorCode"],
            "modelNotFound"
        );
    }

    #[tokio::test]
    async fn continues_to_a_compatible_candidate_when_an_earlier_list_omits_the_selection() {
        let transport = fake(|url: &str| {
            let id = if url.contains("/v1/models") {
                "selected-model"
            } else {
                "native-model"
            };
            json_reply(200, json!({ "data": [{ "id": id }] }))
        });
        let result = check(
            json!({ "provider": "custom", "baseUrl": "http://localhost:1234", "model": "selected-model" }),
            &transport,
        )
        .await;
        assert_eq!(result, json!({ "success": true }));
        assert_eq!(
            transport.urls(),
            [
                "http://localhost:1234/models",
                "http://localhost:1234/v1/models"
            ]
        );
    }

    #[tokio::test]
    async fn reports_endpoint_not_found_only_after_every_candidate_404s() {
        assert_eq!(
            check(
                json!({ "provider": "custom", "baseUrl": "http://localhost:1234" }),
                &fake(|_| status(404))
            )
            .await,
            json!({ "success": false, "errorCode": "endpointNotFound", "error": "The endpoint does not expose an OpenAI-compatible model list.", "status": 404 })
        );
        let transport =
            fake(|url: &str| status(if url.contains("/v1/models") { 401 } else { 404 }));
        assert_eq!(
            check(json!({ "provider": "custom", "baseUrl": "http://localhost:1234", "apiKey": "bad" }), &transport).await,
            json!({ "success": false, "errorCode": "credentialsRejected", "error": "The provider rejected these credentials.", "status": 401 })
        );
    }

    #[test]
    fn anthropic_and_gemini_are_asked_for_their_largest_page() {
        assert_eq!(
            resolve(json!({ "provider": "anthropic", "apiKey": "k" })).endpoint(),
            "https://api.anthropic.com/v1/models?limit=1000"
        );
        assert_eq!(
            resolve(json!({ "provider": "gemini", "apiKey": "k" })).endpoint(),
            "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000"
        );
        let anthropic = resolve(json!({ "provider": "anthropic", "apiKey": "k" }));
        assert_eq!(anthropic.header("x-api-key"), Some("k"));
        assert_eq!(anthropic.header("anthropic-version"), Some("2023-06-01"));
        assert_eq!(anthropic.header("Authorization"), None);
        let gemini = resolve(json!({ "provider": "gemini", "apiKey": "g" }));
        assert_eq!(gemini.header("x-goog-api-key"), Some("g"));
    }

    #[tokio::test]
    async fn gemini_rejections_are_read_from_the_error_body() {
        let cases = [
            (
                Some(
                    json!({ "error": { "code": 400, "status": "INVALID_ARGUMENT", "details": [{ "reason": "API_KEY_INVALID" }] } }),
                ),
                "credentialsRejected",
            ),
            (
                Some(json!({ "error": { "status": "FAILED_PRECONDITION" } })),
                "providerStatus",
            ),
            (
                Some(
                    json!({ "error": { "code": 400, "details": [{ "reason": "API_KEY_SERVICE_BLOCKED" }] } }),
                ),
                "providerStatus",
            ),
            (None, "providerStatus"),
        ];
        for (body, code) in cases {
            let transport = fake(move |_| Answer::Reply(400, Ok(body.clone())));
            let result = check(
                json!({ "provider": "gemini", "apiKey": "made-up" }),
                &transport,
            )
            .await;
            assert_eq!(result["errorCode"], code);
            assert_eq!(result["status"], 400);
        }
    }

    #[tokio::test]
    async fn an_error_body_that_never_arrives_is_a_timeout_or_network_failure() {
        for (error, code) in [
            (ProbeError::Timeout, "timeout"),
            (ProbeError::Network, "network"),
        ] {
            let transport = fake(move |_| Answer::Reply(400, Err(error)));
            let result = check(
                json!({ "provider": "gemini", "apiKey": "made-up" }),
                &transport,
            )
            .await;
            assert_eq!(result["errorCode"], code);
        }
    }

    #[tokio::test]
    async fn a_401_or_403_is_a_rejection_whatever_its_body_does() {
        for provider in ["gemini", "openai", "anthropic"] {
            for code in [401, 403] {
                let transport = fake(move |_| Answer::Reply(code, Err(ProbeError::Network)));
                let result = check(
                    json!({ "provider": provider, "apiKey": "made-up" }),
                    &transport,
                )
                .await;
                assert_eq!(
                    result["errorCode"], "credentialsRejected",
                    "{provider} {code}"
                );
                assert_eq!(transport.reads(), 0, "the status alone decides");
            }
        }
    }

    #[tokio::test]
    async fn other_providers_error_bodies_are_never_read() {
        for provider in ["openai", "groq", "anthropic"] {
            let transport = fake(|_| {
                json_reply(
                    400,
                    json!({ "error": { "message": "Incorrect API key provided: sk-ma****" } }),
                )
            });
            let result = check(json!({ "provider": provider, "apiKey": "k" }), &transport).await;
            assert_eq!(result["errorCode"], "providerStatus", "{provider}");
            assert_eq!(transport.reads(), 0);
        }
    }

    #[tokio::test]
    async fn hosted_providers_refuse_redirects_and_custom_servers_follow_them() {
        for provider in ["openai", "groq", "anthropic", "gemini", "openrouter"] {
            assert_eq!(
                resolve(json!({ "provider": provider, "apiKey": "k" })).redirect,
                RedirectMode::Error
            );
        }
        assert_eq!(
            resolve(json!({ "provider": "custom", "baseUrl": "http://localhost:1234" })).redirect,
            RedirectMode::Follow
        );
        let transport = fake(|_| json_reply(200, json!({ "data": [{ "id": "m" }] })));
        check(
            json!({ "provider": "openai", "apiKey": "k", "model": "m" }),
            &transport,
        )
        .await;
        assert_eq!(
            transport.calls.lock().unwrap()[0].1.redirect,
            RedirectMode::Error
        );
    }

    #[tokio::test]
    async fn openrouter_is_checked_at_its_key_route() {
        let request = resolve(json!({ "provider": "openrouter", "apiKey": "sk-or-made-up" }));
        assert_eq!(request.endpoint(), "https://openrouter.ai/api/v1/key");
        assert_eq!(
            request.header("Authorization"),
            Some("Bearer sk-or-made-up")
        );
        for code in [401, 403] {
            let transport = fake(move |_| {
                json_reply(code, json!({ "error": { "message": "User not found." } }))
            });
            let result = check(
                json!({ "provider": "openrouter", "apiKey": "sk-or-made-up" }),
                &transport,
            )
            .await;
            assert_eq!(result["errorCode"], "credentialsRejected");
        }
    }

    #[tokio::test]
    async fn an_openrouter_management_key_is_refused() {
        for flag in ["is_management_key", "is_provisioning_key"] {
            let transport =
                fake(move |_| json_reply(200, json!({ "data": { "label": "admin", flag: true } })));
            let result = check(
                json!({ "provider": "openrouter", "apiKey": "k" }),
                &transport,
            )
            .await;
            assert_eq!(result["errorCode"], "credentialsRejected", "{flag}");
        }
    }

    #[tokio::test]
    async fn any_other_openrouter_key_passes_with_no_model_check() {
        let config = json!({ "provider": "openrouter", "apiKey": "sk-or-key", "model": "anthropic/claude-test" });
        let transport = fake(|_| {
            json_reply(
                200,
                json!({ "data": { "label": "laptop", "is_free_tier": true, "limit_remaining": null } }),
            )
        });
        assert_eq!(
            check(config.clone(), &transport).await,
            json!({ "success": true })
        );
        assert_eq!(transport.urls(), ["https://openrouter.ai/api/v1/key"]);
        assert_eq!(
            check(config.clone(), &fake(|_| status(200))).await,
            json!({ "success": true })
        );
        assert_eq!(
            check(config, &fake(|_| status(503))).await["errorCode"],
            "providerStatus"
        );
    }

    #[tokio::test]
    async fn only_a_200_from_the_openrouter_key_route_can_pass() {
        for code in [204, 202] {
            let result = check(
                json!({ "provider": "openrouter", "apiKey": "sk-or-key" }),
                &fake(move |_| status(code)),
            )
            .await;
            assert_eq!(
                result,
                json!({ "success": false, "errorCode": "providerStatus", "error": format!("The provider returned status {code}."), "status": code })
            );
        }
    }

    #[tokio::test]
    async fn a_model_list_whose_body_never_arrives_is_no_verdict() {
        let config = json!({ "provider": "openai", "apiKey": "k", "model": "gpt-test" });
        for (error, code) in [
            (ProbeError::Timeout, "timeout"),
            (ProbeError::Network, "network"),
        ] {
            let result = check(
                config.clone(),
                &fake(move |_| Answer::Reply(200, Err(error))),
            )
            .await;
            assert_eq!(result["errorCode"], code);
            assert_eq!(result["unverified"], true);
        }
        let unreadable = check(config, &fake(|_| status(200))).await;
        assert_eq!(
            unreadable["errorCode"], "modelNotFound",
            "a list that can't be read offers nothing"
        );
    }

    #[tokio::test]
    async fn a_reasoning_check_on_a_server_without_model_lists_is_endpoint_not_found() {
        let config = json!({ "scope": "toneRewrite", "provider": "custom", "baseUrl": "http://127.0.0.1:9", "model": "m" });
        let missing = fake(|_| status(404));
        assert_eq!(
            check(config.clone(), &missing).await["errorCode"],
            "endpointNotFound"
        );
        assert_eq!(missing.urls().len(), 2);
        let down = fake(|_| Answer::Fail(ProbeError::Network));
        assert_eq!(check(config, &down).await["errorCode"], "network");
        assert_eq!(down.urls().len(), 2);
    }
}
