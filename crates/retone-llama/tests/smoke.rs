//! Real llama-server smoke run (ignored by default: needs the downloaded
//! binary in `resources/bin` and network for a ~0.8 GB model on first run).
//!
//!   cargo test -p retone-llama --test smoke -- --ignored --nocapture
//!   RETONE_SMOKE_BIN_DIR=target/aarch64-apple-darwin/release/bundle/macos/ReTone.app/Contents/Resources/bin \
//!     cargo test -p retone-llama --test smoke -- --ignored --nocapture
//!
//! Downloads Gemma 3 1B into the real cache (`~/.cache/retone/models`, or
//! `RETONE_CACHE_ROOT`) unless present, sends one completion through the
//! bridge, checks the idle stop, the quit-time shutdown, and that a server a
//! "crashed" app left behind is reaped from its pid file.

use retone_llama::bridge::LocalReasoningBridge;
use retone_llama::cache_dir;
use retone_llama::models::ModelManager;
use retone_llama::server::{LlamaServer, default_deps};
use retone_llama::sidecar::{
    PidFile, ReapGraces, SHUTDOWN_DEADLINE, SidecarRegistry, is_process_alive, reap_stale_sidecars,
    wait_for_exit,
};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const MODEL: &str = "gemma-3-1b-it-q4_k_m";

/// `resources/bin`, or `RETONE_SMOKE_BIN_DIR` (e.g. a built bundle's
/// `ReTone.app/Contents/Resources/bin`, to run the signed, hardened copy).
fn bin_dir() -> std::path::PathBuf {
    match std::env::var_os("RETONE_SMOKE_BIN_DIR") {
        Some(dir) => dir.into(),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/bin"),
    }
}

fn server(user_data: &Path, idle: Duration) -> (LlamaServer, PidFile) {
    let pids = PidFile::new(user_data);
    let mut deps = default_deps(
        vec![bin_dir()],
        Some(pids.clone()),
        tokio::runtime::Handle::current(),
        None,
    )
    .unwrap();
    deps.timings.idle_timeout = idle;
    (LlamaServer::new(deps), pids)
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .unwrap()
}

