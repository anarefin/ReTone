//! Test helpers: a tiny HTTP/1.1 mock server on 127.0.0.1 (one request per
//! connection) for the download and llama-server paths.

use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

pub enum Reply {
    Full {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Promises `body.len()` bytes, sends `send` of them, then closes.
    Truncated {
        status: u16,
        body: Vec<u8>,
        send: usize,
    },
    /// Promises `length` bytes, sends `send` of them, then never more.
    Stall {
        status: u16,
        length: usize,
        send: usize,
    },
    /// Waits before replying (a slow generation).
    Delayed(Duration, Box<Reply>),
}

impl Reply {
    pub fn bytes(status: u16, body: Vec<u8>) -> Self {
        Self::Full {
            status,
            headers: Vec::new(),
            body,
        }
    }

    pub fn json(status: u16, body: Value) -> Self {
        Self::Full {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.to_string().into_bytes(),
        }
    }

    pub fn truncated(status: u16, body: Vec<u8>, send: usize) -> Self {
        Self::Truncated { status, body, send }
    }

    pub fn stall(status: u16, length: usize, send: usize) -> Self {
        Self::Stall {
            status,
            length,
            send,
        }
    }

    pub fn delayed(self, delay: Duration) -> Self {
        Self::Delayed(delay, Box::new(self))
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        if let Self::Full { headers, .. } = &mut self {
            headers.push((name.into(), value.into()));
        }
        self
    }
}

type Handler = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

pub struct MockServer {
    pub port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockServer {
    pub async fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler: Handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, handler).await;
                });
            }
        });
        Self { port, task }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

async fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_string();
    let path = first.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok(Some(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

fn head(status: u16, length: usize, headers: &[(String, String)]) -> Vec<u8> {
    let mut out =
        format!("HTTP/1.1 {status} X\r\nContent-Length: {length}\r\nConnection: close\r\n");
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.into_bytes()
}

async fn serve(mut stream: TcpStream, handler: Handler) -> std::io::Result<()> {
    let Some(request) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let mut reply = handler(&request);
    loop {
        match reply {
            Reply::Delayed(delay, inner) => {
                tokio::time::sleep(delay).await;
                reply = *inner;
            }
            Reply::Full {
                status,
                headers,
                body,
            } => {
                stream
                    .write_all(&head(status, body.len(), &headers))
                    .await?;
                stream.write_all(&body).await?;
                stream.flush().await?;
                return Ok(());
            }
            Reply::Truncated { status, body, send } => {
                stream.write_all(&head(status, body.len(), &[])).await?;
                stream.write_all(&body[..send]).await?;
                stream.flush().await?;
                return Ok(());
            }
            Reply::Stall {
                status,
                length,
                send,
            } => {
                stream.write_all(&head(status, length, &[])).await?;
                stream.write_all(&vec![0u8; send]).await?;
                stream.flush().await?;
                tokio::time::sleep(Duration::from_secs(3600)).await;
                return Ok(());
            }
        }
    }
}

// --- fakes for the server lifecycle -----------------------------------------

use crate::client::{Endpoint, PostError, ServerClient};
use crate::process::{BoxFuture, ExitInfo, ProcessHandle, ProcessSpawner, SpawnSpec};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::sync::{oneshot, watch};

/// What a fake spawn does.
#[derive(Debug, Clone)]
pub enum FakeOutcome {
    /// Runs until signalled.
    Live,
    /// Exits at once, as a crash during startup.
    Dies {
        code: Option<i32>,
        signal: Option<i32>,
        stderr: String,
    },
    /// The spawn itself fails.
    SpawnError(String),
}

type Behaviour = Arc<dyn Fn(&SpawnSpec) -> FakeOutcome + Send + Sync>;

/// Records every spawn and every signal; processes end on the first signal.
pub struct FakeSpawner {
    pub specs: Mutex<Vec<SpawnSpec>>,
    pub signals: Mutex<Vec<(u32, i32)>>,
    behaviour: Mutex<Behaviour>,
    gate: Mutex<Option<watch::Receiver<bool>>>,
    next_pid: AtomicU32,
    /// When false, processes ignore SIGTERM (only SIGKILL ends them).
    pub honour_sigterm: AtomicBool,
}

impl Default for FakeSpawner {
    fn default() -> Self {
        Self {
            specs: Mutex::new(Vec::new()),
            signals: Mutex::new(Vec::new()),
            behaviour: Mutex::new(Arc::new(|_| FakeOutcome::Live)),
            gate: Mutex::new(None),
            next_pid: AtomicU32::new(900_000),
            honour_sigterm: AtomicBool::new(true),
        }
    }
}

impl FakeSpawner {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_behaviour(&self, f: impl Fn(&SpawnSpec) -> FakeOutcome + Send + Sync + 'static) {
        *self.behaviour.lock().unwrap() = Arc::new(f);
    }

    /// Holds every later spawn until the returned sender sends `true`.
    pub fn hold(&self) -> watch::Sender<bool> {
        let (tx, rx) = watch::channel(false);
        *self.gate.lock().unwrap() = Some(rx);
        tx
    }

    pub fn clear(&self) {
        self.specs.lock().unwrap().clear();
        self.signals.lock().unwrap().clear();
    }

    pub fn spawn_count(&self) -> usize {
        self.specs.lock().unwrap().len()
    }

    /// Context sizes of every spawn, in order.
    pub fn contexts(&self) -> Vec<u64> {
        self.specs
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.arg_value("--ctx-size").unwrap().parse().unwrap())
            .collect()
    }

    pub fn sigterms(&self) -> usize {
        self.signals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| *s == libc::SIGTERM)
            .count()
    }
}

