//! The llama-server lifecycle: a free port
//! in 8221–8240, `--jinja`, Metal with `--n-gpu-layers 99`, a bounded
//! `--cache-ram`, an optional MTP drafter, `DYLD_LIBRARY_PATH` next to the
//! binary, 120 s startup polled every 500 ms, a 5 s health check that marks the
//! server not ready after 3 failures, and a 5-min idle stop unless the user
//! keeps the model loaded. The context only grows while a server runs, so a
//! short request never shrinks a window a long one paid to open.
//!
//! Hardening: each launch gets a random API key (passed in `LLAMA_API_KEY`,
//! not argv, so `ps` never shows it), `/slots` (which echoes prompts) is
//! disabled, the web UI is off, and the idle check reads `/metrics` instead.

use crate::client::{Endpoint, PostError, ServerClient, generate_api_key, requests_in_flight};
use crate::context::BASELINE_CONTEXT_SIZE;
use crate::process::{ProcessHandle, ProcessSpawner, SpawnSpec, signal_name};
use crate::sidecar::PidFile;
use serde::Serialize;
use serde_json::{Value, json};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

pub const PORT_RANGE_START: u16 = 8221;
pub const PORT_RANGE_END: u16 = 8240;
/// The context a start without an explicit size asks for.
pub const DEFAULT_CONTEXT_SIZE: u64 = 4096;
pub const SIDECAR_NAME: &str = "llama";
/// Binary names, most specific first.
pub const BINARY_NAMES: [&str; 2] = ["llama-server-darwin-arm64", "llama-server"];

#[derive(Debug, Clone, Copy)]
pub struct ServerTimings {
    pub startup_timeout: Duration,
    pub startup_poll: Duration,
    pub health_interval: Duration,
    pub health_timeout: Duration,
    pub health_failure_threshold: u32,
    pub idle_timeout: Duration,
    pub preflight_timeout: Duration,
    pub inference_timeout: Duration,
    pub stop_grace: Duration,
}

impl Default for ServerTimings {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(120),
            startup_poll: Duration::from_millis(500),
            health_interval: Duration::from_secs(5),
            health_timeout: Duration::from_secs(2),
            health_failure_threshold: 3,
            idle_timeout: Duration::from_secs(5 * 60),
            preflight_timeout: Duration::from_secs(10),
            inference_timeout: Duration::from_secs(300),
            stop_grace: Duration::from_secs(5),
        }
    }
}

pub type PortPicker = Arc<dyn Fn() -> io::Result<u16> + Send + Sync>;

/// Everything the server touches outside itself.
pub struct ServerDeps {
    pub spawner: Arc<dyn ProcessSpawner>,
    pub client: Arc<dyn ServerClient>,
    pub pick_port: PortPicker,
    /// Directories searched for the binary, in order.
    pub binary_dirs: Vec<PathBuf>,
    pub pid_file: Option<PidFile>,
    pub timings: ServerTimings,
    /// Where timers and watchers run (commands may call in from the main
    /// thread, outside any runtime).
    pub runtime: tokio::runtime::Handle,
    /// Resolves to `true` once stale sidecars were reaped; nothing spawns before.
    pub spawn_gate: Option<watch::Receiver<bool>>,
}

/// What a start asks for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StartOptions {
    pub context_size: Option<u64>,
    pub threads: Option<u32>,
    pub gpu_layers: Option<u32>,
    pub cache_ram_mib: Option<u64>,
    pub draft_model_path: Option<PathBuf>,
}

/// `llama-server-status` as the renderer reads it (`LlamaServerStatus`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub available: bool,
    pub running: bool,
    pub port: Option<u16>,
    /// The running server's API key: the live preview sends it as a Bearer
    /// token when it streams to `port` directly.
    pub api_key: Option<String>,
    pub model_path: Option<String>,
    pub model_name: Option<String>,
    pub backend: Option<String>,
    pub gpu_accelerated: bool,
}

/// A failure from the server, typed where the renderer translates it.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerError {
    pub message: String,
    pub code: Option<&'static str>,
    pub needed_tokens: Option<u64>,
    pub max_context_tokens: Option<u64>,
}

