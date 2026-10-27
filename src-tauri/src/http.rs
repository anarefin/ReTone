//! HTTP from the main process: the connection check and the Anthropic proxy.
//! `reqwest` with rustls and `rustls-platform-verifier`, so the macOS trust
//! store (including corporate roots) decides TLS. Renderer calls go through `@tauri-apps/plugin-http` instead.

use retone_core::providers::connection_test::{
    ProbeError, ProbeResponse, ProbeTransport, ProviderRequest, RedirectMode,
};
use serde_json::Value;
use std::time::Duration;

/// Shared clients, one per redirect policy (reqwest sets it per client).
pub struct HttpClients {
    /// Fails on any redirect: a hosted provider never redirects, and following
    /// one would hand its key header to another host.
    pub no_redirect: reqwest::Client,
    /// Follows up to 10 redirects; reqwest drops `Authorization` across hosts.
    pub follow: reqwest::Client,
}

fn user_agent() -> String {
    format!("ReTone/{}", env!("CARGO_PKG_VERSION"))
}

impl HttpClients {
    pub fn new() -> Result<Self, reqwest::Error> {
        let refuse = reqwest::redirect::Policy::custom(|attempt| attempt.error("redirect refused"));
        Ok(Self {
            no_redirect: reqwest::Client::builder()
                .user_agent(user_agent())
                .redirect(refuse)
                .build()?,
            follow: reqwest::Client::builder()
                .user_agent(user_agent())
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()?,
        })
    }

    pub fn for_mode(&self, mode: RedirectMode) -> &reqwest::Client {
        match mode {
            RedirectMode::Error => &self.no_redirect,
            RedirectMode::Follow => &self.follow,
        }
    }
}

fn probe_error(error: &reqwest::Error) -> ProbeError {
    if error.is_timeout() {
        ProbeError::Timeout
    } else {
        ProbeError::Network
    }
}

/// The connection check's transport. `timeout` covers connecting through the
/// last body byte (reqwest's per-request timeout).
pub struct ReqwestTransport<'a> {
    pub clients: &'a HttpClients,
    pub timeout: Duration,
}

pub struct ReqwestProbeResponse(reqwest::Response);

impl ProbeResponse for ReqwestProbeResponse {
    fn status(&self) -> u16 {
        self.0.status().as_u16()
    }

    async fn json(self) -> Result<Option<Value>, ProbeError> {
        let bytes = self.0.bytes().await.map_err(|e| probe_error(&e))?;
        Ok(serde_json::from_slice(&bytes).ok())
    }
}

impl ProbeTransport for ReqwestTransport<'_> {
    type Response = ReqwestProbeResponse;

    async fn get(
        &self,
        url: &str,
        request: &ProviderRequest,
    ) -> Result<ReqwestProbeResponse, ProbeError> {
        let mut builder = self
            .clients
            .for_mode(request.redirect)
            .get(url)
            .timeout(self.timeout);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        builder
            .send()
            .await
            .map(ReqwestProbeResponse)
            .map_err(|e| probe_error(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{Reply, serve};
    use retone_core::providers::connection_test::test_provider_connection;
    use serde_json::json;

    fn transport(clients: &HttpClients, timeout: Duration) -> ReqwestTransport<'_> {
        ReqwestTransport { clients, timeout }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[tokio::test]
    async fn a_custom_server_check_passes_and_sends_the_key() {
        let server = serve(|req| match req.path.as_str() {
            "/v1/models" => Reply::json(200, json!({ "data": [{ "id": "local-model" }] })),
            _ => Reply::status(404),
        })
        .await;
        let clients = HttpClients::new().unwrap();
        let config = json!({
            "provider": "custom",
            "baseUrl": server.url.clone(),
            "apiKey": "sk-local",
            "model": "local-model",
        });
        let result = test_provider_connection(
            &config,
            &transport(&clients, Duration::from_secs(5)),
            no_env,
        )
        .await;
        assert_eq!(result.to_json(), json!({ "success": true }));
        let requests = server.requests();
        let paths: Vec<_> = requests.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["/models", "/v1/models"]);
        assert_eq!(requests[1].header("authorization"), Some("Bearer sk-local"));
        assert_eq!(requests[1].header("accept"), Some("application/json"));
    }

    #[tokio::test]
    async fn a_rejected_key_and_a_down_server_map_to_their_codes() {
        let server = serve(|_| Reply::status(401)).await;
        let clients = HttpClients::new().unwrap();
        let t = transport(&clients, Duration::from_secs(5));
        let config = json!({ "provider": "custom", "baseUrl": format!("{}/v1", server.url), "apiKey": "bad" });
        let result = test_provider_connection(&config, &t, no_env)
            .await
            .to_json();
        assert_eq!(result["errorCode"], "credentialsRejected");
        assert_eq!(result["status"], 401);

        let closed = crate::test_server::closed_port_url().await;
        let config = json!({ "provider": "custom", "baseUrl": format!("{closed}/v1") });
        let result = test_provider_connection(&config, &t, no_env)
            .await
            .to_json();
        assert_eq!(result["errorCode"], "network");
        assert_eq!(result["unverified"], true);
    }

    #[tokio::test]
    async fn the_cap_covers_the_whole_answer_body_included() {
        let server = serve(|_| Reply::StallBody { status: 200 }).await;
        let clients = HttpClients::new().unwrap();
        let config =
            json!({ "provider": "custom", "baseUrl": format!("{}/v1", server.url), "model": "m" });
        let started = std::time::Instant::now();
        let result = test_provider_connection(
            &config,
            &transport(&clients, Duration::from_millis(300)),
            no_env,
        )
        .await
        .to_json();
        assert_eq!(result["errorCode"], "timeout");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_refused_redirect_never_reaches_the_other_host() {
        let target = serve(|_| Reply::json(200, json!({}))).await;
        let target_url = target.url.clone();
        let origin = serve(move |_| Reply::redirect(302, &format!("{target_url}/v1/models"))).await;
        let clients = HttpClients::new().unwrap();
        let t = transport(&clients, Duration::from_secs(5));
        let request = ProviderRequest {
            endpoints: vec![format!("{}/v1/models", origin.url)],
            headers: vec![("x-api-key".into(), "sk-ant-test".into())],
            redirect: RedirectMode::Error,
        };
        let outcome = t.get(request.endpoint(), &request).await;
        assert!(matches!(outcome, Err(ProbeError::Network)));
        assert!(target.requests().is_empty());

        // The same request allowed to follow does reach it, so this test can fail.
        let follow = ProviderRequest {
            redirect: RedirectMode::Follow,
            ..request
        };
        let response = t.get(follow.endpoint(), &follow).await.ok().unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(target.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_custom_server_can_still_redirect_within_itself() {
        let server = serve(|req| match req.path.as_str() {
            "/v1/models" => Reply::redirect(308, "/v1/models/"),
            _ => Reply::json(200, json!({ "data": [{ "id": "local-model" }] })),
        })
        .await;
        let clients = HttpClients::new().unwrap();
        let config = json!({ "provider": "custom", "baseUrl": format!("{}/v1", server.url), "model": "local-model" });
        let result = test_provider_connection(
            &config,
            &transport(&clients, Duration::from_secs(5)),
            no_env,
        )
        .await;
        assert_eq!(result.to_json(), json!({ "success": true }));
    }
}
