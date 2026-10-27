//! The local model manager: the model list
//! with download state, downloads with an optional MTP drafter, delete,
//! cancel, and inference against the shared llama-server with the context
//! sized per request under the per-machine ceiling.

use crate::client::Endpoint;
use crate::context::{
    BASELINE_CONTEXT_SIZE, CeilingInputs, ContextCeiling, estimate_tokens, kv_bytes_per_token,
    resolve_context_ceiling, resolve_context_size,
};
use crate::download::{
    CancelToken, DEFAULT_MAX_RETRIES, DownloadError, DownloadOptions, DownloadTimings,
    STALE_TMP_AGE, check_disk_space, cleanup_stale_downloads, download_file,
};
use crate::gguf::read_gguf_metadata_from_file;
use crate::registry::{LocalModel, LocalProvider, find_model, local_providers};
use crate::server::{ChatOptions, LlamaServer, ServerError, StartOptions};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// Smallest file accepted as a model.
pub const MIN_FILE_SIZE: u64 = 1_000_000;
/// The context every server starts at; requests grow it on demand.
pub const SERVER_CONTEXT_SIZE: u64 = BASELINE_CONTEXT_SIZE;
/// Slack on top of the output reservation.
pub const CONTEXT_RESERVE_TOKENS: u64 = 512;
/// Measure the prompt only when the estimate lands within this factor.
pub const PREFLIGHT_MEASURE_FACTOR: u64 = 2;
/// The smallest answer still worth generating when the prompt crowds the window.
pub const MIN_OUTPUT_TOKENS: u64 = 512;

fn reserve_for(max_tokens: u64) -> u64 {
    max_tokens + CONTEXT_RESERVE_TOKENS
}

/// A failure the renderer can translate: `{ error, code, details }`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelError {
    pub message: String,
    pub code: String,
    pub details: Value,
}

impl ModelError {
    pub fn new(message: impl Into<String>, code: &str, details: Value) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            details,
        }
    }

    pub fn not_found(model_id: &str) -> Self {
        Self::new(
            format!("Model {model_id} not found"),
            "MODEL_NOT_FOUND",
            json!({ "modelId": model_id }),
        )
    }

    /// `{ success: false, error, code, details? }`.
    pub fn to_ipc(&self) -> Value {
        let mut out = json!({ "success": false, "error": self.message, "code": self.code });
        if !self.details.is_null() {
            out["details"] = self.details.clone();
        }
        out
    }
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

fn server_not_found() -> ModelError {
    ModelError::new(
        "llama-server binary not found. Please ensure the app is installed correctly.",
        "LLAMASERVER_NOT_FOUND",
        Value::Null,
    )
}

/// What one inference asks for (the bridge's config, already defaulted).
#[derive(Debug, Clone, Default)]
pub struct InferenceOptions {
    pub system_prompt: String,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub disable_thinking: Option<bool>,
    pub require_complete_output: bool,
    pub refuse_clipped_by_window: bool,
    pub response_format: Option<Value>,
    /// A minimum context window for this request.
    pub context_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Progress {
    progress: f64,
    downloaded: u64,
    total: u64,
}

#[derive(Default)]
struct State {
    progress: HashMap<String, Progress>,
    active: HashMap<String, CancelToken>,
    reservations: HashMap<String, f64>,
    lifecycle: u64,
    current_server_model: Option<String>,
    ceiling_cache: Option<(PathBuf, ContextCeiling)>,
}

/// Machine facts behind seams so the ceiling is testable for any machine.
pub struct Machine {
    pub total_memory_bytes: Box<dyn Fn() -> f64 + Send + Sync>,
    /// Apple Silicon shares memory with the GPU; anything else may have VRAM
    /// of its own that is not probed.
    pub unified_memory: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            total_memory_bytes: Box::new(total_memory_bytes),
            unified_memory: cfg!(all(target_os = "macos", target_arch = "aarch64")),
        }
    }
}

/// `hw.memsize`.
pub fn total_memory_bytes() -> f64 {
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    // SAFETY: `hw.memsize` is a u64 sysctl; buffer and size match.
    let result = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result == 0 { value as f64 } else { 0.0 }
}

pub struct ModelManager {
    providers: &'static [LocalProvider],
    models_dir: PathBuf,
    server: LlamaServer,
    http: reqwest::Client,
    machine: Machine,
    download_timings: DownloadTimings,
    /// Replaces each provider's base URL (tests).
    base_url_override: Option<String>,
    state: Mutex<State>,
}