impl ServerError {
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            needed_tokens: None,
            max_context_tokens: None,
        }
    }

    fn coded(message: &str, code: &'static str) -> Self {
        Self {
            code: Some(code),
            ..Self::plain(message)
        }
    }
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ServerError {}

/// A context overflow (400 with llama.cpp's JSON) becomes `CONTEXT_TOO_LARGE`
/// with the two numbers; anything else keeps the status and body.
pub fn response_error(status: u16, body: &str) -> ServerError {
    if status == 400
        && let Some(error) = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v.get("error").cloned())
    {
        let message = error.get("message").and_then(Value::as_str).unwrap_or("");
        let overflow = error.get("type").and_then(Value::as_str)
            == Some("exceed_context_size_error")
            || message
                .to_ascii_lowercase()
                .contains("exceeds the available context size");
        if overflow {
            let number = |key: &str| {
                error
                    .get(key)
                    .and_then(Value::as_f64)
                    .filter(|n| *n != 0.0 && n.is_finite())
                    .map(|n| n as u64)
            };
            let needed = number("n_prompt_tokens");
            let max = number("n_ctx");
            let text = match (needed, max) {
                (Some(n), Some(m)) => format!(
                    "The request needs {n} tokens of context but the model is running with {m}."
                ),
                _ => "The request is longer than the context this model is running with.".into(),
            };
            return ServerError {
                message: text,
                code: Some("CONTEXT_TOO_LARGE"),
                needed_tokens: needed,
                max_context_tokens: max,
            };
        }
    }
    ServerError::plain(format!("llama-server returned status {status}: {body}"))
}

/// One chat-completions call.
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    /// `None` counts as "suppress thinking", like the JS `!== false`.
    pub disable_thinking: Option<bool>,
    pub require_complete_output: bool,
    pub response_format: Option<Value>,
}

impl ChatOptions {
    fn suppress_thinking(&self) -> bool {
        self.disable_thinking != Some(false)
    }
}

#[derive(Default)]
struct State {
    keep_resident: bool,
    process: Option<Arc<ProcessHandle>>,
    process_generation: u64,
    port: Option<u16>,
    api_key: Option<Arc<str>>,
    ready: bool,
    model_path: Option<PathBuf>,
    /// The drafter requested (stable across identical requests).
    draft_model_path: Option<PathBuf>,
    /// The drafter that actually loaded.
    active_draft_model_path: Option<PathBuf>,
    /// The context that actually loaded.
    active_context_size: Option<u64>,
    /// The context of the running server; grows on demand, reset by `stop`.
    context_size: Option<u64>,
    starting: bool,
    health_task: Option<JoinHandle<()>>,
    health_failures: u32,
    idle_task: Option<JoinHandle<()>>,
    idle_generation: u64,
    backend: Option<&'static str>,
    cached_binary: Option<PathBuf>,
    active_request: Option<(u64, oneshot::Sender<()>)>,
    request_counter: u64,
}

struct Inner {
    deps: ServerDeps,
    state: Mutex<State>,
    start_lock: tokio::sync::Mutex<()>,
}

/// The shared llama-server manager. Cheap to clone.
#[derive(Clone)]
pub struct LlamaServer {
    inner: Arc<Inner>,
}

fn file_stem_without_gguf(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(
        name.strip_suffix(".gguf")
            .map(str::to_string)
            .unwrap_or(name),
    )
}

/// `args` with `--ctx-size` rewritten.
pub fn with_context_size(args: &[String], context_size: u64) -> Vec<String> {
    let mut out = args.to_vec();
    if let Some(i) = out.iter().position(|a| a == "--ctx-size")
        && i + 1 < out.len()
    {
        out[i + 1] = context_size.to_string();
    }
    out
}

