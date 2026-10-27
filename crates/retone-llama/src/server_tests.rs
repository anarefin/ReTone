//! llama-server tests: context sizing and reuse, the idle stop, the Gemma MTP
//! drafter, startup, health and cancellation.

use super::*;
use crate::client::ReqwestServerClient;
use crate::test_support::{FakeClient, FakeOutcome, FakeSpawner, MockServer, Reply, settle};
use std::sync::atomic::Ordering;

const MINUTE: Duration = Duration::from_secs(60);

struct Harness {
    _dir: tempfile::TempDir,
    root: PathBuf,
    spawner: Arc<FakeSpawner>,
    server: LlamaServer,
}

impl Harness {
    fn model(&self, name: &str) -> PathBuf {
        self.root.join("models").join(name)
    }

    fn main(&self) -> PathBuf {
        self.model("main.gguf")
    }
}

fn harness_with(
    client: Arc<dyn ServerClient>,
    port: u16,
    configure: impl FnOnce(&mut ServerDeps),
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/llama-server-darwin-arm64"), "").unwrap();
    std::fs::create_dir_all(root.join("models")).unwrap();
    for name in ["main.gguf", "other.gguf", "big.gguf", "draft.gguf"] {
        std::fs::write(root.join("models").join(name), "gguf").unwrap();
    }
    let spawner = FakeSpawner::new();
    let mut deps = ServerDeps {
        spawner: Arc::new(spawner.clone()),
        client,
        pick_port: Arc::new(move || Ok(port)),
        binary_dirs: vec![root.join("bin")],
        pid_file: None,
        timings: ServerTimings::default(),
        runtime: tokio::runtime::Handle::current(),
        spawn_gate: None,
    };
    configure(&mut deps);
    Harness {
        _dir: dir,
        root,
        spawner,
        server: LlamaServer::new(deps),
    }
}

fn harness() -> (Harness, Arc<FakeClient>) {
    let client = Arc::new(FakeClient::default());
    (harness_with(client.clone(), 8221, |_| {}), client)
}

fn ctx(size: u64) -> StartOptions {
    StartOptions {
        context_size: Some(size),
        ..StartOptions::default()
    }
}

// --- grow-only restart

#[tokio::test]
async fn a_larger_context_restarts_the_server() {
    let (h, _) = harness();
    h.server.start(&h.main(), ctx(16384)).await.unwrap();
    assert_eq!(h.spawner.spawn_count(), 1);
    h.server.start(&h.main(), ctx(32768)).await.unwrap();
    assert_eq!(h.spawner.spawn_count(), 2);
    assert_eq!(h.server.context_size(), Some(32768));
    assert_eq!(h.spawner.sigterms(), 1, "the old server was stopped first");
}

#[tokio::test]
async fn an_equal_or_smaller_context_reuses_the_running_server() {
    let (h, _) = harness();
    h.server.start(&h.main(), ctx(32768)).await.unwrap();
    h.server.start(&h.main(), ctx(32768)).await.unwrap();
    h.server.start(&h.main(), ctx(16384)).await.unwrap();
    assert_eq!(h.spawner.spawn_count(), 1);
    assert_eq!(h.server.context_size(), Some(32768));
}

#[tokio::test]
async fn growing_the_context_restarts_with_an_unchanged_drafter() {
    let (h, _) = harness();
    let draft = Some(h.model("draft.gguf"));
    let with = |size| StartOptions {
        context_size: Some(size),
        draft_model_path: draft.clone(),
        ..StartOptions::default()
    };
    h.server.start(&h.main(), with(16384)).await.unwrap();
    h.server.start(&h.main(), with(65536)).await.unwrap();
    assert_eq!(h.spawner.spawn_count(), 2);
    assert_eq!(h.server.context_size(), Some(65536));
}

#[tokio::test]
async fn a_context_grown_for_one_model_does_not_carry_over() {
    let (h, _) = harness();
    h.server
        .start(&h.model("big.gguf"), ctx(65536))
        .await
        .unwrap();
    h.server
        .start(&h.model("other.gguf"), ctx(16384))
        .await
        .unwrap();
    assert_eq!(h.spawner.spawn_count(), 2);
    assert_eq!(h.server.context_size(), Some(16384));
}

#[tokio::test]
async fn stopping_forgets_the_grown_context() {
    let (h, _) = harness();
    h.server.start(&h.main(), ctx(65536)).await.unwrap();
    assert_eq!(h.server.context_size(), Some(65536));
    h.server.stop().await;
    assert_eq!(h.server.context_size(), None);
    assert!(!h.server.status().running);
}

