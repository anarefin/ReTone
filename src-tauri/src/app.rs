//! App bootstrap and lifecycle: channel and userData,
//! logging, i18n, single instance, windows, Dock, tray, menu, teardown.

use crate::clipboard::InputService;
use crate::env::EnvStore;
use crate::http::HttpClients;
use crate::local::LocalInference;
use crate::secrets::{KeychainBackend, SECRETS, SERVICE, SecretStore};
use crate::{dock, hotkeys, i18n, logging, menu, tone_rewrite, tray, windows};
use retone_core::channel::{Channel, resolve_channel};
use retone_core::log_level::resolve_log_level;
use retone_core::providers::connection_cache::ConnectionCheckCache;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};

/// Preferences the renderer pushes at startup (`sync_startup_preferences`).
#[derive(Debug, Default, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StartupPreferences {
    pub keep_local_model_loaded: bool,
    pub tone_rewrite_whole_field: bool,
}

pub struct AppState {
    pub channel: Channel,
    pub user_data: PathBuf,
    pub env: EnvStore,
    pub startup_preferences: Mutex<StartupPreferences>,
    pub secrets: SecretStore,
    pub http: HttpClients,
    pub connection_checks: ConnectionCheckCache,
    /// The macOS input layer: selection reads, paste, clipboard, Accessibility.
    pub input: InputService,
    /// Local models, the llama-server sidecar and the inference bridge.
    pub local: Arc<LocalInference>,
    shutting_down: AtomicBool,
}

/// Resolved before Tauri starts: what the logger and the profile need.
pub struct Bootstrap {
    pub channel: Channel,
    pub user_data: PathBuf,
    pub env: EnvStore,
}

pub fn user_data_dir(channel: Channel) -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(channel.user_data_dir_name())
}

fn bootstrap() -> Bootstrap {
    let channel = resolve_channel(
        std::env::var("RETONE_CHANNEL").ok().as_deref(),
        std::env::var("VITE_RETONE_CHANNEL").ok().as_deref(),
        tauri::is_dev(),
    );
    let user_data = user_data_dir(channel);
    if let Err(error) = std::fs::create_dir_all(&user_data) {
        eprintln!("Failed to create {}: {error}", user_data.display());
    }
    let env = EnvStore::load(&user_data);

    let args: Vec<String> = std::env::args().collect();
    let level = resolve_log_level(
        &args,
        env.get("RETONE_LOG_LEVEL").as_deref(),
        env.get("LOG_LEVEL").as_deref(),
    );
    logging::init(level);

    // Before anything can fail, so a startup error alert is localized.
    let saved_language = env.ui_language();
    let language = if saved_language.is_empty() {
        i18n::system_language()
    } else {
        saved_language.as_str()
    };
    i18n::change_language(language);

    Bootstrap {
        channel,
        user_data,
        env,
    }
}

fn enable_file_logging(app: &AppHandle, channel: Channel, user_data: &Path) {
    let logger = logging::logger();
    if !logger.level().is_debug_enabled() {
        return;
    }
    let version = app.package_info().version.to_string();
    if let Err(error) = logger.enable_file_logging(&logging::SystemInfo {
        app_version: &version,
        user_data,
        channel: channel.as_str(),
    }) {
        eprintln!("Failed to initialize debug logging: {error}");
    }
}

fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let state = app.state::<AppState>();
    enable_file_logging(app, state.channel, &state.user_data);
    tracing::info!(
        target: "app",
        channel = state.channel.as_str(),
        user_data = %state.user_data.display(),
        "Starting ReTone"
    );

    dock::init(app);
    menu::install(app);

    // A login launch goes to the tray whatever the preference says. setup runs
    // inside applicationDidFinishLaunching, while the launch event is current.
    let launched_hidden = was_launched_at_login();
    let start_minimized = state.env.start_minimized() || launched_hidden;
    tracing::info!(target: "app", enabled = start_minimized, launched_hidden, "Start minimized");
    windows::create_settings_window(app, !start_minimized)?;
    // The picker window is created by the prewarm (5 s after launch when a
    // hotkey is saved) or by the first press.
    tone_rewrite::install(app);
    tray::create(app)?;

    load_secrets(app);

    if !state.env.tone_rewrite_key().is_empty() {
        // Pasting the rewrite needs Accessibility; check early rather than on the
        // first paste. Silent: the settings window shows its own warning.
        state.input.pre_warm_accessibility();
    }

    // Restore the saved hotkey (and prewarm the picker 5 s later), start the
    // native listener when the hotkey needs it, and undo a Globe preference a
    // crashed run left behind.
    hotkeys::init(app);

    // A llama-server a crashed run left behind is killed before any start.
    state.local.reap_stale_sidecars();
    Ok(())
}

/// Whether the session started this process (a login item launch) rather than
/// the user.
fn was_launched_at_login() -> bool {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "macos")]
    let opened_at_login = retone_macos::login_item::launched_as_login_item();
    #[cfg(not(target_os = "macos"))]
    let opened_at_login = false;
    retone_core::login_item::was_launched_hidden(&args, opened_at_login)
}