/// The argument list for one start (Metal).
pub fn build_args(model_path: &Path, port: u16, options: &StartOptions) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--model".into(),
        model_path.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--threads".into(),
        options.threads.unwrap_or(4).to_string(),
        // Unset, llama-server takes the model's full trained context, whose KV
        // cache can exceed RAM.
        "--ctx-size".into(),
        options
            .context_size
            .unwrap_or(DEFAULT_CONTEXT_SIZE)
            .to_string(),
        "--jinja".into(),
        // `/slots` returns every slot's prompt, i.e. the user's text.
        "--no-slots".into(),
        // The idle stop reads `requests_processing` here instead.
        "--metrics".into(),
        "--no-webui".into(),
        // `/v1/models` answers without the key; the alias keeps the model's
        // path (and so the user's home directory) out of it.
        "--alias".into(),
        "local".into(),
    ];
    // Otherwise it reserves 8 GiB for the prompt cache on top of the KV cache.
    if let Some(mib) = options.cache_ram_mib.filter(|m| *m > 0) {
        args.extend(["--cache-ram".into(), mib.to_string()]);
    }
    args.extend([
        "--n-gpu-layers".into(),
        options.gpu_layers.unwrap_or(99).to_string(),
    ]);
    if let Some(draft) = &options.draft_model_path {
        args.extend([
            "--model-draft".into(),
            draft.display().to_string(),
            "--spec-type".into(),
            "draft-mtp".into(),
            "--spec-draft-n-max".into(),
            "3".into(),
        ]);
    }
    args
}

/// The environment variable carrying the per-launch API key (llama-server's
/// own name for `--api-key`); argv would show the key to every user via `ps`.
pub const API_KEY_ENV: &str = "LLAMA_API_KEY";

/// The environment additions for a binary in `bin_dir`.
pub fn build_env(
    bin_dir: &Path,
    existing_dyld: Option<&str>,
    fit: Option<&str>,
) -> Vec<(String, String)> {
    let dyld = match existing_dyld.filter(|s| !s.is_empty()) {
        Some(existing) => format!("{}:{existing}", bin_dir.display()),
        None => bin_dir.display().to_string(),
    };
    vec![
        ("DYLD_LIBRARY_PATH".into(), dyld),
        // Disables llama.cpp's memory auto-fit probing (~70 s of startup).
        ("LLAMA_ARG_FIT".into(), fit.unwrap_or("off").to_string()),
    ]
}

