//! Log levels and how the level is chosen:
//! `--log-level <l>` / `--log-level=<l>`, then `RETONE_LOG_LEVEL`, then
//! `LOG_LEVEL`, then `info`.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace = 10,
    Debug = 20,
    Info = 30,
    Warn = 40,
    Error = 50,
    Fatal = 60,
}

impl LogLevel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            "fatal" => Some(Self::Fatal),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
        }
    }

    /// File logging is on at debug level and below.
    pub fn is_debug_enabled(self) -> bool {
        self <= Self::Debug
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The raw `--log-level` argument, if any.
pub fn arg_log_level<S: AsRef<str>>(args: &[S]) -> Option<String> {
    let mut iter = args.iter().map(AsRef::as_ref);
    while let Some(arg) = iter.next() {
        if arg == "--log-level" {
            return iter.next().map(str::to_string);
        }
        if let Some(value) = arg.strip_prefix("--log-level=") {
            return Some(value.to_string());
        }
    }
    None
}

pub fn resolve_log_level<S: AsRef<str>>(
    args: &[S],
    retone_log_level: Option<&str>,
    log_level: Option<&str>,
) -> LogLevel {
    arg_log_level(args)
        .as_deref()
        .and_then(LogLevel::parse)
        .or_else(|| retone_log_level.and_then(LogLevel::parse))
        .or_else(|| log_level.and_then(LogLevel::parse))
        .unwrap_or(LogLevel::Info)
}

/// One log line: `[ts] [LEVEL][scope][source] message meta`.
pub fn format_line(
    timestamp: &str,
    level: LogLevel,
    message: &str,
    meta: Option<&str>,
    scope: Option<&str>,
    source: Option<&str>,
) -> String {
    let scope_tag = scope.map(|s| format!("[{s}]")).unwrap_or_default();
    let source_tag = source.map(|s| format!("[{s}]")).unwrap_or_default();
    let level_tag = level.as_str().to_ascii_uppercase();
    match meta.filter(|m| !m.is_empty()) {
        Some(meta) => {
            format!("[{timestamp}] [{level_tag}]{scope_tag}{source_tag} {message} {meta}")
        }
        None => format!("[{timestamp}] [{level_tag}]{scope_tag}{source_tag} {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_beats_environment() {
        let args = ["retone", "--log-level", "debug"];
        assert_eq!(
            resolve_log_level(&args, Some("warn"), None),
            LogLevel::Debug
        );
        let args = ["retone", "--log-level=TRACE"];
        assert_eq!(
            resolve_log_level(&args, Some("warn"), None),
            LogLevel::Trace
        );
    }

    #[test]
    fn invalid_values_fall_through() {
        let args = ["retone", "--log-level=loud"];
        assert_eq!(
            resolve_log_level(&args, Some("nope"), Some("error")),
            LogLevel::Error
        );
        let none: [&str; 0] = [];
        assert_eq!(resolve_log_level(&none, None, None), LogLevel::Info);
        assert_eq!(
            resolve_log_level(&["--log-level"], None, None),
            LogLevel::Info
        );
    }

    #[test]
    fn debug_and_below_enable_file_logging() {
        assert!(LogLevel::Trace.is_debug_enabled());
        assert!(LogLevel::Debug.is_debug_enabled());
        assert!(!LogLevel::Info.is_debug_enabled());
    }

    #[test]
    fn formats_lines_with_scope_and_meta() {
        assert_eq!(
            format_line(
                "t",
                LogLevel::Warn,
                "msg",
                Some("{\"a\":1}"),
                Some("hotkey"),
                Some("renderer")
            ),
            "[t] [WARN][hotkey][renderer] msg {\"a\":1}"
        );
        assert_eq!(
            format_line("t", LogLevel::Info, "msg", None, None, None),
            "[t] [INFO] msg"
        );
    }
}