// --- spawn arguments and environment

#[tokio::test]
async fn spawns_with_the_metal_arguments_and_library_path() {
    let (h, _) = harness();
    h.server
        .start(
            &h.main(),
            StartOptions {
                context_size: Some(32768),
                cache_ram_mib: Some(1024),
                threads: Some(4),
                gpu_layers: Some(99),
                draft_model_path: Some(h.model("draft.gguf")),
            },
        )
        .await
        .unwrap();
    let spec = h.spawner.specs.lock().unwrap()[0].clone();
    assert_eq!(spec.program, h.root.join("bin/llama-server-darwin-arm64"));
    assert_eq!(spec.arg_value("--model"), Some(h.main().to_str().unwrap()));
    assert_eq!(spec.arg_value("--host"), Some("127.0.0.1"));
    assert_eq!(spec.arg_value("--port"), Some("8221"));
    assert_eq!(spec.arg_value("--threads"), Some("4"));
    assert_eq!(spec.arg_value("--ctx-size"), Some("32768"));
    assert_eq!(spec.arg_value("--cache-ram"), Some("1024"));
    assert_eq!(spec.arg_value("--n-gpu-layers"), Some("99"));
    assert!(spec.has_arg("--jinja"));
    assert_eq!(
        spec.arg_value("--model-draft"),
        Some(h.model("draft.gguf").to_str().unwrap())
    );
    assert_eq!(spec.arg_value("--spec-type"), Some("draft-mtp"));
    assert_eq!(spec.arg_value("--spec-draft-n-max"), Some("3"));
    let env: std::collections::HashMap<_, _> = spec.env.iter().cloned().collect();
    assert!(env["DYLD_LIBRARY_PATH"].starts_with(h.root.join("bin").to_str().unwrap()));
    assert!(env.contains_key("LLAMA_ARG_FIT"));

    let status = h.server.status();
    assert!(status.running && status.available && status.gpu_accelerated);
    assert_eq!(status.port, Some(8221));
    assert_eq!(status.backend.as_deref(), Some("metal"));
    assert_eq!(status.model_name.as_deref(), Some("main"));
}

fn spawned_key(spec: &crate::process::SpawnSpec) -> String {
    spec.env
        .iter()
        .find(|(k, _)| k == API_KEY_ENV)
        .map(|(_, v)| v.clone())
        .expect("LLAMA_API_KEY in the spawn environment")
}

#[tokio::test]
async fn each_launch_gets_its_own_key_in_the_environment_never_in_argv() {
    let (h, client) = harness();
    h.server.start(&h.main(), ctx(16384)).await.unwrap();
    let first = h.spawner.specs.lock().unwrap()[0].clone();
    let key = spawned_key(&first);
    assert_eq!(key.len(), 64);
    assert!(
        !first
            .args
            .iter()
            .any(|a| a.contains(&key) || a.starts_with("--api-key")),
        "the key must not be visible in argv: {:?}",
        first.args
    );
    for flag in ["--no-slots", "--metrics", "--no-webui"] {
        assert!(first.has_arg(flag), "{flag} missing: {:?}", first.args);
    }
    assert!(!first.has_arg("--slots"));
    assert_eq!(first.arg_value("--alias"), Some("local"));

    let status = h.server.status();
    assert_eq!(status.api_key.as_deref(), Some(key.as_str()));
    assert_eq!(h.server.endpoint(), Some(Endpoint::new(8221, key.as_str())));
    assert!(
        client
            .endpoints
            .lock()
            .unwrap()
            .iter()
            .all(|e| *e.api_key == key),
        "startup health polls use the launch's key"
    );

    // A restart is a new launch with a new key.
    h.server.start(&h.main(), ctx(32768)).await.unwrap();
    let second = spawned_key(&h.spawner.specs.lock().unwrap()[1]);
    assert_ne!(second, key);
    assert_eq!(h.server.status().api_key.as_deref(), Some(second.as_str()));

    h.server.stop().await;
    assert_eq!(h.server.status().api_key, None);
    assert_eq!(h.server.endpoint(), None);
}

#[test]
fn the_status_serializes_the_key_for_the_renderer() {
    let status = ServerStatus {
        available: true,
        running: true,
        port: Some(8221),
        api_key: Some("k".into()),
        model_path: None,
        model_name: None,
        backend: Some("metal".into()),
        gpu_accelerated: true,
    };
    let value = serde_json::to_value(status).unwrap();
    assert_eq!(value["apiKey"], "k");
    assert_eq!(value["port"], 8221);
}