impl LlamaServer {
    pub fn new(deps: ServerDeps) -> Self {
        Self {
            inner: Arc::new(Inner {
                deps,
                state: Mutex::new(State::default()),
                start_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn timings(&self) -> ServerTimings {
        self.inner.deps.timings
    }

    // --- binary

    /// The binary to run. Only a hit is cached: a miss may be transient.
    pub fn binary_path(&self) -> Option<PathBuf> {
        if let Some(path) = self.state().cached_binary.clone() {
            return Some(path);
        }
        let found = self.inner.deps.binary_dirs.iter().find_map(|dir| {
            BINARY_NAMES
                .iter()
                .map(|name| dir.join(name))
                .find(|path| path.is_file())
        })?;
        self.state().cached_binary = Some(found.clone());
        Some(found)
    }

    pub fn is_available(&self) -> bool {
        self.binary_path().is_some()
    }

    // --- accessors

    pub fn is_ready(&self) -> bool {
        self.state().ready
    }

    pub fn has_process(&self) -> bool {
        self.state().process.is_some()
    }

    pub fn port(&self) -> Option<u16> {
        self.state().port
    }

    /// The port and API key of the current (or starting) server.
    pub fn endpoint(&self) -> Option<Endpoint> {
        Self::endpoint_of(&self.state())
    }

    fn endpoint_of(state: &State) -> Option<Endpoint> {
        Some(Endpoint::new(state.port?, state.api_key.clone()?))
    }

    pub fn context_size(&self) -> Option<u64> {
        self.state().context_size
    }

    pub fn status(&self) -> ServerStatus {
        let available = self.is_available();
        let state = self.state();
        ServerStatus {
            available,
            running: state.ready && state.process.is_some(),
            port: state.port,
            api_key: state.port.and(state.api_key.as_deref().map(str::to_string)),
            model_path: state.model_path.as_ref().map(|p| p.display().to_string()),
            model_name: state.model_path.as_deref().and_then(file_stem_without_gguf),
            backend: state.backend.map(str::to_string),
            gpu_accelerated: matches!(state.backend, Some("metal" | "vulkan")),
        }
    }

    pub fn reset_gpu_detection(&self) {
        let mut state = self.state();
        state.backend = None;
        state.cached_binary = None;
    }

    // --- start / stop

    /// Starts (or reuses) a server for `model_path`. A running server is reused
    /// when the model and drafter match and its context is at least the one
    /// asked for; that request also counts as activity for the idle timer.
    pub async fn start(&self, model_path: &Path, options: StartOptions) -> Result<(), ServerError> {
        let _serialized = self.inner.start_lock.lock().await;
        let requested_context = options.context_size.unwrap_or(DEFAULT_CONTEXT_SIZE);
        {
            let mut state = self.state();
            if state.ready
                && state.model_path.as_deref() == Some(model_path)
                && state.draft_model_path == options.draft_model_path
                && requested_context <= state.context_size.unwrap_or(0)
            {
                drop(state);
                // The live preview streams to the port directly; asking for the
                // running server is its only activity.
                self.reset_idle_timer();
                return Ok(());
            }
            state.starting = true;
        }

        if self.has_process() {
            self.stop().await;
        }
        // Residency may have been turned off while the old server stopped.
        self.clear_idle_timer();

        let result = self.do_start(model_path, &options).await;
        let mut state = self.state();
        state.starting = false;
        if result.is_ok() {
            state.context_size = Some(state.active_context_size.unwrap_or(requested_context));
        }
        result
    }

    async fn do_start(&self, model_path: &Path, options: &StartOptions) -> Result<(), ServerError> {
        if let Some(gate) = &self.inner.deps.spawn_gate {
            let mut gate = gate.clone();
            let _ = gate.wait_for(|reaped| *reaped).await;
        }
        let binary = self
            .binary_path()
            .ok_or_else(|| ServerError::plain("llama-server binary not found"))?;
        if !model_path.exists() {
            return Err(ServerError::plain(format!(
                "Model file not found: {}",
                model_path.display()
            )));
        }
        let port = (self.inner.deps.pick_port)().map_err(|e| ServerError::plain(e.to_string()))?;
        let api_key: Arc<str> = generate_api_key()
            .map_err(|e| {
                ServerError::plain(format!("Failed to generate the llama-server key: {e}"))
            })?
            .into();
        let endpoint = Endpoint::new(port, api_key.clone());
        let requested_context = options.context_size.unwrap_or(DEFAULT_CONTEXT_SIZE);
        {
            let mut state = self.state();
            state.port = Some(port);
            state.api_key = Some(api_key.clone());
            state.model_path = Some(model_path.to_path_buf());
            state.draft_model_path = options.draft_model_path.clone();
            state.active_draft_model_path = None;
            state.active_context_size = Some(requested_context);
        }

        let bin_dir = binary.parent().unwrap_or(Path::new("/")).to_path_buf();
        let mut env = build_env(
            &bin_dir,
            std::env::var("DYLD_LIBRARY_PATH").ok().as_deref(),
            std::env::var("LLAMA_ARG_FIT").ok().as_deref(),
        );
        env.push((API_KEY_ENV.into(), api_key.to_string()));
        let spec = SpawnSpec {
            program: binary,
            args: build_args(model_path, port, options),
            env,
            cwd: std::env::temp_dir(),
        };
        self.start_with_binary(spec, &endpoint).await?;

        {
            let mut state = self.state();
            // The bundled Metal build is the pinned version, so it never
            // rejects the drafter flags.
            state.backend = Some("metal");
            state.active_draft_model_path = state.draft_model_path.clone();
        }
        self.start_health_check();
        self.reset_idle_timer();
        tracing::info!(
            target: "llama",
            port,
            model = %file_stem_without_gguf(model_path).unwrap_or_default(),
            backend = "metal",
            mtp = options.draft_model_path.is_some(),
            context = requested_context,
            "llama-server started successfully"
        );
        Ok(())
    }

    async fn start_with_binary(
        &self,
        spec: SpawnSpec,
        endpoint: &Endpoint,
    ) -> Result<(), ServerError> {
        // `args` carry no secret (the key is in the environment, never logged).
        tracing::debug!(target: "llama", binary = %spec.program.display(), port = endpoint.port, args = ?spec.args, "Spawning llama-server");
        let handle = self
            .inner
            .deps
            .spawner
            .spawn(spec)
            .await
            .map_err(|e| ServerError::plain(format!("Failed to spawn llama-server: {e}")))?;
        let handle = Arc::new(handle);
        if let Some(pids) = &self.inner.deps.pid_file {
            pids.write(SIDECAR_NAME, handle.pid);
        }
        let generation = {
            let mut state = self.state();
            state.process_generation += 1;
            state.process = Some(handle.clone());
            state.ready = false;
            state.process_generation
        };
        self.watch_exit(handle.clone(), generation);

        let timings = self.timings();
        let started = Instant::now();
        let mut polls = 0u32;
        loop {
            if let Some(exit) = handle.exited() {
                return Err(died_during_startup(exit, &handle.stderr()));
            }
            polls += 1;
            if self
                .inner
                .deps
                .client
                .health(endpoint, timings.health_timeout)
                .await
            {
                self.state().ready = true;
                tracing::debug!(target: "llama", startup_ms = started.elapsed().as_millis() as u64, polls, "llama-server ready");
                return Ok(());
            }
            if started.elapsed() >= timings.startup_timeout {
                self.kill(&handle).await;
                return Err(ServerError::plain(format!(
                    "llama-server failed to start within {}ms",
                    timings.startup_timeout.as_millis()
                )));
            }
            tokio::time::sleep(timings.startup_poll).await;
        }
    }

    /// Records the exit of the current process: not ready, no process, no
    /// health check, pid file cleared. The context survives (only `stop`
    /// forgets it).
    fn watch_exit(&self, handle: Arc<ProcessHandle>, generation: u64) {
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let pid_file = self.inner.deps.pid_file.clone();
        self.inner.deps.runtime.spawn(async move {
            let exit = handle.wait().await;
            tracing::debug!(target: "llama", ?exit, "llama-server process exited");
            if let Some(pids) = pid_file {
                pids.clear(SIDECAR_NAME);
            }
            let Some(inner) = weak.upgrade() else { return };
            let server = LlamaServer { inner };
            let mut state = server.state();
            if state.process_generation == generation && state.process.is_some() {
                state.ready = false;
                state.process = None;
                if let Some(task) = state.health_task.take() {
                    task.abort();
                }
            }
        });
    }

    async fn kill(&self, handle: &ProcessHandle) {
        handle.signal(libc::SIGTERM);
        if tokio::time::timeout(self.timings().stop_grace, handle.wait())
            .await
            .is_err()
        {
            handle.signal(libc::SIGKILL);
            let _ = tokio::time::timeout(Duration::from_secs(1), handle.wait()).await;
        }
    }

    /// Stops the server (SIGTERM, SIGKILL after 5 s) and forgets everything
    /// about it, the grown context included.
    pub async fn stop(&self) {
        self.clear_idle_timer();
        let process = {
            let mut state = self.state();
            if let Some(task) = state.health_task.take() {
                task.abort();
            }
            match state.process.clone() {
                Some(process) => process,
                None => {
                    state.ready = false;
                    state.context_size = None;
                    return;
                }
            }
        };
        tracing::debug!(target: "llama", "Stopping llama-server");
        self.kill(&process).await;
        let mut state = self.state();
        // A start that ran meanwhile (an idle stop racing a restart) owns the
        // state now.
        if state
            .process
            .as_ref()
            .is_some_and(|current| !Arc::ptr_eq(current, &process))
        {
            return;
        }
        state.process = None;
        state.ready = false;
        state.port = None;
        state.api_key = None;
        state.model_path = None;
        state.draft_model_path = None;
        state.active_draft_model_path = None;
        state.backend = None;
        state.context_size = None;
        state.active_context_size = None;
    }

    // --- health

    fn start_health_check(&self) {
        let weak = Arc::downgrade(&self.inner);
        let timings = self.timings();
        let mut state = self.state();
        if let Some(task) = state.health_task.take() {
            task.abort();
        }
        state.health_failures = 0;
        // Deadlines are fixed now, not when the task first runs.
        let first = Instant::now() + timings.health_interval;
        state.health_task = Some(self.inner.deps.runtime.spawn(async move {
            let mut ticker = tokio::time::interval_at(first, timings.health_interval);
            loop {
                ticker.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let server = LlamaServer { inner };
                let endpoint = {
                    let state = server.state();
                    if state.process.is_none() {
                        return;
                    }
                    Self::endpoint_of(&state)
                };
                let healthy = match endpoint {
                    Some(endpoint) => {
                        server
                            .inner
                            .deps
                            .client
                            .health(&endpoint, timings.health_timeout)
                            .await
                    }
                    None => false,
                };
                let mut state = server.state();
                if healthy {
                    state.health_failures = 0;
                } else {
                    state.health_failures += 1;
                    if state.health_failures >= timings.health_failure_threshold {
                        tracing::warn!(target: "llama", failures = state.health_failures, "llama-server health check failed");
                        state.ready = false;
                    }
                }
            }
        }));
    }

    // --- idle

    /// The "Keep model loaded" preference. Windows resend it on load, so only
    /// a change touches the timer; a start in progress arms its own.
    pub fn set_keep_resident(&self, keep: bool) {
        let rearm = {
            let mut state = self.state();
            if state.keep_resident == keep {
                return;
            }
            state.keep_resident = keep;
            !keep && state.process.is_some() && !state.starting
        };
        if keep {
            self.clear_idle_timer();
        } else if rearm {
            self.reset_idle_timer();
        }
    }

    pub fn keep_resident(&self) -> bool {
        self.state().keep_resident
    }

    pub fn has_idle_timer(&self) -> bool {
        self.state().idle_task.is_some()
    }

    pub fn clear_idle_timer(&self) {
        let mut state = self.state();
        state.idle_generation += 1;
        if let Some(task) = state.idle_task.take() {
            task.abort();
        }
    }

    /// (Re)arms the 5-min idle stop, unless the model is kept resident.
    pub fn reset_idle_timer(&self) {
        let weak = Arc::downgrade(&self.inner);
        let timings = self.timings();
        let mut state = self.state();
        state.idle_generation += 1;
        if let Some(task) = state.idle_task.take() {
            task.abort();
        }
        if state.keep_resident {
            return;
        }
        let generation = state.idle_generation;
        let deadline = Instant::now() + timings.idle_timeout;
        state.idle_task = Some(self.inner.deps.runtime.spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let Some(inner) = weak.upgrade() else { return };
            let server = LlamaServer { inner };
            let endpoint = server.endpoint();
            // The live preview talks to the port directly: only the server
            // knows whether an answer is still being generated. (`/metrics`,
            // not `/slots`: the slots endpoint is disabled because it echoes
            // the prompt.)
            let metrics = match endpoint {
                Some(endpoint) => {
                    server
                        .inner
                        .deps
                        .client
                        .get_text(&endpoint, "/metrics", timings.preflight_timeout)
                        .await
                }
                None => None,
            };
            let model = {
                let mut state = server.state();
                if state.idle_generation != generation {
                    return;
                }
                // This task is the timer: detach rather than abort itself.
                state.idle_task.take();
                state.model_path.as_deref().and_then(file_stem_without_gguf)
            };
            let busy = metrics
                .as_deref()
                .and_then(requests_in_flight)
                .is_some_and(|n| n > 0);
            if busy {
                server.reset_idle_timer();
                return;
            }
            tracing::info!(target: "llama", timeout_ms = timings.idle_timeout.as_millis() as u64, ?model, "llama-server idle timeout reached, stopping to free memory");
            server.stop().await;
        }));
    }

    // --- measuring

    fn live_endpoint(&self) -> Option<Endpoint> {
        let state = self.state();
        if !(state.ready && state.process.is_some()) {
            return None;
        }
        Self::endpoint_of(&state)
    }

    /// The context actually in force (`/props`), or `None` when unreadable.
    pub async fn usable_context_size(&self) -> Option<u64> {
        let endpoint = self.live_endpoint()?;
        let props = self
            .inner
            .deps
            .client
            .request_json(&endpoint, "/props", None, self.timings().preflight_timeout)
            .await?;
        props
            .pointer("/default_generation_settings/n_ctx")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n > 0.0)
            .map(|n| n as u64)
    }

    /// Exact prompt tokens with the chat template applied (`/apply-template`
    /// then `/tokenize`), mirroring what [`inference`](Self::inference) sends.
    pub async fn count_prompt_tokens(
        &self,
        messages: &Value,
        disable_thinking: Option<bool>,
    ) -> Option<u64> {
        let endpoint = self.live_endpoint()?;
        let mut payload = json!({ "messages": messages });
        if disable_thinking != Some(false) {
            payload["chat_template_kwargs"] = json!({ "enable_thinking": false });
        }
        let timeout = self.timings().preflight_timeout;
        let client = &self.inner.deps.client;
        let templated = client
            .request_json(&endpoint, "/apply-template", Some(payload), timeout)
            .await?;
        let prompt = templated.get("prompt")?.as_str()?.to_string();
        let tokenized = client
            .request_json(
                &endpoint,
                "/tokenize",
                Some(json!({ "content": prompt })),
                timeout,
            )
            .await?;
        tokenized
            .get("tokens")?
            .as_array()
            .map(|tokens| tokens.len() as u64)
    }

    // --- inference

    /// One non-streaming chat completion. The idle timer pauses while it runs.
    pub async fn inference(
        &self,
        messages: Value,
        options: ChatOptions,
    ) -> Result<String, ServerError> {
        let Some(endpoint) = self.live_endpoint() else {
            return Err(ServerError::plain("llama-server is not running"));
        };
        self.clear_idle_timer();

        let mut body = json!({
            "messages": messages,
            "temperature": options.temperature.unwrap_or(0.7),
            "max_tokens": options.max_tokens.unwrap_or(512),
            "stream": false,
        });
        if let Some(format) = &options.response_format {
            body["response_format"] = format.clone();
        }
        // Qwen templates otherwise think into `reasoning_content` first and
        // can spend the whole budget there.
        let suppress = options.suppress_thinking();
        if suppress {
            body["chat_template_kwargs"] = json!({ "enable_thinking": false });
        }
        let max_tokens = body["max_tokens"].clone();

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let request_id = {
            let mut state = self.state();
            state.request_counter += 1;
            state.active_request = Some((state.request_counter, cancel_tx));
            state.request_counter
        };
        let started = Instant::now();
        let client = self.inner.deps.client.clone();
        let timeout = self.timings().inference_timeout;
        // Dropping the request closes the connection, which is what makes
        // llama-server stop generating.
        let outcome = tokio::select! {
            result = client.post(&endpoint, "/v1/chat/completions", body, timeout) => result,
            _ = cancel_rx => Err(PostError::Transport("cancelled".into())),
        };
        {
            let mut state = self.state();
            if state.active_request.as_ref().map(|(id, _)| *id) == Some(request_id) {
                state.active_request = None;
            }
        }
        self.reset_idle_timer();

        let (status, text) = outcome.map_err(|error| match error {
            PostError::Timeout => ServerError::plain("llama-server request timed out"),
            PostError::Transport(message) => {
                ServerError::plain(format!("llama-server request failed: {message}"))
            }
        })?;
        tracing::debug!(target: "llama", status, elapsed_ms = started.elapsed().as_millis() as u64, "llama-server inference completed");
        if status != 200 {
            return Err(response_error(status, &text));
        }
        let response: Value = serde_json::from_str(&text).map_err(|e| {
            ServerError::plain(format!("Failed to parse llama-server response: {e}"))
        })?;
        let choice = response.pointer("/choices/0");
        let message = choice.and_then(|c| c.get("message"));
        let finish = choice
            .and_then(|c| c.get("finish_reason"))
            .and_then(Value::as_str);
        let content = message
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str);
        let truncated = matches!(finish, Some("length" | "max_tokens"));
        // A cut-off reply with no content spent its budget reasoning.
        if truncated
            && (options.require_complete_output || content.is_none_or(|c| c.trim().is_empty()))
        {
            return Err(ServerError::coded(
                "Model output was truncated",
                "OUTPUT_TRUNCATED",
            ));
        }
        if truncated {
            tracing::warn!(target: "llama", max_tokens = %max_tokens, "llama-server reply was cut off at max_tokens");
        }
        if options.response_format.is_some() {
            if finish != Some("stop") {
                return Err(ServerError::coded(
                    "Model completion could not be verified",
                    "OUTPUT_COMPLETION_UNVERIFIED",
                ));
            }
            return Ok(content.unwrap_or("").to_string());
        }
        // Some builds route a suppressed-thinking answer into
        // `reasoning_content`; with thinking on, that field is the reasoning
        // and must never stand in for the answer.
        let reasoning = message
            .and_then(|m| m.get("reasoning_content"))
            .and_then(Value::as_str);
        let text = match content {
            Some(c) if !c.is_empty() => c,
            _ if suppress => reasoning.unwrap_or(""),
            _ => "",
        };
        Ok(text.trim().to_string())
    }