impl ProcessSpawner for Arc<FakeSpawner> {
    fn spawn(&self, spec: SpawnSpec) -> BoxFuture<'static, std::io::Result<ProcessHandle>> {
        let this = self.clone();
        Box::pin(async move {
            let gate = this.gate.lock().unwrap().clone();
            if let Some(mut gate) = gate {
                let _ = gate.wait_for(|open| *open).await;
            }
            let outcome = (this.behaviour.lock().unwrap().clone())(&spec);
            this.specs.lock().unwrap().push(spec);
            let pid = this.next_pid.fetch_add(1, Ordering::SeqCst);
            let (exit_tx, exit_rx) = watch::channel(None);
            let stderr = Arc::new(Mutex::new(String::new()));
            match outcome {
                FakeOutcome::SpawnError(message) => return Err(std::io::Error::other(message)),
                FakeOutcome::Dies {
                    code,
                    signal,
                    stderr: text,
                } => {
                    *stderr.lock().unwrap() = text;
                    exit_tx.send_replace(Some(ExitInfo { code, signal }));
                }
                FakeOutcome::Live => {}
            }
            let exit_tx = Arc::new(exit_tx);
            let recorder = this.clone();
            let signaller = Arc::new(move |signal: i32| {
                recorder.signals.lock().unwrap().push((pid, signal));
                if signal == libc::SIGKILL || recorder.honour_sigterm.load(Ordering::SeqCst) {
                    exit_tx.send_replace(Some(ExitInfo {
                        code: None,
                        signal: Some(signal),
                    }));
                }
            });
            Ok(ProcessHandle::new(pid, exit_rx, stderr, signaller))
        })
    }
}

/// A scripted llama-server: health and `/metrics` without sockets.
pub struct FakeClient {
    pub healthy: AtomicBool,
    pub processing: AtomicBool,
    /// When set, `/metrics` waits for this answer.
    pub pending_metrics: Mutex<Option<oneshot::Receiver<Option<String>>>>,
    /// When true, `/metrics` fails.
    pub metrics_fail: AtomicBool,
    /// Every endpoint a call was made with, in order.
    pub endpoints: Mutex<Vec<Endpoint>>,
}

impl Default for FakeClient {
    fn default() -> Self {
        Self {
            healthy: AtomicBool::new(true),
            processing: AtomicBool::new(false),
            pending_metrics: Mutex::new(None),
            metrics_fail: AtomicBool::new(false),
            endpoints: Mutex::new(Vec::new()),
        }
    }
}

impl FakeClient {
    fn record(&self, endpoint: &Endpoint) {
        self.endpoints.lock().unwrap().push(endpoint.clone());
    }
}

impl ServerClient for FakeClient {
    fn health(&self, endpoint: &Endpoint, _timeout: Duration) -> BoxFuture<'_, bool> {
        self.record(endpoint);
        let healthy = self.healthy.load(Ordering::SeqCst);
        Box::pin(async move { healthy })
    }

    fn request_json(
        &self,
        endpoint: &Endpoint,
        _path: &str,
        _body: Option<Value>,
        _timeout: Duration,
    ) -> BoxFuture<'_, Option<Value>> {
        self.record(endpoint);
        Box::pin(async { None })
    }

    fn get_text(
        &self,
        endpoint: &Endpoint,
        path: &str,
        _timeout: Duration,
    ) -> BoxFuture<'_, Option<String>> {
        self.record(endpoint);
        let is_metrics = path == "/metrics";
        Box::pin(async move {
            if !is_metrics || self.metrics_fail.load(Ordering::SeqCst) {
                return None;
            }
            let pending = self.pending_metrics.lock().unwrap().take();
            if let Some(answer) = pending {
                return answer.await.ok().flatten();
            }
            let processing = u8::from(self.processing.load(Ordering::SeqCst));
            Some(format!(
                "# TYPE llamacpp:requests_processing gauge\n\
                 llamacpp:requests_processing {processing}\n\
                 llamacpp:requests_deferred 0\n"
            ))
        })
    }

    fn post(
        &self,
        endpoint: &Endpoint,
        _path: &str,
        _body: Value,
        _timeout: Duration,
    ) -> BoxFuture<'_, Result<(u16, String), PostError>> {
        self.record(endpoint);
        Box::pin(async { Err(PostError::Transport("no server".into())) })
    }
}

/// Runs queued tasks until the runtime is idle (a paused clock never advances
/// on its own here; tests move it with `tokio::time::advance`).
pub async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}