#[test]
fn env_prepends_to_an_existing_library_path_and_keeps_a_fit_override() {
    let env = build_env(Path::new("/app/bin"), Some("/opt/lib"), Some("on"));
    assert_eq!(
        env[0],
        ("DYLD_LIBRARY_PATH".into(), "/app/bin:/opt/lib".into())
    );
    assert_eq!(env[1], ("LLAMA_ARG_FIT".into(), "on".into()));
    assert_eq!(
        build_env(Path::new("/app/bin"), None, None)[1].1,
        "off".to_string()
    );
}

#[test]
fn without_a_cache_bound_or_drafter_those_flags_are_absent() {
    let args = build_args(Path::new("/m.gguf"), 8222, &StartOptions::default());
    assert!(
        !args
            .iter()
            .any(|a| a == "--cache-ram" || a == "--model-draft")
    );
    assert_eq!(
        args[args.iter().position(|a| a == "--ctx-size").unwrap() + 1],
        "4096"
    );
    let rewritten = with_context_size(&args, 16384);
    assert_eq!(
        rewritten[rewritten.iter().position(|a| a == "--ctx-size").unwrap() + 1],
        "16384"
    );
}

// --- startup failures

#[tokio::test]
async fn a_missing_binary_or_model_fails_before_spawning() {
    let (h, _) = harness();
    let error = h
        .server
        .start(&h.model("missing.gguf"), ctx(16384))
        .await
        .unwrap_err();
    assert!(error.message.starts_with("Model file not found"));
    std::fs::remove_file(h.root.join("bin/llama-server-darwin-arm64")).unwrap();
    h.server.reset_gpu_detection();
    assert!(!h.server.is_available());
    let error = h.server.start(&h.main(), ctx(16384)).await.unwrap_err();
    assert_eq!(error.message, "llama-server binary not found");
    assert_eq!(h.spawner.spawn_count(), 0);
}

#[tokio::test]
async fn a_process_that_dies_during_startup_reports_signal_and_output() {
    let (h, _) = harness();
    h.spawner.set_behaviour(|_| FakeOutcome::Dies {
        code: None,
        signal: Some(libc::SIGKILL),
        stderr: "ggml_metal: failed to allocate buffer".into(),
    });
    let error = h.server.start(&h.main(), ctx(16384)).await.unwrap_err();
    assert!(
        error
            .message
            .starts_with("llama-server process died during startup (signal: SIGKILL)")
    );
    assert!(error.message.contains("insufficient memory"));
    assert!(
        error
            .message
            .contains("Process output: ggml_metal: failed to allocate buffer")
    );
    assert!(!h.server.is_ready());

    h.spawner.set_behaviour(|_| FakeOutcome::Dies {
        code: Some(1),
        signal: None,
        stderr: String::new(),
    });
    let error = h.server.start(&h.main(), ctx(16384)).await.unwrap_err();
    assert_eq!(
        error.message,
        "llama-server process died during startup (exit code: 1)"
    );
}

#[tokio::test(start_paused = true)]
async fn a_server_that_never_turns_healthy_times_out_after_120_seconds() {
    let (h, client) = harness();
    client.healthy.store(false, Ordering::SeqCst);
    let started = tokio::time::Instant::now();
    let error = h.server.start(&h.main(), ctx(16384)).await.unwrap_err();
    assert_eq!(
        error.message,
        "llama-server failed to start within 120000ms"
    );
    assert!(started.elapsed() >= Duration::from_secs(120));
    assert!(
        started.elapsed() < Duration::from_secs(121),
        "polled every 500 ms"
    );
    assert_eq!(
        h.spawner.sigterms(),
        1,
        "the half-started process is killed"
    );
}

#[tokio::test]
async fn nothing_spawns_before_stale_sidecars_are_reaped() {
    let (gate_tx, gate_rx) = watch::channel(false);
    let client = Arc::new(FakeClient::default());
    let h = harness_with(client, 8221, |deps| deps.spawn_gate = Some(gate_rx));
    let server = h.server.clone();
    let main = h.main();
    let start = tokio::spawn(async move { server.start(&main, ctx(16384)).await });
    settle().await;
    assert_eq!(h.spawner.spawn_count(), 0);
    gate_tx.send_replace(true);
    start.await.unwrap().unwrap();
    assert_eq!(h.spawner.spawn_count(), 1);
}

