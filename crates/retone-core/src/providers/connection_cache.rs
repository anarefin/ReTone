//! Shares connection checks between Settings panels. Identical checks share one request while it
//! runs, and a Connected answer is reused for ten minutes. A failure is never
//! reused, so the next ask (Retry included) reaches the provider again. Only a
//! SHA-256 digest of each check is kept, never a credential.

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const CONNECTED_TTL: Duration = Duration::from_secs(10 * 60);

/// Everything that decides a check's answer.
const CHECK_FIELDS: [&str; 5] = ["scope", "provider", "apiKey", "baseUrl", "model"];

fn check_key(config: &Value) -> String {
    let fields: Vec<&Value> = CHECK_FIELDS
        .iter()
        .map(|f| config.get(*f).unwrap_or(&Value::Null))
        .collect();
    let digest = Sha256::digest(serde_json::to_vec(&fields).unwrap_or_default());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A check's outcome. `Err` is a check that failed to run (never cached).
pub type CheckOutcome = Result<Value, String>;
type Running = Shared<BoxFuture<'static, CheckOutcome>>;

#[derive(Default)]
struct Inner {
    in_flight: HashMap<String, Running>,
    connected_until: HashMap<String, Duration>,
}

/// Milliseconds-resolution monotonic clock, injectable for tests.
pub type Clock = Arc<dyn Fn() -> Duration + Send + Sync>;

#[derive(Clone)]
pub struct ConnectionCheckCache {
    ttl: Duration,
    now: Clock,
    inner: Arc<Mutex<Inner>>,
}

impl Default for ConnectionCheckCache {
    fn default() -> Self {
        let start = Instant::now();
        Self::new(CONNECTED_TTL, Arc::new(move || start.elapsed()))
    }
}

fn is_success(outcome: &CheckOutcome) -> bool {
    matches!(outcome, Ok(v) if v.get("success") == Some(&Value::Bool(true)))
}

impl ConnectionCheckCache {
    pub fn new(ttl: Duration, now: Clock) -> Self {
        Self {
            ttl,
            now,
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Answers `config` from the cache, joins an identical check in flight, or
    /// runs `check`.
    pub fn check<F, Fut>(
        &self,
        config: &Value,
        check: F,
    ) -> impl Future<Output = CheckOutcome> + Send + 'static
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = CheckOutcome> + Send + 'static,
    {
        let key = check_key(config);
        let mut inner = self.inner.lock().expect("connection cache lock");
        let now = (self.now)();
        if inner
            .connected_until
            .get(&key)
            .is_some_and(|until| now < *until)
        {
            return futures::future::ready(Ok(serde_json::json!({ "success": true })))
                .left_future();
        }
        inner.connected_until.remove(&key);
        let running = match inner.in_flight.get(&key) {
            Some(running) => running.clone(),
            None => {
                let shared_inner = Arc::clone(&self.inner);
                let clock = Arc::clone(&self.now);
                let ttl = self.ttl;
                let entry_key = key.clone();
                let running: Running = async move {
                    let outcome = check().await;
                    let mut inner = shared_inner.lock().expect("connection cache lock");
                    if is_success(&outcome) {
                        inner
                            .connected_until
                            .insert(entry_key.clone(), clock() + ttl);
                    }
                    inner.in_flight.remove(&entry_key);
                    outcome
                }
                .boxed()
                .shared();
                inner.in_flight.insert(key, running.clone());
                running
            }
        };
        running.right_future()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    fn config() -> Value {
        json!({ "scope": "toneRewrite", "provider": "openai", "apiKey": "sk-made-up", "model": "gpt-test" })
    }

    fn pass() -> Value {
        json!({ "success": true })
    }

    fn rejected() -> Value {
        json!({ "success": false, "errorCode": "credentialsRejected", "status": 401 })
    }

    struct Setup {
        cache: ConnectionCheckCache,
        clock: Arc<AtomicU64>,
        sent: Arc<AtomicUsize>,
    }

    fn setup() -> Setup {
        let clock = Arc::new(AtomicU64::new(0));
        let reader = Arc::clone(&clock);
        let cache = ConnectionCheckCache::new(
            CONNECTED_TTL,
            Arc::new(move || Duration::from_millis(reader.load(Ordering::SeqCst))),
        );
        Setup {
            cache,
            clock,
            sent: Arc::new(AtomicUsize::new(0)),
        }
    }

    impl Setup {
        async fn ask(&self, config: &Value, answer: Value) -> CheckOutcome {
            let sent = Arc::clone(&self.sent);
            self.cache
                .check(config, move || async move {
                    sent.fetch_add(1, Ordering::SeqCst);
                    Ok(answer)
                })
                .await
        }
        fn advance(&self, ms: u64) {
            self.clock.fetch_add(ms, Ordering::SeqCst);
        }
        fn sent(&self) -> usize {
            self.sent.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn identical_checks_in_flight_share_one_request() {
        let s = setup();
        let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
        let rx = Arc::new(Mutex::new(Some(rx)));
        let calls = Arc::new(AtomicUsize::new(0));
        let make = || {
            let rx = Arc::clone(&rx);
            let calls = Arc::clone(&calls);
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let rx = rx.lock().unwrap().take().expect("asked once");
                Ok(rx.await.unwrap())
            }
        };
        let first = s.cache.check(&config(), make());
        let second = s.cache.check(&config(), make());
        tx.send(pass()).unwrap();
        let (a, b) = futures::join!(first, second);
        assert_eq!(a.unwrap(), pass());
        assert_eq!(b.unwrap(), pass());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn every_part_of_a_check_is_part_of_its_identity() {
        let s = setup();
        let base = config();
        let mut variants = vec![base.clone()];
        for (field, value) in [
            ("scope", "other"),
            ("provider", "groq"),
            ("apiKey", "sk-other"),
            ("baseUrl", "http://127.0.0.1:1234/v1"),
            ("model", "gpt-other"),
        ] {
            let mut v = base.clone();
            v[field] = json!(value);
            variants.push(v);
        }
        for v in &variants {
            s.ask(v, pass()).await.unwrap();
        }
        assert_eq!(s.sent(), variants.len());
    }

    #[tokio::test]
    async fn connected_is_reused_for_ten_minutes_then_asked_again() {
        let s = setup();
        s.ask(&config(), pass()).await.unwrap();
        s.advance(CONNECTED_TTL.as_millis() as u64 - 1);
        assert_eq!(s.ask(&config(), pass()).await.unwrap(), pass());
        assert_eq!(s.sent(), 1, "a remembered Connected sends nothing");
        s.advance(1);
        s.ask(&config(), pass()).await.unwrap();
        assert_eq!(s.sent(), 2);
    }

    #[tokio::test]
    async fn a_failure_is_never_reused() {
        let s = setup();
        assert_eq!(s.ask(&config(), rejected()).await.unwrap(), rejected());
        assert_eq!(s.ask(&config(), pass()).await.unwrap(), pass());
        assert_eq!(s.sent(), 2);
    }

    #[tokio::test]
    async fn a_check_that_errors_is_forgotten_and_the_next_ask_goes_out() {
        let s = setup();
        let broken = s
            .cache
            .check(&config(), || async { Err("bridge failed".to_string()) })
            .await;
        assert!(broken.is_err());
        s.ask(&config(), pass()).await.unwrap();
        assert_eq!(s.sent(), 1);
    }

    #[test]
    fn keys_are_digests_not_credentials() {
        let key = check_key(&config());
        assert_eq!(key.len(), 64);
        assert!(!key.contains("sk-made-up"));
    }
}
