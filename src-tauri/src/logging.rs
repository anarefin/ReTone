//! Logging.
//!
//! Level: `--log-level`, then `RETONE_LOG_LEVEL` / `LOG_LEVEL` (process
//! environment or `userData/.env`), then `info`. Lines go to stdout/stderr and,
//! at debug level and below, to `userData/logs/debug-<timestamp>.log`.
//!
//! Rust code logs through `tracing` with the scope as the target
//! (`tracing::info!(target: "tray", ...)`); module-path targets get no scope
//! tag. Renderer entries arrive through the `app_log` command.

use retone_core::log_level::{LogLevel, format_line};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

pub struct Logger {
    level: AtomicU8,
    file: Mutex<Option<File>>,
    file_path: Mutex<Option<PathBuf>>,
    console: bool,
}

fn level_to_u8(level: LogLevel) -> u8 {
    level as u8
}

fn u8_to_level(value: u8) -> LogLevel {
    match value {
        10 => LogLevel::Trace,
        20 => LogLevel::Debug,
        40 => LogLevel::Warn,
        50 => LogLevel::Error,
        60 => LogLevel::Fatal,
        _ => LogLevel::Info,
    }
}

/// What the System Info header records, so a debug log answers "which release
/// on which OS" on its own.
pub struct SystemInfo<'a> {
    pub app_version: &'a str,
    pub user_data: &'a Path,
    pub channel: &'a str,
}

impl Logger {
    pub fn new(level: LogLevel, console: bool) -> Self {
        Self {
            level: AtomicU8::new(level_to_u8(level)),
            file: Mutex::new(None),
            file_path: Mutex::new(None),
            console,
        }
    }

    pub fn level(&self) -> LogLevel {
        u8_to_level(self.level.load(Ordering::Relaxed))
    }

    pub fn set_level(&self, level: LogLevel) {
        self.level.store(level_to_u8(level), Ordering::Relaxed);
    }

    pub fn should_log(&self, level: LogLevel) -> bool {
        level >= self.level()
    }

    pub fn log_path(&self) -> Option<PathBuf> {
        self.file_path.lock().ok().and_then(|p| p.clone())
    }

    /// Opens `<user_data>/logs/debug-<timestamp>.log` and writes the System
    /// Info header. No-op when already open.
    pub fn enable_file_logging(&self, info: &SystemInfo<'_>) -> std::io::Result<PathBuf> {
        if let Some(path) = self.log_path() {
            return Ok(path);
        }
        let logs_dir = info.user_data.join("logs");
        fs::create_dir_all(&logs_dir)?;
        let stamp = chrono::Utc::now()
            .format("%Y-%m-%dT%H-%M-%S-%3fZ")
            .to_string();
        let path = logs_dir.join(format!("debug-{stamp}.log"));
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        *self.file.lock().expect("log file lock") = Some(file);
        *self.file_path.lock().expect("log path lock") = Some(path.clone());

        let log_file = path.display().to_string();
        self.write(
            LogLevel::Debug,
            "Debug logging enabled",
            Some(&serde_json::json!({ "logFile": log_file }).to_string()),
            None,
            None,
        );
        self.write(
            LogLevel::Info,
            "System Info",
            Some(&system_info_meta(info)),
            None,
            None,
        );
        Ok(path)
    }

    pub fn write(
        &self,
        level: LogLevel,
        message: &str,
        meta: Option<&str>,
        scope: Option<&str>,
        source: Option<&str>,
    ) {
        if !self.should_log(level) {
            return;
        }
        let timestamp = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let line = format_line(&timestamp, level, message, meta, scope, source);
        if self.console {
            // The console line drops the timestamp.
            let console_line = line.split_once("] ").map(|(_, rest)| rest).unwrap_or(&line);
            if level >= LogLevel::Error {
                eprintln!("{console_line}");
            } else {
                println!("{console_line}");
            }
        }
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = writeln!(file, "{line}");
        }
    }

    pub fn flush(&self) {
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = file.flush();
        }
    }
}

fn uname() -> (String, String) {
    // SAFETY: utsname is plain old data; uname fills it or fails.
    unsafe {
        let mut info: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut info) != 0 {
            return (String::new(), String::new());
        }
        let field = |raw: &[libc::c_char]| {
            std::ffi::CStr::from_ptr(raw.as_ptr())
                .to_string_lossy()
                .into_owned()
        };
        (field(&info.release), field(&info.version))
    }
}

pub fn system_info_meta(info: &SystemInfo<'_>) -> String {
    let (os_release, os_version) = uname();
    serde_json::to_string_pretty(&serde_json::json!({
        "appVersion": info.app_version,
        "platform": std::env::consts::OS,
        "osRelease": os_release,
        "osVersion": os_version,
        "arch": std::env::consts::ARCH,
        "channel": info.channel,
        "userDataPath": info.user_data.display().to_string(),
        "runtime": "tauri",
    }))
    .unwrap_or_default()
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// The process-wide logger. Before `init` it logs at info to the console.
pub fn logger() -> &'static Logger {
    LOGGER.get_or_init(|| Logger::new(LogLevel::Info, true))
}