fn is_valid_model(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() > MIN_FILE_SIZE)
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

impl ModelManager {
    /// `models_dir` is created now and cleared of stale temp files.
    pub fn new(models_dir: PathBuf, server: LlamaServer, http: reqwest::Client) -> Self {
        let _ = std::fs::create_dir_all(&models_dir);
        cleanup_stale_downloads(&models_dir, STALE_TMP_AGE);
        Self {
            providers: local_providers(),
            models_dir,
            server,
            http,
            machine: Machine::default(),
            download_timings: DownloadTimings::default(),
            base_url_override: None,
            state: Mutex::new(State::default()),
        }
    }

    pub fn with_machine(mut self, machine: Machine) -> Self {
        self.machine = machine;
        self
    }

    pub fn with_download_source(mut self, base_url: String, timings: DownloadTimings) -> Self {
        self.base_url_override = Some(base_url);
        self.download_timings = timings;
        self
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn server(&self) -> &LlamaServer {
        &self.server
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub fn find(&self, model_id: &str) -> Option<(&'static LocalModel, &'static LocalProvider)> {
        find_model(self.providers, model_id)
    }

    pub fn model_path(&self, model: &LocalModel) -> PathBuf {
        self.models_dir.join(&model.file_name)
    }

    pub fn current_server_model(&self) -> Option<String> {
        self.state().current_server_model.clone()
    }

    fn base_url<'a>(&'a self, provider: &'a LocalProvider) -> &'a str {
        self.base_url_override
            .as_deref()
            .unwrap_or(&provider.base_url)
    }

    pub fn download_url(&self, provider: &LocalProvider, model: &LocalModel) -> String {
        format!(
            "{}/{}/resolve/main/{}",
            self.base_url(provider),
            model.hf_repo,
            model.file_name
        )
    }

    fn draft_download_url(&self, provider: &LocalProvider, model: &LocalModel) -> Option<String> {
        Some(format!(
            "{}/{}/resolve/main/{}",
            self.base_url(provider),
            model.draft_hf_repo.as_deref()?,
            model.draft_file_name.as_deref()?
        ))
    }

    // --- listing

    /// Every registry model with its download state (`LocalLLMModelStatus`).
    pub fn all_models(&self) -> Vec<Value> {
        let entries: Vec<(&LocalModel, &LocalProvider, PathBuf)> = self
            .providers
            .iter()
            .flat_map(|provider| {
                provider
                    .models
                    .iter()
                    .map(move |model| (model, provider, self.models_dir.join(&model.file_name)))
            })
            .collect();

        // A download can finish between the file check and the in-memory
        // state read; retry so no model reads "neither downloaded nor
        // downloading".
        let mut downloaded = Vec::new();
        for _ in 0..3 {
            let lifecycle = self.state().lifecycle;
            downloaded = entries
                .iter()
                .map(|(_, _, path)| is_valid_model(path))
                .collect();
            if lifecycle == self.state().lifecycle {
                break;
            }
        }

        let state = self.state();
        entries
            .iter()
            .zip(downloaded)
            .map(|((model, provider, path), is_downloaded)| {
                let progress = state.progress.get(&model.id).copied().unwrap_or_default();
                let mut out: Map<String, Value> = model.raw.clone();
                out.insert("providerId".into(), json!(provider.id));
                out.insert("providerName".into(), json!(provider.name));
                out.insert("isDownloaded".into(), json!(is_downloaded));
                out.insert(
                    "isDownloading".into(),
                    json!(state.active.contains_key(&model.id)),
                );
                out.insert("downloadProgress".into(), json!(progress.progress));
                out.insert("downloadedSize".into(), json!(progress.downloaded));
                out.insert("totalSize".into(), json!(progress.total));
                out.insert(
                    "path".into(),
                    if is_downloaded {
                        json!(path.display().to_string())
                    } else {
                        Value::Null
                    },
                );
                Value::Object(out)
            })
            .collect()
    }

    pub fn is_downloaded(&self, model_id: &str) -> bool {
        self.find(model_id)
            .is_some_and(|(model, _)| is_valid_model(&self.model_path(model)))
    }

    pub fn any_downloaded(&self) -> bool {
        self.providers
            .iter()
            .flat_map(|p| &p.models)
            .any(|m| is_valid_model(&self.model_path(m)))
    }

    /// The drafter path, only when declared and present at a valid size.
    pub fn draft_path(&self, model: &LocalModel) -> Option<PathBuf> {
        if !model.has_drafter() {
            return None;
        }
        let path = self.models_dir.join(model.draft_file_name.as_deref()?);
        is_valid_model(&path).then_some(path)
    }

    // --- downloads

    fn reserved_bytes(state: &State) -> f64 {
        state
            .reservations
            .iter()
            .map(|(id, bytes)| {
                let done = state.progress.get(id).map_or(0, |p| p.downloaded) as f64;
                (bytes - done).max(0.0)
            })
            .sum()
    }

    /// Downloads a model (and its drafter, best effort). `on_progress` gets
    /// `(percent, downloaded, total)`.
    pub async fn download_model(
        &self,
        model_id: &str,
        on_progress: &(dyn Fn(f64, u64, u64) + Send + Sync),
    ) -> Result<PathBuf, ModelError> {
        let (model, provider) = self
            .find(model_id)
            .ok_or_else(|| ModelError::not_found(model_id))?;
        let model_path = self.model_path(model);
        if is_valid_model(&model_path) {
            return Ok(model_path);
        }

        let cancel = CancelToken::new();
        {
            let mut state = self.state();
            if state.active.contains_key(model_id) {
                return Err(ModelError::new(
                    "Model is already being downloaded",
                    "DOWNLOAD_IN_PROGRESS",
                    json!({ "modelId": model_id }),
                ));
            }
            state.active.insert(model_id.to_string(), cancel.clone());
            state.lifecycle += 1;
        }

        let result = self
            .run_download(model, provider, &model_path, &cancel, on_progress)
            .await;

        {
            let mut state = self.state();
            if state.active.remove(model_id).is_some() {
                state.lifecycle += 1;
            }
            state.progress.remove(model_id);
            state.reservations.remove(model_id);
        }

        result.map_err(|error| match error {
            Fail::Model(error) => error,
            Fail::Download(DownloadError::Cancelled) => ModelError::new(
                "Download cancelled by user",
                "DOWNLOAD_CANCELLED",
                json!({ "modelId": model_id }),
            ),
            Fail::Download(DownloadError::Http(status)) => ModelError::new(
                format!("Download failed with status {status}"),
                "DOWNLOAD_FAILED",
                json!({ "statusCode": status }),
            ),
            Fail::Download(other) => ModelError::new(
                format!("Network error: {other}"),
                "NETWORK_ERROR",
                json!({ "error": other.to_string() }),
            ),
        })
    }

    async fn run_download(
        &self,
        model: &LocalModel,
        provider: &LocalProvider,
        model_path: &Path,
        cancel: &CancelToken,
        on_progress: &(dyn Fn(f64, u64, u64) + Send + Sync),
    ) -> Result<PathBuf, Fail> {
        let _ = std::fs::create_dir_all(&self.models_dir);
        let has_drafter = model.has_drafter();

        let mut required = if model.size_bytes > 0 {
            model.size_bytes
        } else {
            model.size_mb * 1_000_000
        };
        if required > 0 && has_drafter {
            required += model.draft_size_bytes;
        }
        if required > 0 {
            let reserved = {
                let mut state = self.state();
                state
                    .reservations
                    .insert(model.id.clone(), required as f64 * 1.2);
                Self::reserved_bytes(&state)
            };
            let (ok, available) = check_disk_space(&self.models_dir, reserved as u64);
            if !ok {
                return Err(Fail::Model(ModelError::new(
                    format!(
                        "Not enough disk space. Need ~{}MB, only {}MB available.",
                        (reserved / 1_000_000.0).round() as u64,
                        (available as f64 / 1_000_000.0).round() as u64
                    ),
                    "INSUFFICIENT_DISK_SPACE",
                    json!({ "required": reserved, "available": available }),
                )));
            }
        }

        // With a drafter, progress spans both files by declared size and
        // never moves backwards across the boundary.
        let combined_total = if has_drafter {
            model.size_bytes + model.draft_size_bytes
        } else {
            0
        };
        let last_combined = Mutex::new(0u64);
        let record = |progress: f64, downloaded: u64, total: u64| {
            self.state().progress.insert(
                model.id.clone(),
                Progress {
                    progress,
                    downloaded,
                    total,
                },
            );
            on_progress(progress, downloaded, total);
        };
        let emit_combined = |raw: u64| {
            let mut last = last_combined.lock().unwrap_or_else(|e| e.into_inner());
            let mut combined = raw.min(combined_total);
            if combined < *last {
                combined = *last;
            } else {
                *last = combined;
            }
            drop(last);
            let progress = if combined_total > 0 {
                combined as f64 / combined_total as f64 * 100.0
            } else {
                0.0
            };
            record(progress, combined, combined_total);
        };
        let main_progress = |downloaded: u64, total: u64| {
            if has_drafter {
                emit_combined(downloaded);
            } else {
                let progress = if total > 0 {
                    downloaded as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                record(progress, downloaded, total);
            }
        };

        let url = self.download_url(provider, model);
        download_file(
            &self.http,
            &url,
            model_path,
            DownloadOptions {
                cancel,
                on_progress: Some(&main_progress),
                max_retries: DEFAULT_MAX_RETRIES,
                expected_size: 0,
                timings: self.download_timings,
            },
        )
        .await
        .map_err(Fail::Download)?;

        let size = file_size(model_path);
        if size < MIN_FILE_SIZE {
            let _ = std::fs::remove_file(model_path);
            return Err(Fail::Model(ModelError::new(
                "Downloaded file appears to be corrupted or incomplete",
                "DOWNLOAD_CORRUPTED",
                json!({ "size": size, "minSize": MIN_FILE_SIZE }),
            )));
        }

        // The drafter is opportunistic: a failure leaves the model usable.
        if has_drafter
            && let (Some(draft_url), Some(draft_name)) = (
                self.draft_download_url(provider, model),
                model.draft_file_name.as_deref(),
            )
        {
            let draft_path = self.models_dir.join(draft_name);
            let draft_progress = |downloaded: u64, _total: u64| emit_combined(size + downloaded);
            let result = download_file(
                &self.http,
                &draft_url,
                &draft_path,
                DownloadOptions {
                    cancel,
                    on_progress: Some(&draft_progress),
                    max_retries: DEFAULT_MAX_RETRIES,
                    expected_size: 0,
                    timings: self.download_timings,
                },
            )
            .await;
            match result {
                Ok(_) if file_size(&draft_path) < MIN_FILE_SIZE => {
                    let _ = std::fs::remove_file(&draft_path);
                    tracing::warn!(target: "models", model = %model.id, "MTP drafter file too small, keeping model without it");
                }
                Ok(_) => {}
                Err(DownloadError::Cancelled) => {
                    let _ = std::fs::remove_file(&draft_path);
                    return Err(Fail::Download(DownloadError::Cancelled));
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&draft_path);
                    tracing::warn!(target: "models", model = %model.id, %error, "MTP drafter download failed, keeping model without it");
                }
            }
        }
        Ok(model_path.to_path_buf())
    }

    /// Cancels a running download; the download itself cleans up.
    pub fn cancel_download(&self, model_id: &str) -> bool {
        match self.state().active.get(model_id) {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Deletes a model file and its drafter (best effort).
    pub fn delete_model(&self, model_id: &str) -> Result<(), ModelError> {
        let (model, _) = self
            .find(model_id)
            .ok_or_else(|| ModelError::not_found(model_id))?;
        let path = self.model_path(model);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| {
                ModelError::new(
                    error.to_string(),
                    "DELETE_FAILED",
                    json!({ "modelId": model_id }),
                )
            })?;
        }
        if let Some(draft) = &model.draft_file_name {
            let _ = std::fs::remove_file(self.models_dir.join(draft));
        }
        Ok(())
    }

    // --- server

    /// Start options for `model`: the baseline context (bounded by the
    /// model's own), 4 threads, all layers on the GPU, the drafter if present.
    pub fn server_start_options(
        &self,
        model: &LocalModel,
        context_size: Option<u64>,
        cache_ram_mib: Option<u64>,
    ) -> StartOptions {
        let declared = if model.context_length > 0 {
            model.context_length
        } else {
            SERVER_CONTEXT_SIZE
        };
        StartOptions {
            context_size: Some(context_size.unwrap_or(SERVER_CONTEXT_SIZE.min(declared))),
            threads: Some(4),
            gpu_layers: Some(99),
            cache_ram_mib,
            draft_model_path: self.draft_path(model),
        }
    }

    /// The renderer's `llama-server-start`: starts (or reuses) the server for
    /// a model and returns its port and API key.
    pub async fn start_server_for(&self, model_id: &str) -> Result<Endpoint, String> {
        let (model, _) = self
            .find(model_id)
            .ok_or_else(|| format!("Model \"{model_id}\" not found"))?;
        let path = self.model_path(model);
        self.server
            .start(&path, self.server_start_options(model, None, None))
            .await
            .map_err(|e| e.message)?;
        self.state().current_server_model = Some(model_id.to_string());
        self.server
            .endpoint()
            .ok_or_else(|| "llama-server is not running".to_string())
    }

    /// Starts the server for `model_id` if it is downloaded; `false` otherwise.
    pub async fn prewarm(&self, model_id: &str) -> bool {
        let Some((model, _)) = self.find(model_id) else {
            return false;
        };
        let path = self.model_path(model);
        if !is_valid_model(&path) || !self.server.is_available() {
            return false;
        }
        match self
            .server
            .start(&path, self.server_start_options(model, None, None))
            .await
        {
            Ok(()) => {
                self.state().current_server_model = Some(model_id.to_string());
                tracing::info!(target: "llama", model_id, "llama-server pre-warmed");
                true
            }
            Err(error) => {
                tracing::warn!(target: "llama", %error, "Failed to pre-warm llama-server");
                false
            }
        }
    }

    pub async fn stop_server(&self) {
        self.server.stop().await;
        self.state().current_server_model = None;
    }

    pub fn cancel_inference(&self) {
        self.server.cancel_inference();
    }

    /// The largest context this machine may give this model, memoized per
    /// model file (reading it costs a few MiB of GGUF header).
    pub async fn context_ceiling(&self, model: &LocalModel, model_path: &Path) -> ContextCeiling {
        if let Some((path, value)) = &self.state().ceiling_cache
            && path == model_path
        {
            return *value;
        }
        let path = model_path.to_path_buf();
        let metadata = tokio::task::spawn_blocking(move || read_gguf_metadata_from_file(&path))
            .await
            .ok()
            .flatten();
        let kv = kv_bytes_per_token(metadata.as_ref());
        let trained = metadata
            .as_ref()
            .and_then(|m| m.context_length)
            .filter(|n| *n > 0.0)
            .map(|n| n as u64)
            .unwrap_or(model.context_length);
        let value = resolve_context_ceiling(CeilingInputs {
            total_memory_bytes: (self.machine.total_memory_bytes)(),
            weights_bytes: file_size(model_path),
            draft_weights_bytes: self.draft_path(model).map_or(0, |p| file_size(&p)),
            kv_bytes_per_token: kv,
            has_recurrent_state: metadata.as_ref().is_some_and(|m| m.has_recurrent_state),
            trained_context_tokens: trained,
            discrete_gpu_unverified: !self.machine.unified_memory,
        });
        tracing::info!(
            target: "llama",
            model = %model.id,
            architecture = metadata.as_ref().map(|m| m.architecture.as_str()),
            kv_bytes_per_token = kv,
            ceiling = value.ceiling,
            reason = value.reason.as_str(),
            cache_ram_mib = value.cache_ram_mib,
            "Resolved llama-server context ceiling"
        );
        self.state().ceiling_cache = Some((model_path.to_path_buf(), value));
        value
    }

    // --- inference

    pub async fn run_inference(
        &self,
        model_id: &str,
        prompt: &str,
        options: &InferenceOptions,
    ) -> Result<String, ModelError> {
        let started = Instant::now();
        tracing::debug!(target: "llama", model_id, prompt_len = prompt.len(), "INFERENCE_START");
        if !self.server.is_available() {
            return Err(server_not_found());
        }
        let (model, _) = self
            .find(model_id)
            .ok_or_else(|| ModelError::not_found(model_id))?;
        let model_path = self.model_path(model);
        if !is_valid_model(&model_path) {
            return Err(ModelError::new(
                format!("Model {model_id} is not downloaded or is corrupted"),
                "MODEL_NOT_DOWNLOADED",
                json!({ "modelId": model_id }),
            ));
        }

        let messages = json!([
            { "role": "system", "content": options.system_prompt },
            { "role": "user", "content": prompt },
        ]);
        let max_tokens = options.max_tokens.unwrap_or(512);
        let reserve = reserve_for(max_tokens);
        let estimated = estimate_tokens(&options.system_prompt) + estimate_tokens(prompt);
        // A caller may raise the floor, never past the ceiling.
        let floor = SERVER_CONTEXT_SIZE.max(options.context_size.unwrap_or(0));

        let mut run = Run {
            manager: self,
            model,
            model_id,
            model_path: &model_path,
            ceiling: None,
            smallest_refused: u64::MAX,
        };

        let mut target = floor;
        // The baseline floor needs no ceiling, which keeps the GGUF read off
        // the short-request path.
        if floor > SERVER_CONTEXT_SIZE || estimated + reserve > floor {
            let ceiling = run.ceiling().await.ceiling;
            target = resolve_context_size(estimated + reserve, floor, ceiling);
        }

        let needs_start = !self.server.is_ready()
            || self.current_server_model().as_deref() != Some(model_id)
            || target > self.server.context_size().unwrap_or(SERVER_CONTEXT_SIZE);
        if needs_start {
            tracing::debug!(target: "llama", model_id, context = target, "INFERENCE_STARTING_SERVER");
            run.start_server(target).await?;
        }

        let granted = run
            .ensure_prompt_fits(&messages, options, estimated, max_tokens, floor)
            .await?;
        // The preflight shrank the reply to make room for the prompt; a reply
        // that fills it was cut short by the window.
        let refuse_window_clip = options.refuse_clipped_by_window && granted < max_tokens;

        let result = self
            .server
            .inference(
                messages,
                ChatOptions {
                    temperature: Some(options.temperature.unwrap_or(0.7)),
                    max_tokens: Some(granted),
                    disable_thinking: options.disable_thinking,
                    require_complete_output: options.require_complete_output || refuse_window_clip,
                    response_format: options.response_format.clone(),
                },
            )
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(text) => {
                tracing::debug!(target: "llama", elapsed_ms, result_len = text.len(), "INFERENCE_SUCCESS");
                Ok(text)
            }
            Err(error) => {
                tracing::debug!(target: "llama", elapsed_ms, error = %error.message, "INFERENCE_FAILED");
                Err(inference_error(error, model, refuse_window_clip))
            }
        }
    }
}

enum Fail {
    Model(ModelError),
    Download(DownloadError),
}

fn inference_error(error: ServerError, model: &LocalModel, refuse_window_clip: bool) -> ModelError {
    if refuse_window_clip && error.code == Some("OUTPUT_TRUNCATED") {
        return ModelError::new(
            format!(
                "{} ran out of context before the reply was complete.",
                model.name
            ),
            "CONTEXT_TOO_LARGE",
            json!({ "modelId": model.id, "modelName": model.name, "neededTokens": null, "maxContextTokens": null }),
        );
    }
    // Typed failures keep their identity so the renderer can translate them.
    match error.code {
        Some(
            code @ ("CONTEXT_TOO_LARGE" | "OUTPUT_TRUNCATED" | "OUTPUT_COMPLETION_UNVERIFIED"),
        ) => ModelError::new(
            error.message,
            code,
            json!({
                "modelId": model.id,
                "modelName": model.name,
                "neededTokens": error.needed_tokens,
                "maxContextTokens": error.max_context_tokens,
            }),
        ),
        _ => ModelError::new(
            format!("Inference failed: {}", error.message),
            "INFERENCE_FAILED",
            json!({ "error": error.message }),
        ),
    }
}

/// One inference's sizing state: the lazily read ceiling and the smallest
/// window this machine has already refused for the model.
struct Run<'a> {
    manager: &'a ModelManager,
    model: &'a LocalModel,
    model_id: &'a str,
    model_path: &'a Path,
    ceiling: Option<ContextCeiling>,
    smallest_refused: u64,
}