/// Tells the user why ReTone cannot start, then exits with an error.
fn startup_failed(error: &str) -> ! {
    tracing::error!(target: "app", "Failed to start ReTone: {error}");
    logging::logger().flush();
    crate::dialog::show_error_and_wait(
        &i18n::t("startup.error.title"),
        &i18n::t_with("startup.error.message", &[("error", error)]),
    );
    std::process::exit(1);
}

/// Reads the Keychain off the main thread (a locked Keychain can prompt), then
/// tells every window which keys arrived so a renderer that asked earlier
/// refreshes them.
fn load_secrets(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let loaded = state.secrets.load_all();
        tracing::info!(
            target: "environment",
            loaded = loaded.len(),
            total = SECRETS.len(),
            "Loaded API keys from the Keychain"
        );
        for spec in loaded {
            let _ = app.emit("api-key-updated", spec.store_key);
        }
    });
}

/// Quits from the tray or the app menu.
pub fn quit(app: &AppHandle) {
    app.exit(0);
}

/// Synchronous teardown, once: releases the hotkeys, stops the event tap
/// (handing the Globe action back), then stops llama-server (8 s deadline).
fn teardown(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.shutting_down.swap(true, Ordering::SeqCst) {
        return;
    }
    tracing::info!(target: "app", "Shutting down");
    hotkeys::shutdown(app);
    state.local.shutdown();
    logging::logger().flush();
}

pub fn run() {
    let Bootstrap {
        channel,
        user_data,
        env,
    } = bootstrap();

    let secrets = SecretStore::new(Box::new(KeychainBackend::new(SERVICE)), |name| {
        env.get(name)
    });
    let http =
        HttpClients::new().unwrap_or_else(|error| startup_failed(&format!("HTTP client: {error}")));

    let runtime = tauri::async_runtime::handle().inner().clone();
    // RETONE_CACHE_ROOT may come from userData/.env.
    let cache_root = env.get(retone_llama::cache_dir::CACHE_ROOT_ENV);
    let local = LocalInference::new(
        &user_data,
        cache_root.as_deref(),
        runtime,
        http.follow.clone(),
    )
    .unwrap_or_else(|error| startup_failed(&format!("Local inference: {error}")));

    let builder = tauri::Builder::default()
        // Must be first: a second launch only shows the existing settings window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tracing::info!(target: "app", "Second instance launched; showing settings");
            windows::show_settings(app);
        }))
        .plugin(tauri_plugin_opener::init())
        // Regular-key hotkeys.
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        // Renderer provider calls (CORS-free fetch with streaming bodies).
        .plugin(tauri_plugin_http::init())
        .manage(AppState {
            channel,
            user_data,
            env,
            startup_preferences: Mutex::new(StartupPreferences::default()),
            secrets,
            http,
            connection_checks: ConnectionCheckCache::default(),
            input: InputService::new(),
            local: Arc::new(local),
            shutting_down: AtomicBool::new(false),
        })
        .on_menu_event(|app, event| menu::handle_event(app, event.id().as_ref()))
        .on_window_event(|window, event| {
            let label = window.label();
            match event {
                // The app lives in the tray: closing the settings window only hides it.
                WindowEvent::CloseRequested { api, .. } if label == windows::SETTINGS_LABEL => {
                    api.prevent_close();
                    windows::hide_settings(window.app_handle());
                }
                WindowEvent::Focused(false) if windows::is_picker_label(label) => {
                    tone_rewrite::on_picker_blur(window.app_handle(), label);
                }
                WindowEvent::Destroyed if windows::is_picker_label(label) => {
                    tone_rewrite::on_picker_destroyed(window.app_handle(), label);
                }
                _ => {}
            }
        })
        // A crashed picker renderer is replaced on the next press; a crashed
        // settings renderer is reloaded.
        .on_web_content_process_terminate(|webview| {
            let label = webview.label();
            if windows::is_picker_label(label) {
                tone_rewrite::on_picker_renderer_gone(webview.app_handle(), label);
            } else if label == windows::SETTINGS_LABEL {
                windows::on_settings_renderer_gone(webview.app_handle());
            } else {
                tracing::warn!(target: "window", label, "Web content process terminated");
            }
        })
        .setup(|app| {
            if let Err(error) = setup(app.handle()) {
                startup_failed(&error.to_string());
            }
            Ok(())
        });

    let app = crate::register_commands(builder)
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| startup_failed(&error.to_string()));

    app.run(|app, event| match event {
        // No-op when every window is gone; only an explicit quit (code set) exits.
        RunEvent::ExitRequested { api, code, .. } => {
            if code.is_none() {
                api.prevent_exit();
            } else {
                teardown(app);
            }
        }
        RunEvent::Exit => teardown(app),
        // Clicking the Dock icon opens the settings window.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => windows::show_settings(app),
        _ => {}
    });
}