/// Installs the global logger and the tracing bridge. Call once, early.
pub fn init(level: LogLevel) {
    let _ = LOGGER.set(Logger::new(level, true));
    logger().set_level(level);
    let _ = tracing_subscriber::registry().with(BridgeLayer).try_init();
}

/// Writes a renderer entry (`app_log`).
pub fn log_entry(
    level: &str,
    message: &str,
    meta: Option<&serde_json::Value>,
    scope: Option<&str>,
    source: Option<&str>,
) {
    let level = LogLevel::parse(level).unwrap_or(LogLevel::Info);
    let meta_text = meta.and_then(|m| match m {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => serde_json::to_string_pretty(other).ok(),
    });
    logger().write(
        level,
        message,
        meta_text.as_deref(),
        scope.filter(|s| !s.is_empty()),
        Some(source.filter(|s| !s.is_empty()).unwrap_or("renderer")),
    );
}

fn tracing_level(level: &Level) -> LogLevel {
    match *level {
        Level::TRACE => LogLevel::Trace,
        Level::DEBUG => LogLevel::Debug,
        Level::INFO => LogLevel::Info,
        Level::WARN => LogLevel::Warn,
        Level::ERROR => LogLevel::Error,
    }
}

/// Scopes Rust code logs under (`tracing::info!(target: "<scope>", ...)`).
pub const SCOPES: &[&str] = &[
    "app",
    "window",
    "dock",
    "tray",
    "menu",
    "environment",
    "i18n",
    "ipc",
    "hotkey",
    "tone-rewrite",
    "clipboard",
    "reasoning",
    "models",
    "llama",
    "sidecar",
];

/// Only ReTone's own events: a known scope, or a module path in one of the
/// workspace crates.
fn is_own_target(target: &str) -> bool {
    SCOPES.contains(&target) || target.starts_with("retone_")
}

struct BridgeLayer;

#[derive(Default)]
struct FieldCollector {
    message: String,
    scope: Option<String>,
    source: Option<String>,
    extra: String,
}

impl Visit for FieldCollector {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "scope" => self.scope = Some(value.to_string()),
            "source" => self.source = Some(value.to_string()),
            name => {
                let _ = write!(self.extra, " {name}={value:?}");
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "scope" => self.scope = Some(format!("{value:?}").trim_matches('"').to_string()),
            "source" => self.source = Some(format!("{value:?}").trim_matches('"').to_string()),
            name => {
                let _ = write!(self.extra, " {name}={value:?}");
            }
        }
    }
}

impl<S: Subscriber> Layer<S> for BridgeLayer {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        is_own_target(metadata.target()) && logger().should_log(tracing_level(metadata.level()))
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut fields = FieldCollector::default();
        event.record(&mut fields);
        let target = metadata.target();
        let scope = fields
            .scope
            .clone()
            .or_else(|| SCOPES.contains(&target).then(|| target.to_string()));
        let meta = fields.extra.trim();
        logger().write(
            tracing_level(metadata.level()),
            &fields.message,
            (!meta.is_empty()).then_some(meta),
            scope.as_deref(),
            fields.source.as_deref(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The System Info header carries
    // the app version and the OS build, so a debug log answers "which release".
    #[test]
    fn debug_log_system_info_header_includes_the_app_version() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(LogLevel::Debug, false);
        let path = logger
            .enable_file_logging(&SystemInfo {
                app_version: "9.9.9-test",
                user_data: dir.path(),
                channel: "development",
            })
            .unwrap();
        logger.flush();

        assert!(path.starts_with(dir.path().join("logs")));
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("debug-") && name.ends_with(".log"),
            "{name}"
        );

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("System Info"));
        assert!(contents.contains("appVersion"));
        assert!(contents.contains("9.9.9-test"));
        assert!(contents.contains("osRelease"));
        assert!(contents.contains("osVersion"));
        assert!(contents.contains("\"arch\""));
    }

    #[test]
    fn lines_below_the_level_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(LogLevel::Debug, false);
        let path = logger
            .enable_file_logging(&SystemInfo {
                app_version: "1",
                user_data: dir.path(),
                channel: "production",
            })
            .unwrap();
        logger.set_level(LogLevel::Warn);
        logger.write(LogLevel::Info, "quiet", None, Some("tray"), None);
        logger.write(
            LogLevel::Error,
            "loud",
            None,
            Some("hotkey"),
            Some("renderer"),
        );
        logger.flush();
        let contents = fs::read_to_string(path).unwrap();
        assert!(!contents.contains("quiet"));
        assert!(contents.contains("[ERROR][hotkey][renderer] loud"));
    }

    #[test]
    fn own_targets_are_scopes_or_workspace_modules() {
        assert!(is_own_target("tray"));
        assert!(is_own_target("tone-rewrite"));
        assert!(is_own_target("retone_lib::windows"));
        assert!(!is_own_target("tao::platform_impl"));
        assert!(!is_own_target("hyper_util::client"));
    }
}
