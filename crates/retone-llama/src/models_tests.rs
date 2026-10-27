//! Model manager tests: context sizing, the Gemma MTP drafter, downloads
//! through the manager, and the bridge. The model file is a real GGUF header padded past the size check,
//! so the ceiling goes through the production path.

use super::*;
use crate::bridge::LocalReasoningBridge;
use crate::client::ReqwestServerClient;
use crate::download::DownloadTimings;
use crate::gguf::fixtures;
use crate::server::{ServerDeps, ServerTimings};
use crate::test_support::{FakeOutcome, FakeSpawner, MockServer, Reply};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const SHORT_PROMPT: &str = "Clean up this sentence.";

fn long_prompt() -> String {
    "word ".repeat(12000)
}

#[derive(Default)]
struct Calls {
    tokenize: AtomicUsize,
    completions: AtomicUsize,
}

struct Setup {
    _dir: tempfile::TempDir,
    _mock: MockServer,
    manager: Arc<ModelManager>,
    spawner: Arc<FakeSpawner>,
    model_id: String,
    calls: Arc<Calls>,
    completion_body: Arc<Mutex<Option<Value>>>,
}

impl Setup {
    /// Context sizes of the starts made after setup.
    fn restarts(&self) -> Vec<u64> {
        self.spawner.contexts()
    }

    fn max_tokens(&self) -> u64 {
        self.completion_body.lock().unwrap().as_ref().unwrap()["max_tokens"]
            .as_u64()
            .unwrap()
    }
}

struct Opts {
    token_count: Option<usize>,
    total_memory: f64,
    finish_reason: &'static str,
    unified: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            token_count: None,
            total_memory: 48.0 * GIB,
            finish_reason: "stop",
            unified: true,
        }
    }
}

fn first_model() -> &'static LocalModel {
    &local_providers()[0].models[0]
}

async fn setup(opts: Opts) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("llama-server-darwin-arm64"), "").unwrap();
    let models_dir = dir.path().join("models");

    let spawner = FakeSpawner::new();
    let calls = Arc::new(Calls::default());
    let completion_body = Arc::new(Mutex::new(None));
    let (c, body, s) = (calls.clone(), completion_body.clone(), spawner.clone());
    let Opts {
        token_count,
        total_memory,
        finish_reason,
        unified,
    } = opts;
    let mock = MockServer::start(move |req| match req.path.as_str() {
        "/health" => Reply::json(200, json!({ "status": "ok" })),
        // The window of the most recent start, as the real server reports.
        "/props" => Reply::json(
            200,
            json!({ "default_generation_settings": { "n_ctx": s.contexts().last().copied().unwrap_or(16384) } }),
        ),
        "/apply-template" => Reply::json(200, json!({ "prompt": "RENDERED" })),
        "/tokenize" => {
            c.tokenize.fetch_add(1, Ordering::SeqCst);
            match token_count {
                Some(n) => Reply::json(200, json!({ "tokens": vec![0; n] })),
                None => Reply::json(500, json!({})),
            }
        }
        _ => {
            c.completions.fetch_add(1, Ordering::SeqCst);
            *body.lock().unwrap() = Some(req.json());
            Reply::json(
                200,
                json!({ "choices": [{ "finish_reason": finish_reason, "message": { "content": "done" } }] }),
            )
        }
    })
    .await;

    let port = mock.port;
    let server = LlamaServer::new(ServerDeps {
        spawner: Arc::new(spawner.clone()),
        client: Arc::new(ReqwestServerClient::new().unwrap()),
        pick_port: Arc::new(move || Ok(port)),
        binary_dirs: vec![bin],
        pid_file: None,
        timings: ServerTimings::default(),
        runtime: tokio::runtime::Handle::current(),
        spawn_gate: None,
    });
    let manager = Arc::new(
        ModelManager::new(models_dir.clone(), server, reqwest::Client::new()).with_machine(
            Machine {
                total_memory_bytes: Box::new(move || total_memory),
                unified_memory: unified,
            },
        ),
    );

    let model = first_model();
    let mut file = fixtures::build(&fixtures::llama_3_2_3b(), fixtures::Options::default());
    file.extend(vec![1u8; 1_000_001]);
    std::fs::write(models_dir.join(&model.file_name), file).unwrap();

    // A ready server at the starting context, serving this model.
    let started = manager.start_server_for(&model.id).await.unwrap();
    assert_eq!(started.port, port);
    assert_eq!(started.api_key.len(), 64, "the per-launch key comes back");
    assert_eq!(manager.server().context_size(), Some(16384));
    spawner.clear();

    Setup {
        _dir: dir,
        _mock: mock,
        manager,
        spawner,
        model_id: model.id.clone(),
        calls,
        completion_body,
    }
}

