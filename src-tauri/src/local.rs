//! Local inference in the app: the model manager, the one-at-a-time bridge,
//! download status for the renderer, and the llama-server sidecar's
//! bookkeeping (stale pids reaped at startup before anything spawns, stopped
//! on quit within the 8 s deadline).

use retone_llama::bridge::LocalReasoningBridge;
use retone_llama::cache_dir;
use retone_llama::download_status::LocalModelDownloadStatus;
use retone_llama::models::ModelManager;
use retone_llama::server::{LlamaServer, default_deps};
use retone_llama::sidecar::{
    PidFile, ReapGraces, SHUTDOWN_DEADLINE, SidecarRegistry, reap_stale_sidecars,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::watch;

pub struct LocalInference {
    pub models: Arc<ModelManager>,
    pub bridge: LocalReasoningBridge,
    downloads: Mutex<LocalModelDownloadStatus>,
    sidecars: SidecarRegistry,
    pid_file: PidFile,
    reaped: watch::Sender<bool>,
    runtime: tokio::runtime::Handle,
}

/// Where the bundled binary lives: `Contents/Resources/bin` in the app
/// bundle, and the repo's `resources/bin` in debug builds.
pub fn binary_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(macos) = exe.parent()
    {
        dirs.push(macos.join("../Resources/bin"));
    }
    if cfg!(debug_assertions) {
        dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/bin"));
    }
    dirs
}

impl LocalInference {
    /// Builds the service. `http` follows redirects (Hugging Face resolves to
    /// a CDN).
    pub fn new(
        user_data: &Path,
        cache_root_override: Option<&str>,
        runtime: tokio::runtime::Handle,
        http: reqwest::Client,
    ) -> Result<Self, reqwest::Error> {
        let pid_file = PidFile::new(user_data);
        let (reaped, gate) = watch::channel(false);
        let server = LlamaServer::new(default_deps(
            binary_dirs(),
            Some(pid_file.clone()),
            runtime.clone(),
            Some(gate),
        )?);
        let models_dir = cache_dir::models_dir(cache_root_override);
        tracing::info!(
            target: "llama",
            models_dir = %models_dir.display(),
            available = server.is_available(),
            binary = ?server.binary_path(),
            "Local inference ready"
        );
        let models = Arc::new(ModelManager::new(models_dir, server.clone(), http));

        let sidecars = SidecarRegistry::new();
        sidecars.register(retone_llama::SIDECAR_NAME, move || {
            let server = server.clone();
            async move { server.stop().await }
        });

        Ok(Self {
            bridge: LocalReasoningBridge::new(models.clone()),
            models,
            downloads: Mutex::new(LocalModelDownloadStatus::new()),
            sidecars,
            pid_file,
            reaped,
            runtime,
        })
    }

    pub fn server(&self) -> &LlamaServer {
        self.models.server()
    }

    pub fn downloads(&self) -> MutexGuard<'_, LocalModelDownloadStatus> {
        self.downloads.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Kills a llama-server a crashed run left behind, then lets starts
    /// through. Runs in the background; a start waits for it.
    pub fn reap_stale_sidecars(self: &Arc<Self>) {
        let this = self.clone();
        self.runtime.spawn(async move {
            reap_stale_sidecars(&this.pid_file, ReapGraces::default()).await;
            tracing::debug!(target: "sidecar", "Stale sidecars reaped");
            this.reaped.send_replace(true);
        });
    }

    /// Stops llama-server on quit, waiting at most 8 s. Blocks the caller.
    pub fn shutdown(&self) {
        let finished = self
            .runtime
            .block_on(self.sidecars.shutdown_all(SHUTDOWN_DEADLINE));
        if !finished {
            tracing::warn!(target: "sidecar", "Sidecar shutdown hit the deadline");
        }
    }
}