    /// The OS kills the process (tests).
    #[cfg(test)]
    pub(crate) fn simulate_crash(&self) {
        if let Some(process) = self.state().process.clone() {
            process.signal(libc::SIGKILL);
        }
    }

    /// Aborts the running inference, if any.
    pub fn cancel_inference(&self) {
        if let Some((_, cancel)) = self.state().active_request.take() {
            let _ = cancel.send(());
        }
    }
}

fn died_during_startup(exit: crate::process::ExitInfo, stderr: &str) -> ServerError {
    let detail = match (exit.signal, exit.code) {
        (Some(signal), _) => format!(" (signal: {})", signal_name(signal)),
        (None, Some(code)) => format!(" (exit code: {code})"),
        _ => String::new(),
    };
    let oom = if exit.signal == Some(libc::SIGKILL) {
        " — the process was killed by the OS, likely due to insufficient memory. Try a smaller/more quantized model, or reduce the context size."
    } else {
        ""
    };
    let trimmed = stderr.trim();
    let tail: String = {
        let chars: Vec<char> = trimmed.chars().collect();
        chars[chars.len().saturating_sub(800)..].iter().collect()
    };
    let output = if tail.is_empty() {
        String::new()
    } else {
        format!("\nProcess output: {tail}")
    };
    ServerError::plain(format!(
        "llama-server process died during startup{detail}{oom}{output}"
    ))
}

/// Builds a real server: spawns processes, talks HTTP to loopback, scans
/// 8221–8240 for a port.
pub fn default_deps(
    binary_dirs: Vec<PathBuf>,
    pid_file: Option<PidFile>,
    runtime: tokio::runtime::Handle,
    spawn_gate: Option<watch::Receiver<bool>>,
) -> Result<ServerDeps, reqwest::Error> {
    Ok(ServerDeps {
        spawner: Arc::new(crate::process::TokioSpawner),
        client: Arc::new(crate::client::ReqwestServerClient::new()?),
        pick_port: Arc::new(|| {
            crate::process::find_available_port(PORT_RANGE_START, PORT_RANGE_END)
        }),
        binary_dirs,
        pid_file,
        timings: ServerTimings::default(),
        runtime,
        spawn_gate,
    })
}

/// The baseline every server starts at (re-exported for callers).
pub const SERVER_CONTEXT_SIZE: u64 = BASELINE_CONTEXT_SIZE;

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