fn opts(system: &str) -> InferenceOptions {
    InferenceOptions {
        system_prompt: system.into(),
        ..InferenceOptions::default()
    }
}

#[tokio::test]
async fn a_short_request_costs_no_measurement_and_no_restart() {
    let s = setup(Opts {
        token_count: Some(40),
        ..Opts::default()
    })
    .await;
    let result = s
        .manager
        .run_inference(
            &s.model_id,
            SHORT_PROMPT,
            &InferenceOptions {
                max_tokens: Some(900),
                ..opts("Clean up dictation.")
            },
        )
        .await
        .unwrap();
    assert_eq!(result, "done");
    assert!(s.restarts().is_empty());
    assert_eq!(s.calls.tokenize.load(Ordering::SeqCst), 0);
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 1);
    assert_eq!(s.max_tokens(), 900);
}

#[tokio::test]
async fn a_long_request_grows_the_context_once_and_succeeds() {
    let s = setup(Opts {
        token_count: Some(20514),
        ..Opts::default()
    })
    .await;
    let result = s
        .manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Write meeting notes."))
        .await
        .unwrap();
    assert_eq!(result, "done");
    assert_eq!(s.restarts(), vec![32768]);
    assert_eq!(s.manager.server().context_size(), Some(32768));
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_request_too_large_for_the_machine_fails_before_reaching_the_model() {
    let s = setup(Opts {
        token_count: Some(200000),
        ..Opts::default()
    })
    .await;
    let error = s
        .manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Write notes."))
        .await
        .unwrap_err();
    assert_eq!(error.code, "CONTEXT_TOO_LARGE");
    assert!(error.details["neededTokens"].as_u64().unwrap() >= 200000);
    assert!(error.details["maxContextTokens"].as_u64().unwrap() > 0);
    assert!(!error.message.contains('{'));
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_broken_measurement_falls_back_to_the_estimate() {
    let s = setup(Opts::default()).await; // /tokenize answers 500
    let result = s
        .manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Write notes."))
        .await
        .unwrap();
    assert_eq!(result, "done");
    assert!(s.calls.tokenize.load(Ordering::SeqCst) > 0);
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_short_request_after_a_long_one_reuses_the_grown_context() {
    let s = setup(Opts {
        token_count: Some(20514),
        ..Opts::default()
    })
    .await;
    s.manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Write notes."))
        .await
        .unwrap();
    assert_eq!(s.restarts(), vec![32768]);
    s.manager
        .run_inference(&s.model_id, SHORT_PROMPT, &opts("Clean up."))
        .await
        .unwrap();
    assert_eq!(s.restarts(), vec![32768]);
}

#[tokio::test]
async fn a_small_machine_refuses_what_a_large_one_accepts() {
    let s = setup(Opts {
        token_count: Some(60000),
        total_memory: 8.0 * GIB,
        ..Opts::default()
    })
    .await;
    let error = s
        .manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Notes."))
        .await
        .unwrap_err();
    assert_eq!(error.code, "CONTEXT_TOO_LARGE");
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_callers_context_floor_cannot_breach_the_ceiling() {
    let s = setup(Opts {
        token_count: Some(40),
        total_memory: 12.0 * GIB,
        ..Opts::default()
    })
    .await;
    s.manager
        .run_inference(
            &s.model_id,
            SHORT_PROMPT,
            &InferenceOptions {
                context_size: Some(131072),
                ..opts("Edit this selection.")
            },
        )
        .await
        .unwrap();
    let restarts = s.restarts();
    assert_eq!(restarts.len(), 1);
    assert!(restarts[0] > 16384 && restarts[0] < 131072, "{restarts:?}");
    assert_eq!(s.manager.server().context_size(), Some(restarts[0]));
}

#[tokio::test]
async fn a_prompt_that_fits_gets_a_smaller_answer_rather_than_a_refusal() {
    let s = setup(Opts {
        token_count: Some(13000),
        total_memory: 8.0 * GIB,
        ..Opts::default()
    })
    .await;
    let result = s
        .manager
        .run_inference(
            &s.model_id,
            &long_prompt(),
            &InferenceOptions {
                max_tokens: Some(4096),
                ..opts("Write meeting notes.")
            },
        )
        .await
        .unwrap();
    assert_eq!(result, "done");
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 1);
    assert_eq!(s.max_tokens(), 16384 - 13000);
}

#[tokio::test]
async fn a_prompt_leaving_no_room_for_an_answer_is_refused() {
    let s = setup(Opts {
        token_count: Some(16000),
        total_memory: 8.0 * GIB,
        ..Opts::default()
    })
    .await;
    let error = s
        .manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Write notes."))
        .await
        .unwrap_err();
    assert_eq!(error.code, "CONTEXT_TOO_LARGE");
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_selection_edit_is_refused_rather_than_clipped() {
    let s = setup(Opts {
        token_count: Some(13000),
        total_memory: 8.0 * GIB,
        ..Opts::default()
    })
    .await;
    let error = s
        .manager
        .run_inference(
            &s.model_id,
            &long_prompt(),
            &InferenceOptions {
                max_tokens: Some(8192),
                require_complete_output: true,
                ..opts("Rewrite the selection.")
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "CONTEXT_TOO_LARGE");
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 0);
}

fn rewrite() -> InferenceOptions {
    InferenceOptions {
        max_tokens: Some(8192),
        require_complete_output: true,
        refuse_clipped_by_window: true,
        ..opts("Revise the summary.")
    }
}

#[tokio::test]
async fn a_rewrite_gets_a_smaller_answer_but_never_a_clipped_one() {
    let fits = setup(Opts {
        token_count: Some(13000),
        total_memory: 8.0 * GIB,
        ..Opts::default()
    })
    .await;
    assert_eq!(
        fits.manager
            .run_inference(&fits.model_id, &long_prompt(), &rewrite())
            .await
            .unwrap(),
        "done"
    );
    assert_eq!(fits.max_tokens(), 16384 - 13000);

    let clipped = setup(Opts {
        token_count: Some(13000),
        total_memory: 8.0 * GIB,
        finish_reason: "length",
        ..Opts::default()
    })
    .await;
    let error = clipped
        .manager
        .run_inference(&clipped.model_id, &long_prompt(), &rewrite())
        .await
        .unwrap_err();
    assert_eq!(error.code, "CONTEXT_TOO_LARGE");

    let capped = setup(Opts {
        token_count: Some(40),
        finish_reason: "length",
        ..Opts::default()
    })
    .await;
    let error = capped
        .manager
        .run_inference(&capped.model_id, SHORT_PROMPT, &rewrite())
        .await
        .unwrap_err();
    assert_eq!(error.code, "OUTPUT_TRUNCATED");
}

#[tokio::test]
async fn a_reply_the_window_clipped_is_refused_when_asked() {
    let s = setup(Opts {
        token_count: Some(13000),
        total_memory: 8.0 * GIB,
        finish_reason: "length",
        ..Opts::default()
    })
    .await;
    let error = s
        .manager
        .run_inference(
            &s.model_id,
            &long_prompt(),
            &InferenceOptions {
                max_tokens: Some(4096),
                refuse_clipped_by_window: true,
                ..opts("Write meeting notes.")
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "CONTEXT_TOO_LARGE");
    assert_eq!(error.details["modelId"], s.model_id);
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 1);
    assert_eq!(s.max_tokens(), 16384 - 13000);
}

#[tokio::test]
async fn a_trimmed_reply_that_finishes_is_kept() {
    let s = setup(Opts {
        token_count: Some(13000),
        total_memory: 8.0 * GIB,
        ..Opts::default()
    })
    .await;
    let result = s
        .manager
        .run_inference(
            &s.model_id,
            &long_prompt(),
            &InferenceOptions {
                max_tokens: Some(4096),
                refuse_clipped_by_window: true,
                ..opts("Write meeting notes.")
            },
        )
        .await
        .unwrap();
    assert_eq!(result, "done");
}

#[tokio::test]
async fn a_reply_cut_at_the_callers_own_cap_is_kept_when_only_window_clips_are_refused() {
    let s = setup(Opts {
        token_count: Some(40),
        finish_reason: "length",
        ..Opts::default()
    })
    .await;
    let result = s
        .manager
        .run_inference(
            &s.model_id,
            SHORT_PROMPT,
            &InferenceOptions {
                max_tokens: Some(4096),
                refuse_clipped_by_window: true,
                ..InferenceOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(result, "done");
    assert_eq!(s.max_tokens(), 4096);
}

#[tokio::test]
async fn a_grow_that_fails_comes_back_at_the_window_that_worked() {
    let s = setup(Opts {
        token_count: Some(20514),
        ..Opts::default()
    })
    .await;
    s.spawner.set_behaviour(|spec| {
        if spec
            .arg_value("--ctx-size")
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 16384
        {
            FakeOutcome::Dies {
                code: None,
                signal: Some(libc::SIGKILL),
                stderr: "ggml_metal: failed to allocate buffer of size 4096.00 MiB".into(),
            }
        } else {
            FakeOutcome::Live
        }
    });
    let error = s
        .manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Write notes."))
        .await
        .unwrap_err();
    assert_eq!(error.code, "CONTEXT_TOO_LARGE");
    assert_eq!(s.restarts(), vec![32768, 16384]);
    assert!(s.manager.server().is_ready(), "never left without a server");
}

#[tokio::test]
async fn a_server_that_cannot_come_back_reports_a_typed_failure() {
    let s = setup(Opts {
        token_count: Some(20514),
        ..Opts::default()
    })
    .await;
    s.spawner.set_behaviour(|_| FakeOutcome::Dies {
        code: None,
        signal: Some(libc::SIGKILL),
        stderr: "ggml_metal_graph_compute: command buffer 0 failed with status 5".into(),
    });
    let error = s
        .manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Write notes."))
        .await
        .unwrap_err();
    assert_eq!(error.code, "LOCAL_SERVER_UNAVAILABLE");
    assert!(!error.message.contains("Process output"));
    assert!(!error.details.to_string().contains("Process output"));
}

#[tokio::test]
async fn an_answer_too_small_to_be_worth_generating_is_refused() {
    let s = setup(Opts {
        token_count: Some(16100),
        total_memory: 8.0 * GIB,
        ..Opts::default()
    })
    .await;
    let error = s
        .manager
        .run_inference(
            &s.model_id,
            &long_prompt(),
            &InferenceOptions {
                max_tokens: Some(4096),
                ..opts("Write notes.")
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "CONTEXT_TOO_LARGE");
    assert_eq!(s.calls.completions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_server_that_already_died_is_not_restored_to_a_window_it_lost() {
    let s = setup(Opts {
        token_count: Some(20514),
        ..Opts::default()
    })
    .await;
    s.manager.server().simulate_crash();
    for _ in 0..50 {
        if !s.manager.server().is_ready() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(!s.manager.server().is_ready());
    s.spawner.clear();
    s.spawner.set_behaviour(|_| FakeOutcome::Dies {
        code: None,
        signal: Some(libc::SIGKILL),
        stderr: String::new(),
    });
    let error = s
        .manager
        .run_inference(&s.model_id, &long_prompt(), &opts("Write notes."))
        .await
        .unwrap_err();
    assert_eq!(error.code, "LOCAL_SERVER_UNAVAILABLE");
    assert_eq!(s.restarts(), vec![32768], "no second attempt");
}

#[tokio::test]
async fn only_unified_memory_skips_the_unverified_gpu_cap() {
    let apple = setup(Opts {
        token_count: Some(40),
        total_memory: 64.0 * GIB,
        ..Opts::default()
    })
    .await;
    let intel = setup(Opts {
        token_count: Some(40),
        total_memory: 64.0 * GIB,
        unified: false,
        ..Opts::default()
    })
    .await;
    let model = first_model();
    let unified = apple
        .manager
        .context_ceiling(model, &apple.manager.model_path(model))
        .await;
    let discrete = intel
        .manager
        .context_ceiling(model, &intel.manager.model_path(model))
        .await;
    assert_ne!(unified.reason, crate::context::CeilingReason::GpuCap);
    assert_eq!(discrete.reason, crate::context::CeilingReason::GpuCap);
    assert!(discrete.ceiling < unified.ceiling);
}

#[tokio::test]
async fn a_reply_cut_off_at_the_cap_keeps_its_code_through_the_bridge() {
    let s = setup(Opts {
        token_count: Some(40),
        finish_reason: "length",
        ..Opts::default()
    })
    .await;
    let bridge = LocalReasoningBridge::new(s.manager.clone());
    let error = bridge
        .process_text(
            SHORT_PROMPT,
            &s.model_id,
            &json!({ "requireCompleteOutput": true }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "OUTPUT_TRUNCATED");
    assert_eq!(error.details["modelId"], s.model_id);
    let ipc = error.to_ipc();
    assert_eq!(ipc["success"], false);
    assert_eq!(ipc["code"], "OUTPUT_TRUNCATED");
}

#[tokio::test]
async fn missing_models_and_binaries_are_typed() {
    let s = setup(Opts::default()).await;
    let error = s
        .manager
        .run_inference("nope", "x", &InferenceOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, "MODEL_NOT_FOUND");
    let other = &local_providers()[0].models[1];
    let error = s
        .manager
        .run_inference(&other.id, "x", &InferenceOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, "MODEL_NOT_DOWNLOADED");
}

// --- the bridge

#[tokio::test]
async fn the_bridge_runs_one_request_at_a_time_and_cancels_by_tag() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("llama-server-darwin-arm64"), "").unwrap();
    let models_dir = dir.path().join("models");
    let mock = MockServer::start(|req| match req.path.as_str() {
        "/health" => Reply::json(200, json!({})),
        _ => Reply::json(
            200,
            json!({ "choices": [{ "finish_reason": "stop", "message": { "content": "<think>x</think> slow" } }] }),
        )
        .delayed(Duration::from_millis(400)),
    })
    .await;
    let port = mock.port;
    let server = LlamaServer::new(ServerDeps {
        spawner: Arc::new(FakeSpawner::new()),
        client: Arc::new(ReqwestServerClient::new().unwrap()),
        pick_port: Arc::new(move || Ok(port)),
        binary_dirs: vec![bin],
        pid_file: None,
        timings: ServerTimings::default(),
        runtime: tokio::runtime::Handle::current(),
        spawn_gate: None,
    });
    let manager = Arc::new(ModelManager::new(
        models_dir.clone(),
        server,
        reqwest::Client::new(),
    ));
    let model = first_model();
    std::fs::write(models_dir.join(&model.file_name), vec![0u8; 1_000_001]).unwrap();
    let bridge = Arc::new(LocalReasoningBridge::new(manager));
    assert!(bridge.is_available());

    // Completes, with thinking stripped.
    let text = bridge
        .process_text("hello", &model.id, &json!({}))
        .await
        .unwrap();
    assert_eq!(text, "slow");

    let running = {
        let bridge = bridge.clone();
        let id = model.id.clone();
        tokio::spawn(async move {
            bridge
                .process_text("hello", &id, &json!({ "requestId": "tone-rewrite-abc" }))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(bridge.is_processing());
    let busy = bridge
        .process_text("again", &model.id, &json!({}))
        .await
        .unwrap_err();
    assert_eq!(busy.code, "LOCAL_MODEL_BUSY");
    assert_eq!(busy.to_ipc().get("details"), None);

    bridge.cancel("someone-else");
    bridge.cancel_tagged("other-");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(bridge.is_processing(), "only the tagged caller may cancel");
    bridge.cancel_tagged("tone-rewrite-");
    let error = running.await.unwrap().unwrap_err();
    assert_eq!(error.code, "INFERENCE_FAILED");
    assert!(error.message.contains("cancelled"));
    assert!(!bridge.is_processing());
}

// --- drafter files

#[tokio::test]
async fn the_draft_path_needs_a_declared_and_valid_drafter() {
    let dir = tempfile::tempdir().unwrap();
    let server = LlamaServer::new(ServerDeps {
        spawner: Arc::new(FakeSpawner::new()),
        client: Arc::new(ReqwestServerClient::new().unwrap()),
        pick_port: Arc::new(|| Ok(1)),
        binary_dirs: vec![],
        pid_file: None,
        timings: ServerTimings::default(),
        runtime: tokio::runtime::Handle::current(),
        spawn_gate: None,
    });
    let manager = ModelManager::new(dir.path().to_path_buf(), server, reqwest::Client::new());
    let (qat, _) = manager.find("gemma-4-e2b-it-qat-q4_0").unwrap();
    let (plain, _) = manager.find("gemma-4-e2b-it-q4_k_m").unwrap();
    let draft = dir.path().join(qat.draft_file_name.as_deref().unwrap());

    assert_eq!(manager.draft_path(qat), None);
    std::fs::write(&draft, vec![1u8; 500]).unwrap();
    assert_eq!(manager.draft_path(qat), None);
    std::fs::write(&draft, vec![1u8; 1_000_001]).unwrap();
    assert_eq!(manager.draft_path(qat), Some(draft.clone()));
    assert_eq!(manager.draft_path(plain), None);
    assert_eq!(
        manager
            .server_start_options(qat, None, None)
            .draft_model_path,
        Some(draft.clone())
    );

    // Delete removes the drafter too, and tolerates it being gone.
    let main = manager.model_path(qat);
    std::fs::write(&main, vec![1u8; 1_000_001]).unwrap();
    manager.delete_model(&qat.id).unwrap();
    assert!(!main.exists() && !draft.exists());
    std::fs::write(&main, vec![1u8; 1_000_001]).unwrap();
    manager.delete_model(&qat.id).unwrap();
    assert!(!main.exists());
    assert_eq!(
        manager.delete_model("nope").unwrap_err().code,
        "MODEL_NOT_FOUND"
    );
}

// --- downloads through the manager

fn download_manager(dir: &Path, base_url: String) -> ModelManager {
    let server = LlamaServer::new(ServerDeps {
        spawner: Arc::new(FakeSpawner::new()),
        client: Arc::new(ReqwestServerClient::new().unwrap()),
        pick_port: Arc::new(|| Ok(1)),
        binary_dirs: vec![],
        pid_file: None,
        timings: ServerTimings::default(),
        runtime: tokio::runtime::Handle::current(),
        spawn_gate: None,
    });
    ModelManager::new(
        dir.to_path_buf(),
        server,
        reqwest::Client::builder().no_proxy().build().unwrap(),
    )
    .with_download_source(
        base_url,
        DownloadTimings {
            stall: Duration::from_millis(300),
            backoff_base: Duration::from_millis(5),
            max_backoff: Duration::from_millis(20),
            progress_throttle: Duration::from_millis(1),
        },
    )
}

const SMALL_MODEL: &str = "lfm2.5-230m-q8_0";

#[tokio::test]
async fn downloads_a_model_reporting_progress_and_listing_it() {
    let paths = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    let mock = MockServer::start(move |req| {
        seen.lock().unwrap().push(req.path.clone());
        Reply::bytes(200, vec![5u8; 1_500_000])
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager = download_manager(dir.path(), mock.url(""));
    let progress = Mutex::new(Vec::new());
    let report = |p: f64, d: u64, t: u64| progress.lock().unwrap().push((p, d, t));

    let path = manager.download_model(SMALL_MODEL, &report).await.unwrap();

    let (model, _) = manager.find(SMALL_MODEL).unwrap();
    assert_eq!(path, dir.path().join(&model.file_name));
    assert_eq!(
        paths.lock().unwrap()[0],
        format!("/{}/resolve/main/{}", model.hf_repo, model.file_name)
    );
    assert_eq!(
        progress.lock().unwrap().last(),
        Some(&(100.0, 1_500_000, 1_500_000))
    );
    let listed = manager
        .all_models()
        .into_iter()
        .find(|m| m["id"] == SMALL_MODEL)
        .unwrap();
    assert_eq!(listed["isDownloaded"], true);
    assert_eq!(listed["isDownloading"], false);
    assert_eq!(listed["path"], path.display().to_string());
    assert_eq!(listed["providerId"], "liquidai");
    assert_eq!(listed["fileName"], model.file_name);
    // Already there: no second request.
    manager
        .download_model(SMALL_MODEL, &|_, _, _| {})
        .await
        .unwrap();
    assert_eq!(paths.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn download_failures_are_typed() {
    let mock = MockServer::start(|req| {
        if req.path.contains("Q8_0") {
            Reply::bytes(404, b"nope".to_vec())
        } else {
            Reply::bytes(200, vec![1u8; 10])
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager = download_manager(dir.path(), mock.url(""));
    let error = manager
        .download_model(SMALL_MODEL, &|_, _, _| {})
        .await
        .unwrap_err();
    assert_eq!(error.code, "DOWNLOAD_FAILED");
    assert_eq!(error.message, "Download failed with status 404");
    assert_eq!(error.details["statusCode"], 404);

    let error = manager
        .download_model("lfm2.5-350m-q8_0", &|_, _, _| {})
        .await
        .unwrap_err();
    assert_eq!(
        error.code, "DOWNLOAD_FAILED",
        "the 350M file also matches Q8_0"
    );

    let error = manager
        .download_model("nope", &|_, _, _| {})
        .await
        .unwrap_err();
    assert_eq!(error.code, "MODEL_NOT_FOUND");
}

#[tokio::test]
async fn a_tiny_download_is_rejected_as_corrupted() {
    let mock = MockServer::start(|_| Reply::bytes(200, vec![1u8; 10])).await;
    let dir = tempfile::tempdir().unwrap();
    let manager = download_manager(dir.path(), mock.url(""));
    let error = manager
        .download_model(SMALL_MODEL, &|_, _, _| {})
        .await
        .unwrap_err();
    assert_eq!(error.code, "DOWNLOAD_CORRUPTED");
    assert!(!manager.is_downloaded(SMALL_MODEL));
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[tokio::test]
async fn a_cancelled_download_is_typed_and_a_second_one_is_refused_meanwhile() {
    let mock = MockServer::start(|_| Reply::stall(200, 5_000_000, 64 * 1024)).await;
    let dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(download_manager(dir.path(), mock.url("")));
    let started = Arc::new(tokio::sync::Notify::new());
    let running = {
        let manager = manager.clone();
        let started = started.clone();
        tokio::spawn(async move {
            manager
                .download_model(SMALL_MODEL, &move |_, _, _| started.notify_one())
                .await
        })
    };
    started.notified().await;
    let listed = manager
        .all_models()
        .into_iter()
        .find(|m| m["id"] == SMALL_MODEL)
        .unwrap();
    assert_eq!(listed["isDownloading"], true);
    assert!(listed["downloadedSize"].as_u64().unwrap() > 0);

    let second = manager
        .download_model(SMALL_MODEL, &|_, _, _| {})
        .await
        .unwrap_err();
    assert_eq!(second.code, "DOWNLOAD_IN_PROGRESS");

    assert!(manager.cancel_download(SMALL_MODEL));
    let error = running.await.unwrap().unwrap_err();
    assert_eq!(error.code, "DOWNLOAD_CANCELLED");
    assert_eq!(error.message, "Download cancelled by user");
    assert!(!manager.cancel_download(SMALL_MODEL));
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "temp file removed"
    );
}

#[tokio::test]
async fn a_drafter_downloads_after_the_model_with_monotonic_combined_progress() {
    let mock = MockServer::start(|req| {
        if req.path.contains("mtp-") {
            Reply::bytes(200, vec![2u8; 1_200_000])
        } else {
            Reply::bytes(200, vec![1u8; 2_000_000])
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager = download_manager(dir.path(), mock.url(""));
    let progress = Mutex::new(Vec::<(f64, u64, u64)>::new());
    let report = |p: f64, d: u64, t: u64| progress.lock().unwrap().push((p, d, t));
    let id = "gemma-4-e2b-it-qat-q4_0";
    manager.download_model(id, &report).await.unwrap();

    let (model, _) = manager.find(id).unwrap();
    assert!(manager.draft_path(model).is_some());
    let progress = progress.lock().unwrap();
    let total = model.size_bytes + model.draft_size_bytes;
    assert!(progress.iter().all(|(_, _, t)| *t == total));
    assert!(
        progress.windows(2).all(|w| w[1].1 >= w[0].1),
        "never regresses"
    );
}

#[tokio::test]
async fn a_failed_drafter_keeps_the_model() {
    let mock = MockServer::start(|req| {
        if req.path.contains("mtp-") {
            Reply::bytes(500, vec![])
        } else {
            Reply::bytes(200, vec![1u8; 2_000_000])
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let manager = download_manager(dir.path(), mock.url(""));
    let id = "gemma-4-e2b-it-qat-q4_0";
    manager.download_model(id, &|_, _, _| {}).await.unwrap();
    let (model, _) = manager.find(id).unwrap();
    assert!(manager.is_downloaded(id));
    assert!(manager.draft_path(model).is_none());
}