#[tokio::test]
async fn the_pid_file_tracks_the_running_process() {
    let client = Arc::new(FakeClient::default());
    let pids_dir = tempfile::tempdir().unwrap();
    let pids = PidFile::new(pids_dir.path());
    let h = harness_with(client, 8221, |deps| deps.pid_file = Some(pids.clone()));
    h.server.start(&h.main(), ctx(16384)).await.unwrap();
    assert_eq!(pids.read_all().len(), 1);
    assert_eq!(pids.read_all()[0].0, "llama");
    h.server.stop().await;
    settle().await;
    assert!(pids.read_all().is_empty());
}

#[tokio::test]
async fn a_crash_after_startup_marks_the_server_not_running() {
    let (h, _) = harness();
    h.server.start(&h.main(), ctx(32768)).await.unwrap();
    // Simulate the OS killing it.
    let process = h.server.state().process.clone().unwrap();
    process.signal(libc::SIGKILL);
    settle().await;
    assert!(!h.server.is_ready());
    assert!(!h.server.has_process());
    assert_eq!(
        h.server.context_size(),
        Some(32768),
        "only stop() forgets the context"
    );
}

#[tokio::test]
async fn stop_escalates_to_sigkill_when_sigterm_is_ignored() {
    let client = Arc::new(FakeClient::default());
    let h = harness_with(client, 8221, |deps| {
        deps.timings.stop_grace = Duration::from_millis(50);
    });
    h.spawner.honour_sigterm.store(false, Ordering::SeqCst);
    h.server.start(&h.main(), ctx(16384)).await.unwrap();
    h.server.stop().await;
    let signals: Vec<i32> = h
        .spawner
        .signals
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.1)
        .collect();
    assert_eq!(signals, vec![libc::SIGTERM, libc::SIGKILL]);
}

#[tokio::test(start_paused = true)]
async fn three_failed_health_checks_mark_the_server_not_ready() {
    let (h, client) = harness();
    h.server.start(&h.main(), ctx(16384)).await.unwrap();
    client.healthy.store(false, Ordering::SeqCst);
    for _ in 0..2 {
        tokio::time::advance(Duration::from_secs(5)).await;
        settle().await;
    }
    assert!(h.server.is_ready(), "two failures are tolerated");
    tokio::time::advance(Duration::from_secs(5)).await;
    settle().await;
    assert!(!h.server.is_ready());
}

// --- drafter

#[tokio::test]
async fn restarts_only_when_the_model_or_drafter_presence_changes() {
    let (h, _) = harness();
    let draft = |on: bool| StartOptions {
        draft_model_path: on.then(|| h.model("draft.gguf")),
        ..StartOptions::default()
    };
    h.server.start(&h.main(), draft(false)).await.unwrap();
    h.server.start(&h.main(), draft(false)).await.unwrap();
    assert_eq!(h.spawner.spawn_count(), 1);
    h.server.start(&h.main(), draft(true)).await.unwrap();
    assert_eq!(h.spawner.spawn_count(), 2);
    h.server.start(&h.main(), draft(true)).await.unwrap();
    assert_eq!(h.spawner.spawn_count(), 2);
    h.server.start(&h.main(), draft(false)).await.unwrap();
    assert_eq!(h.spawner.spawn_count(), 3);
}

#[tokio::test]
async fn stop_clears_both_drafter_paths() {
    let (h, _) = harness();
    h.server
        .start(
            &h.main(),
            StartOptions {
                draft_model_path: Some(h.model("draft.gguf")),
                ..StartOptions::default()
            },
        )
        .await
        .unwrap();
    assert!(h.server.state().active_draft_model_path.is_some());
    h.server.stop().await;
    let state = h.server.state();
    assert!(state.draft_model_path.is_none());
    assert!(state.active_draft_model_path.is_none());
}

// --- idle unload

#[tokio::test(start_paused = true)]
async fn asking_for_the_running_server_postpones_the_idle_stop() {
    let (h, _) = harness();
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    tokio::time::advance(4 * MINUTE).await;
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    tokio::time::advance(4 * MINUTE).await;
    settle().await;
    assert_eq!(
        h.spawner.sigterms(),
        0,
        "an active server must not idle out"
    );
    tokio::time::advance(MINUTE + Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 1, "it still stops once really idle");
}