impl Run<'_> {
    async fn ceiling(&mut self) -> ContextCeiling {
        if let Some(ceiling) = self.ceiling {
            return ceiling;
        }
        let ceiling = self
            .manager
            .context_ceiling(self.model, self.model_path)
            .await;
        self.ceiling = Some(ceiling);
        ceiling
    }

    fn unavailable(&self, context: u64, cause: &ServerError) -> ModelError {
        // llama.cpp's startup dump is for the log only: details cross IPC.
        tracing::error!(target: "llama", model = self.model_id, context, error = %cause.message, "llama-server could not be started");
        ModelError::new(
            format!("{} could not be started on this computer.", self.model.name),
            "LOCAL_SERVER_UNAVAILABLE",
            json!({ "modelId": self.model_id, "modelName": self.model.name }),
        )
    }

    async fn start_at(&self, size: u64) -> Result<(), ServerError> {
        let cache_ram = self.ceiling.map(|c| c.cache_ram_mib);
        let options = self
            .manager
            .server_start_options(self.model, Some(size), cache_ram);
        self.manager.server.start(self.model_path, options).await
    }

    async fn start_server(&mut self, context: u64) -> Result<(), ModelError> {
        let server = &self.manager.server;
        if server.is_ready() && context >= self.smallest_refused {
            return Ok(());
        }
        // Only a live server already serving this model has a window worth
        // coming back to.
        let restorable = if server.is_ready()
            && server.has_process()
            && self.manager.current_server_model().as_deref() == Some(self.model_id)
        {
            server.context_size()
        } else {
            None
        };
        match self.start_at(context).await {
            Ok(()) => {
                if server.context_size().unwrap_or(context) < context {
                    self.smallest_refused = self.smallest_refused.min(context);
                }
            }
            Err(error) => {
                self.smallest_refused = self.smallest_refused.min(context);
                // The failed start already stopped the working server; come
                // back at the window that worked.
                let Some(previous) = restorable.filter(|p| *p < context) else {
                    return Err(self.unavailable(context, &error));
                };
                tracing::warn!(target: "llama", model = self.model_id, attempted = context, restoring = previous, error = %error.message, "llama-server failed to grow; restoring the previous context");
                if let Err(error) = self.start_at(previous).await {
                    return Err(self.unavailable(previous, &error));
                }
            }
        }
        self.manager.state().current_server_model = Some(self.model_id.to_string());
        Ok(())
    }

    /// Makes sure the prompt fits the running window: grows it once if not,
    /// trades output room for prompt room if it still does not, and refuses
    /// only when no usable answer is left. Returns the output allowance.
    async fn ensure_prompt_fits(
        &mut self,
        messages: &Value,
        options: &InferenceOptions,
        estimated: u64,
        max_tokens: u64,
        floor: u64,
    ) -> Result<u64, ModelError> {
        let server = self.manager.server.clone();
        let reserve = reserve_for(max_tokens);
        let running = server.context_size().unwrap_or(floor);
        if (estimated + reserve) * PREFLIGHT_MEASURE_FACTOR <= running {
            return Ok(max_tokens);
        }

        let usable = server.usable_context_size().await.unwrap_or(running);
        let Some(exact) = server
            .count_prompt_tokens(messages, options.disable_thinking)
            .await
        else {
            tracing::warn!(target: "llama", model = self.model_id, estimated, "Could not measure prompt tokens; proceeding on the estimate");
            return Ok(max_tokens);
        };

        let needed = exact + reserve;
        if needed <= usable {
            return Ok(max_tokens);
        }

        let ceiling = self.ceiling().await.ceiling;
        let grown = resolve_context_size(needed, floor, ceiling);
        if grown > usable {
            self.start_server(grown).await?;
        }

        let now_usable = match server.usable_context_size().await {
            Some(n) => n,
            None => server.context_size().unwrap_or(grown),
        };
        if needed <= now_usable {
            return Ok(max_tokens);
        }

        // llama-server rejects only a prompt that fills the window by itself;
        // a shorter answer is still a real answer.
        let accepts_shorter = !options.require_complete_output || options.refuse_clipped_by_window;
        let affordable = now_usable.saturating_sub(exact);
        if accepts_shorter && affordable >= MIN_OUTPUT_TOKENS {
            tracing::info!(target: "llama", model = self.model_id, requested = max_tokens, granted = affordable, prompt_tokens = exact, context = now_usable, "Trimming the output allowance to fit the context window");
            return Ok(affordable);
        }

        Err(ModelError::new(
            format!(
                "This content needs about {needed} tokens of context, but {} can only use {now_usable} on this computer.",
                self.model.name
            ),
            "CONTEXT_TOO_LARGE",
            json!({
                "modelId": self.model_id,
                "modelName": self.model.name,
                "neededTokens": needed,
                "maxContextTokens": now_usable,
            }),
        ))
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
