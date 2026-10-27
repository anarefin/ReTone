//! HTTP to the local llama-server, behind [`ServerClient`] so lifecycle tests
//! (idle unload, health) run on a paused clock without sockets.
//!
//! Every server is started with a random per-launch API key (see
//! [`generate_api_key`]); every request except `/health` carries it as
//! `Authorization: Bearer <key>`, so other local processes and browser pages
//! (llama-server answers any CORS origin) cannot use the server or read it.

use crate::process::BoxFuture;
use serde_json::Value;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

/// Where a running server listens, and the key it was started with.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    pub api_key: Arc<str>,
}

impl Endpoint {
    pub fn new(port: u16, api_key: impl Into<Arc<str>>) -> Self {
        Self {
            port,
            api_key: api_key.into(),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.api_key)
    }
}

// The key never reaches a log line.
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("port", &self.port)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// 32 random bytes from the OS as 64 hex characters.
pub fn generate_api_key() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Requests in flight (`requests_processing` + `requests_deferred`) from the
/// Prometheus text of `/metrics`; `None` when neither gauge is present.
pub fn requests_in_flight(metrics: &str) -> Option<u64> {
    let mut found = false;
    let mut total = 0f64;
    for line in metrics.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if matches!(
            name,
            "llamacpp:requests_processing" | "llamacpp:requests_deferred"
        ) && let Ok(value) = value.trim().parse::<f64>()
            && value.is_finite()
        {
            found = true;
            total += value.max(0.0);
        }
    }
    found.then_some(total as u64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostError {
    Timeout,
    Transport(String),
}

pub trait ServerClient: Send + Sync {
    /// `GET /health` answered 200 within `timeout` (no key needed).
    fn health(&self, endpoint: &Endpoint, timeout: Duration) -> BoxFuture<'_, bool>;

    /// A small JSON round trip (`GET` without a body, `POST` with one).
    /// `None` on any failure, non-200 status or non-JSON body: these calls
    /// refine a decision and must never fail the request.
    fn request_json(
        &self,
        endpoint: &Endpoint,
        path: &str,
        body: Option<Value>,
        timeout: Duration,
    ) -> BoxFuture<'_, Option<Value>>;

    /// `GET` returning a 200 body as text; `None` on any failure.
    fn get_text(
        &self,
        endpoint: &Endpoint,
        path: &str,
        timeout: Duration,
    ) -> BoxFuture<'_, Option<String>>;

    /// `POST` returning the status and the raw body.
    fn post(
        &self,
        endpoint: &Endpoint,
        path: &str,
        body: Value,
        timeout: Duration,
    ) -> BoxFuture<'_, Result<(u16, String), PostError>>;
}

/// The real client: loopback only, never through a proxy.
#[derive(Debug, Clone)]
pub struct ReqwestServerClient {
    client: reqwest::Client,
}

impl ReqwestServerClient {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().no_proxy().build()?,
        })
    }
}

fn describe(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

impl ServerClient for ReqwestServerClient {
    fn health(&self, endpoint: &Endpoint, timeout: Duration) -> BoxFuture<'_, bool> {
        let request = self.client.get(endpoint.url("/health")).timeout(timeout);
        Box::pin(async move {
            matches!(
                request.send().await,
                Ok(response) if response.status().as_u16() == 200
            )
        })
    }

    fn request_json(
        &self,
        endpoint: &Endpoint,
        path: &str,
        body: Option<Value>,
        timeout: Duration,
    ) -> BoxFuture<'_, Option<Value>> {
        let target = endpoint.url(path);
        let request = match body {
            Some(body) => self.client.post(target).json(&body),
            None => self.client.get(target),
        }
        .header(reqwest::header::AUTHORIZATION, endpoint.bearer())
        .timeout(timeout);
        Box::pin(async move {
            let response = request.send().await.ok()?;
            if response.status().as_u16() != 200 {
                return None;
            }
            let bytes = response.bytes().await.ok()?;
            serde_json::from_slice(&bytes).ok()
        })
    }

    fn get_text(
        &self,
        endpoint: &Endpoint,
        path: &str,
        timeout: Duration,
    ) -> BoxFuture<'_, Option<String>> {
        let request = self
            .client
            .get(endpoint.url(path))
            .header(reqwest::header::AUTHORIZATION, endpoint.bearer())
            .timeout(timeout);
        Box::pin(async move {
            let response = request.send().await.ok()?;
            if response.status().as_u16() != 200 {
                return None;
            }
            response.text().await.ok()
        })
    }

    fn post(
        &self,
        endpoint: &Endpoint,
        path: &str,
        body: Value,
        timeout: Duration,
    ) -> BoxFuture<'_, Result<(u16, String), PostError>> {
        let request = self
            .client
            .post(endpoint.url(path))
            .header(reqwest::header::AUTHORIZATION, endpoint.bearer())
            .json(&body)
            .timeout(timeout);
        Box::pin(async move {
            let map = |e: reqwest::Error| {
                if e.is_timeout() {
                    PostError::Timeout
                } else {
                    PostError::Transport(describe(&e))
                }
            };
            let response = request.send().await.map_err(map)?;
            let status = response.status().as_u16();
            let text = response.text().await.map_err(map)?;
            Ok((status, text))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_keys_are_long_random_and_hex() {
        let a = generate_api_key().unwrap();
        let b = generate_api_key().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn the_endpoint_debug_output_hides_the_key() {
        let endpoint = Endpoint::new(8221, "secret-key");
        let text = format!("{endpoint:?}");
        assert!(text.contains("8221"));
        assert!(!text.contains("secret-key"));
        assert_eq!(endpoint.bearer(), "Bearer secret-key");
        assert_eq!(endpoint.url("/health"), "http://127.0.0.1:8221/health");
    }

    #[test]
    fn requests_in_flight_reads_the_processing_and_deferred_gauges() {
        let text = "# HELP llamacpp:requests_processing Number of requests processing.\n\
                    # TYPE llamacpp:requests_processing gauge\n\
                    llamacpp:requests_processing 1\n\
                    llamacpp:requests_deferred 2\n\
                    llamacpp:prompt_tokens_total 40\n";
        assert_eq!(requests_in_flight(text), Some(3));
        assert_eq!(
            requests_in_flight("llamacpp:requests_processing 0\nllamacpp:requests_deferred 0"),
            Some(0)
        );
        assert_eq!(
            requests_in_flight("llamacpp:n_busy_slots_per_decode 1"),
            None
        );
        assert_eq!(requests_in_flight(""), None);
        assert_eq!(requests_in_flight("llamacpp:requests_processing nan"), None);
    }
}