#[tokio::test(start_paused = true)]
async fn a_server_still_generating_is_not_stopped_mid_stream() {
    let (h, client) = harness();
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    client.processing.store(true, Ordering::SeqCst);
    tokio::time::advance(5 * MINUTE + Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 0, "a busy slot postpones the stop");
    client.processing.store(false, Ordering::SeqCst);
    tokio::time::advance(5 * MINUTE + Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_server_that_cannot_answer_metrics_is_stopped() {
    let (h, client) = harness();
    client.metrics_fail.store(true, Ordering::SeqCst);
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    tokio::time::advance(5 * MINUTE + Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 1);
    assert!(!h.server.has_process());
}

#[tokio::test(start_paused = true)]
async fn a_request_arriving_while_metrics_is_checked_keeps_the_server() {
    let (h, client) = harness();
    let (answer, pending) = oneshot::channel();
    *client.pending_metrics.lock().unwrap() = Some(pending);
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    tokio::time::advance(5 * MINUTE + Duration::from_millis(1)).await;
    settle().await;
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    answer
        .send(Some("llamacpp:requests_processing 0\n".into()))
        .unwrap();
    settle().await;
    assert_eq!(h.spawner.sigterms(), 0, "the newer timer owns the decision");
}

#[tokio::test(start_paused = true)]
async fn a_resident_server_stays_loaded() {
    let (h, _) = harness();
    h.server.set_keep_resident(true);
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    tokio::time::advance(30 * MINUTE).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 0);
    assert!(!h.server.has_idle_timer());
}

#[tokio::test(start_paused = true)]
async fn enabling_residency_cancels_a_pending_unload() {
    let (h, _) = harness();
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    tokio::time::advance(4 * MINUTE).await;
    h.server.set_keep_resident(true);
    tokio::time::advance(2 * MINUTE).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 0);
    assert!(!h.server.has_idle_timer());
}

#[tokio::test(start_paused = true)]
async fn a_resident_server_still_restarts_to_grow() {
    let (h, _) = harness();
    h.server.set_keep_resident(true);
    h.server.start(&h.main(), ctx(16384)).await.unwrap();
    h.server.start(&h.main(), ctx(32768)).await.unwrap();
    assert_eq!(h.spawner.sigterms(), 1);
    assert_eq!(h.server.context_size(), Some(32768));
    assert!(!h.server.has_idle_timer());
}

#[tokio::test(start_paused = true)]
async fn turning_residency_off_idles_the_server_out_five_minutes_later() {
    let (h, _) = harness();
    h.server.set_keep_resident(true);
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    tokio::time::advance(30 * MINUTE).await;
    h.server.set_keep_resident(false);
    tokio::time::advance(5 * MINUTE - Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(
        h.spawner.sigterms(),
        0,
        "the countdown starts when residency goes off"
    );
    tokio::time::advance(Duration::from_millis(2)).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 1);
}

#[tokio::test(start_paused = true)]
async fn turning_residency_off_during_a_start_leaves_the_countdown_to_that_start() {
    let (h, _) = harness();
    let release = h.spawner.hold();
    h.server.set_keep_resident(true);
    let server = h.server.clone();
    let main = h.main();
    let starting = tokio::spawn(async move { server.start(&main, StartOptions::default()).await });
    settle().await;
    h.server.set_keep_resident(false);
    tokio::time::advance(30 * MINUTE).await;
    settle().await;
    assert_eq!(
        h.spawner.sigterms(),
        0,
        "a server still starting is never idled out"
    );

    release.send_replace(true);
    starting.await.unwrap().unwrap();
    tokio::time::advance(5 * MINUTE).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 1);
}

#[tokio::test(start_paused = true)]
async fn turning_residency_off_during_a_restart_leaves_the_countdown_to_the_new_start() {
    let (h, _) = harness();
    h.server.set_keep_resident(true);
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();

    let release = h.spawner.hold();
    let server = h.server.clone();
    let other = h.model("other.gguf");
    let restarting =
        tokio::spawn(async move { server.start(&other, StartOptions::default()).await });
    settle().await;
    h.server.set_keep_resident(false);
    tokio::time::advance(30 * MINUTE).await;
    settle().await;
    assert_eq!(
        h.spawner.sigterms(),
        1,
        "only the restart stopped the old server"
    );

    release.send_replace(true);
    restarting.await.unwrap().unwrap();
    tokio::time::advance(5 * MINUTE).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 2);
}