fn process_args(pid: i32) -> String {
    let out = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn running_pid(pids: &PidFile) -> i32 {
    let entries = pids.read_all();
    assert_eq!(entries.len(), 1, "one pid entry: {entries:?}");
    assert_eq!(entries[0].0, "llama");
    entries[0].1
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the real llama-server; needs resources/bin and a small model"]
async fn real_llama_server_round_trip() {
    let user_data = tempfile::tempdir().unwrap();
    let (server, pids) = server(user_data.path(), Duration::from_secs(4));
    assert!(
        server.is_available(),
        "run `npm run download:llama-server` first"
    );
    let manager = Arc::new(ModelManager::new(
        cache_dir::models_dir(std::env::var(cache_dir::CACHE_ROOT_ENV).ok().as_deref()),
        server.clone(),
        http(),
    ));

    if !manager.is_downloaded(MODEL) {
        println!(
            "Downloading {MODEL} into {}",
            manager.models_dir().display()
        );
        let last = std::sync::Mutex::new(0u64);
        manager
            .download_model(MODEL, &|p, d, t| {
                let mut last = last.lock().unwrap();
                if d - *last > 25_000_000 || d == t {
                    *last = d;
                    println!("  {p:.0}% {d}/{t}");
                }
            })
            .await
            .unwrap();
    }

    // One completion through the bridge.
    let bridge = LocalReasoningBridge::new(manager.clone());
    assert!(bridge.is_available());
    let started = std::time::Instant::now();
    let text = bridge
        .process_text(
            "Say hello in five words.",
            MODEL,
            &json!({ "systemPrompt": "Reply briefly.", "maxTokens": 48, "temperature": 0.2,
                     "requestId": "tone-rewrite-smoke" }),
        )
        .await
        .unwrap();
    println!("Completion in {:?}: {text:?}", started.elapsed());
    assert!(!text.trim().is_empty());

    let status = server.status();
    let mut shown = serde_json::to_value(&status).unwrap();
    shown["apiKey"] = json!("<redacted>");
    println!("Status: {shown}");
    assert!(status.running && status.gpu_accelerated);
    let port = status.port.unwrap();
    let key = status.api_key.clone().expect("a per-launch API key");
    assert_eq!(key.len(), 64);
    assert!((8221..=8240).contains(&port));
    let pid = running_pid(&pids);
    assert!(is_process_alive(pid));
    let args = process_args(pid);
    assert!(!args.contains(&key), "the key is not in argv");
    assert!(args.contains("--no-slots") && args.contains("--metrics"));

    // `/health` stays public; everything else needs the key.
    let base = format!("http://127.0.0.1:{port}");
    let get = |path: &str, auth: Option<&str>| {
        let mut request = http().get(format!("{base}{path}"));
        if let Some(key) = auth {
            request = request.bearer_auth(key);
        }
        async move { request.send().await.unwrap().status().as_u16() }
    };
    assert_eq!(get("/health", None).await, 200);
    assert_eq!(get("/props", None).await, 401);
    assert_eq!(get("/metrics", None).await, 401);
    assert_eq!(get("/slots", None).await, 401);
    assert_eq!(get("/props", Some(&key)).await, 200);
    assert_eq!(get("/metrics", Some(&key)).await, 200);
    assert_eq!(get("/slots", Some(&key)).await, 501, "slots disabled");
    assert_eq!(get("/props", Some("wrong")).await, 401);
    let models = http()
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !models.contains(".gguf"),
        "the public model list hides the path: {models}"
    );

    // The renderer's live preview streams to the port directly, with the key.
    let chat = json!({ "messages": [{ "role": "user", "content": "Say hi." }],
                       "max_tokens": 16, "stream": true });
    let refused = http()
        .post(format!("{base}/v1/chat/completions"))
        .header("Origin", "https://example.com")
        .json(&chat)
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status().as_u16(),
        401,
        "unauthenticated chat refused"
    );
    let streamed = http()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&key)
        .json(&chat)
        .send()
        .await
        .unwrap();
    assert_eq!(streamed.status().as_u16(), 200);
    let body = streamed.text().await.unwrap();
    assert!(
        body.contains("data: ") && body.contains("[DONE]"),
        "SSE: {body}"
    );
    println!("Auth: 401 without the key, 200 + SSE stream with it");

    // Idle stop (4 s here instead of 5 min).
    assert!(
        wait_for_exit(pid, Duration::from_secs(15)).await,
        "idle stop"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!server.status().running);
    assert!(pids.read_all().is_empty(), "pid file cleared on exit");
    println!("Idle stop: pid {pid} gone");

    // Quit: the registry stops the sidecar.
    let endpoint = manager.start_server_for(MODEL).await.unwrap();
    assert_ne!(&*endpoint.api_key, key.as_str(), "a new launch, a new key");
    let pid = running_pid(&pids);
    println!("Restarted on port {}, pid {pid}", endpoint.port);
    let registry = SidecarRegistry::new();
    let stopper = server.clone();
    registry.register("llama", move || {
        let server = stopper.clone();
        async move { server.stop().await }
    });
    assert!(registry.shutdown_all(SHUTDOWN_DEADLINE).await);
    assert!(
        wait_for_exit(pid, Duration::from_secs(2)).await,
        "quit kills it"
    );
    println!("Quit: pid {pid} gone");

    // A crashed app leaves the server and its pid file behind; the next
    // launch reaps it.
    manager.start_server_for(MODEL).await.unwrap();
    let pid = running_pid(&pids);
    std::mem::forget(manager);
    std::mem::forget(bridge);
    std::mem::forget(server);
    assert!(is_process_alive(pid));
    reap_stale_sidecars(&PidFile::new(user_data.path()), ReapGraces::default()).await;
    assert!(!is_process_alive(pid), "stale llama-server reaped");
    assert!(pids.read_all().is_empty());
    println!("Reaped stale pid {pid}");
}