#[tokio::test(start_paused = true)]
async fn resyncing_an_unchanged_residency_setting_leaves_the_countdown_alone() {
    let (h, _) = harness();
    h.server
        .start(&h.main(), StartOptions::default())
        .await
        .unwrap();
    tokio::time::advance(4 * MINUTE).await;
    h.server.set_keep_resident(false);
    tokio::time::advance(MINUTE + Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(h.spawner.sigterms(), 1);
}

// --- talking to a running server (real HTTP against a mock llama-server)

async fn live(
    routes: impl Fn(&crate::test_support::Request) -> Reply + Send + Sync + 'static,
) -> (Harness, MockServer) {
    let mock = MockServer::start(move |req| {
        if req.path == "/health" {
            return Reply::json(200, json!({ "status": "ok" }));
        }
        routes(req)
    })
    .await;
    let client = Arc::new(ReqwestServerClient::new().unwrap());
    let h = harness_with(client, mock.port, |_| {});
    h.server.start(&h.main(), ctx(16384)).await.unwrap();
    (h, mock)
}

fn messages() -> Value {
    json!([{ "role": "user", "content": "hi" }])
}

#[tokio::test]
async fn a_context_overflow_is_a_typed_error_not_llama_cpp_json() {
    let (h, _mock) = live(|_| {
        Reply::json(
            400,
            json!({ "error": {
                "code": 400,
                "message": "request (20514 tokens) exceeds the available context size (16384 tokens)",
                "type": "exceed_context_size_error",
                "n_prompt_tokens": 20514,
                "n_ctx": 16384
            }}),
        )
    })
    .await;
    let error = h
        .server
        .inference(messages(), ChatOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, Some("CONTEXT_TOO_LARGE"));
    assert_eq!(error.needed_tokens, Some(20514));
    assert_eq!(error.max_context_tokens, Some(16384));
    assert!(
        !error.message.contains('{'),
        "raw JSON leaked: {}",
        error.message
    );
}

#[tokio::test]
async fn other_failures_keep_their_shape() {
    let (h, _mock) = live(|_| Reply::bytes(500, b"internal error".to_vec())).await;
    let error = h
        .server
        .inference(messages(), ChatOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, None);
    assert!(
        error
            .message
            .starts_with("llama-server returned status 500")
    );
}

/// The mock answers like llama-server started with `--api-key`: `/health`
/// is public, everything else needs `Authorization: Bearer <key>`.
#[tokio::test]
async fn every_request_but_health_carries_the_launch_key() {
    let expected: Arc<Mutex<Option<String>>> = Arc::default();
    type Seen = Vec<(String, Option<String>)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let (exp, log) = (expected.clone(), seen.clone());
    let mock = MockServer::start(move |req| {
        let auth = req.header("authorization").map(str::to_string);
        log.lock().unwrap().push((req.path.clone(), auth.clone()));
        if req.path == "/health" {
            return Reply::json(200, json!({ "status": "ok" }));
        }
        let want = exp.lock().unwrap().clone().map(|k| format!("Bearer {k}"));
        if want.is_none() || auth != want {
            return Reply::json(401, json!({ "error": { "message": "Invalid API Key" } }));
        }
        match req.path.as_str() {
            "/props" => Reply::json(
                200,
                json!({ "default_generation_settings": { "n_ctx": 8192 } }),
            ),
            "/apply-template" => Reply::json(200, json!({ "prompt": "templated" })),
            "/tokenize" => Reply::json(200, json!({ "tokens": [1, 2, 3] })),
            "/v1/chat/completions" => Reply::json(
                200,
                json!({ "choices": [{ "finish_reason": "stop", "message": { "content": "ok" } }] }),
            ),
            _ => Reply::bytes(404, vec![]),
        }
    })
    .await;
    let client = Arc::new(ReqwestServerClient::new().unwrap());
    let h = harness_with(client.clone(), mock.port, |_| {});
    // The mock learns the key the way llama-server does: from the spawn.
    let spawner = h.spawner.clone();
    let exp = expected.clone();
    spawner.set_behaviour(move |spec| {
        *exp.lock().unwrap() = Some(spawned_key(spec));
        FakeOutcome::Live
    });
    h.server.start(&h.main(), ctx(8192)).await.unwrap();
    let key = expected.lock().unwrap().clone().unwrap();

    assert_eq!(h.server.usable_context_size().await, Some(8192));
    assert_eq!(
        h.server.count_prompt_tokens(&messages(), None).await,
        Some(3)
    );
    assert_eq!(
        h.server
            .inference(messages(), ChatOptions::default())
            .await
            .unwrap(),
        "ok"
    );
    let endpoint = h.server.endpoint().unwrap();
    let metrics_mock = MockServer::start(|req| match req.header("authorization") {
        Some(_) => Reply::bytes(200, b"llamacpp:requests_processing 1\n".to_vec()),
        None => Reply::bytes(401, vec![]),
    })
    .await;
    let metrics_endpoint = Endpoint::new(metrics_mock.port, endpoint.api_key.clone());
    let metrics = client
        .get_text(&metrics_endpoint, "/metrics", Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(crate::client::requests_in_flight(&metrics), Some(1));

    let seen = seen.lock().unwrap().clone();
    let bearer = format!("Bearer {key}");
    for (path, auth) in &seen {
        if path != "/health" {
            assert_eq!(
                auth.as_deref(),
                Some(bearer.as_str()),
                "{path} without the key"
            );
        }
    }
    for path in [
        "/props",
        "/apply-template",
        "/tokenize",
        "/v1/chat/completions",
    ] {
        assert!(seen.iter().any(|(p, _)| p == path), "{path} was not called");
    }

    // A wrong key is refused like llama-server does.
    let wrong = Endpoint::new(mock.port, "not-the-key");
    let (status, _) = client
        .post(
            &wrong,
            "/v1/chat/completions",
            json!({}),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_eq!(status, 401);
    assert!(
        client.health(&wrong, Duration::from_secs(2)).await,
        "/health stays public"
    );
}

#[tokio::test]
async fn usable_context_size_reads_props() {
    let (h, _mock) = live(|req| match req.path.as_str() {
        "/props" => Reply::json(
            200,
            json!({ "default_generation_settings": { "n_ctx": 32768 } }),
        ),
        _ => Reply::bytes(404, vec![]),
    })
    .await;
    assert_eq!(h.server.usable_context_size().await, Some(32768));
}

#[tokio::test]
async fn usable_context_size_is_none_when_props_is_unreadable() {
    let (h, _mock) = live(|req| match req.path.as_str() {
        "/props" => Reply::json(500, json!({})),
        _ => Reply::bytes(404, vec![]),
    })
    .await;
    assert_eq!(h.server.usable_context_size().await, None);
    let (h, _mock) = live(|_| Reply::bytes(404, vec![])).await;
    assert_eq!(h.server.usable_context_size().await, None);
}

#[tokio::test]
async fn count_prompt_tokens_prices_the_rendered_template() {
    let templated = Arc::new(Mutex::new(Value::Null));
    let tokenized = Arc::new(Mutex::new(Value::Null));
    let (t, k) = (templated.clone(), tokenized.clone());
    let (h, _mock) = live(move |req| match req.path.as_str() {
        "/apply-template" => {
            *t.lock().unwrap() = req.json();
            Reply::json(
                200,
                json!({ "prompt": "<|im_start|>system\nSYS<|im_end|>\nhello" }),
            )
        }
        "/tokenize" => {
            *k.lock().unwrap() = req.json();
            Reply::json(200, json!({ "tokens": vec![0; 20514] }))
        }
        _ => Reply::bytes(404, vec![]),
    })
    .await;
    let msgs = json!([
        { "role": "system", "content": "SYS" },
        { "role": "user", "content": "hello" }
    ]);
    assert_eq!(
        h.server.count_prompt_tokens(&msgs, Some(true)).await,
        Some(20514)
    );
    assert_eq!(templated.lock().unwrap()["messages"], msgs);
    assert_eq!(
        templated.lock().unwrap()["chat_template_kwargs"],
        json!({ "enable_thinking": false })
    );
    assert_eq!(
        tokenized.lock().unwrap()["content"],
        "<|im_start|>system\nSYS<|im_end|>\nhello"
    );
}

#[tokio::test]
async fn count_prompt_tokens_is_none_when_the_server_cannot_measure() {
    let (h, _mock) = live(|_| Reply::json(500, json!({}))).await;
    assert_eq!(h.server.count_prompt_tokens(&messages(), None).await, None);
}

/// A server that answers every completion with `message` and records bodies.
async fn replying(
    message: Value,
    finish: &'static str,
) -> (Harness, MockServer, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let (h, mock) = live(move |req| {
        seen.lock().unwrap().push(req.json());
        Reply::json(
            200,
            json!({ "choices": [{ "message": message.clone(), "finish_reason": finish }] }),
        )
    })
    .await;
    (h, mock, requests)
}

fn thinking(disable: Option<bool>, strict: bool) -> ChatOptions {
    ChatOptions {
        disable_thinking: disable,
        require_complete_output: strict,
        ..ChatOptions::default()
    }
}

#[tokio::test]
async fn reasoning_never_stands_in_for_the_answer_when_thinking_was_on() {
    let reasoning_only =
        json!({ "content": "", "reasoning_content": "Thinking Process:\n\n1. Analyze" });
    let (h, _m, requests) = replying(reasoning_only.clone(), "length").await;
    for strict in [false, true] {
        let error = h
            .server
            .inference(messages(), thinking(Some(false), strict))
            .await
            .unwrap_err();
        assert_eq!(error.code, Some("OUTPUT_TRUNCATED"));
    }
    assert!(
        requests.lock().unwrap()[0]
            .get("chat_template_kwargs")
            .is_none()
    );

    let (h, _m, _) = replying(reasoning_only, "stop").await;
    assert_eq!(
        h.server
            .inference(messages(), thinking(Some(false), true))
            .await
            .unwrap(),
        ""
    );
}

#[tokio::test]
async fn a_cut_off_reply_with_no_content_is_truncated_even_with_thinking_suppressed() {
    let (h, _m, _) = replying(
        json!({ "content": "", "reasoning_content": "analysis..." }),
        "length",
    )
    .await;
    let error = h
        .server
        .inference(messages(), thinking(Some(true), false))
        .await
        .unwrap_err();
    assert_eq!(error.code, Some("OUTPUT_TRUNCATED"));
}

#[tokio::test]
async fn a_cut_off_reply_with_content_is_returned_to_lenient_callers() {
    let (h, _m, _) = replying(
        json!({ "content": "Cleaned te", "reasoning_content": "" }),
        "length",
    )
    .await;
    assert_eq!(
        h.server
            .inference(messages(), thinking(Some(true), false))
            .await
            .unwrap(),
        "Cleaned te"
    );
    let error = h
        .server
        .inference(messages(), thinking(None, true))
        .await
        .unwrap_err();
    assert_eq!(error.code, Some("OUTPUT_TRUNCATED"));
}

#[tokio::test]
async fn an_answer_routed_into_reasoning_content_is_returned_when_thinking_is_suppressed() {
    let (h, _m, requests) = replying(
        json!({ "content": "", "reasoning_content": "Cleaned text." }),
        "stop",
    )
    .await;
    assert_eq!(
        h.server
            .inference(messages(), thinking(Some(true), false))
            .await
            .unwrap(),
        "Cleaned text."
    );
    assert_eq!(
        h.server
            .inference(messages(), ChatOptions::default())
            .await
            .unwrap(),
        "Cleaned text."
    );
    assert_eq!(
        requests.lock().unwrap()[0]["chat_template_kwargs"],
        json!({ "enable_thinking": false })
    );
}

#[tokio::test]
async fn content_wins_over_reasoning_content_either_way() {
    let (h, _m, requests) = replying(
        json!({ "content": "Cleaned text.", "reasoning_content": "Thinking Process:" }),
        "stop",
    )
    .await;
    for options in [thinking(Some(false), false), ChatOptions::default()] {
        assert_eq!(
            h.server.inference(messages(), options).await.unwrap(),
            "Cleaned text."
        );
    }
    let body = &requests.lock().unwrap()[1];
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(body["max_tokens"], 512);
    assert_eq!(body["stream"], false);
}

#[tokio::test]
async fn a_structured_reply_must_finish_cleanly() {
    let (h, _m, requests) = replying(json!({ "content": "{\"a\":1}" }), "stop").await;
    let format = json!({ "type": "json_object" });
    let options = ChatOptions {
        response_format: Some(format.clone()),
        ..ChatOptions::default()
    };
    assert_eq!(
        h.server
            .inference(messages(), options.clone())
            .await
            .unwrap(),
        "{\"a\":1}"
    );
    assert_eq!(requests.lock().unwrap()[0]["response_format"], format);
    let (h, _m, _) = replying(json!({ "content": "{\"a\":1}" }), "eos").await;
    let error = h.server.inference(messages(), options).await.unwrap_err();
    assert_eq!(error.code, Some("OUTPUT_COMPLETION_UNVERIFIED"));
}

#[tokio::test]
async fn cancelling_aborts_the_running_request() {
    let (h, _mock) = live(|_| {
        Reply::json(
            200,
            json!({ "choices": [{ "message": { "content": "late" }, "finish_reason": "stop" }] }),
        )
        .delayed(Duration::from_secs(30))
    })
    .await;
    let server = h.server.clone();
    let running =
        tokio::spawn(async move { server.inference(messages(), ChatOptions::default()).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.server.cancel_inference();
    let error = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.message, "llama-server request failed: cancelled");
    assert!(h.server.has_idle_timer(), "the idle countdown resumes");
}

#[tokio::test]
async fn inference_needs_a_running_server() {
    let (h, _) = harness();
    let error = h
        .server
        .inference(messages(), ChatOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.message, "llama-server is not running");
}
