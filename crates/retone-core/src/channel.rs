//! Release channel and the per-channel profile directory name.
//!
//! Non-production channels keep their own profile so they can run beside a
//! release (`ReTone-development`, `ReTone-staging`).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Development,
    Staging,
    Production,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Development => "development",
            Channel::Staging => "staging",
            Channel::Production => "production",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "development" => Some(Channel::Development),
            "staging" => Some(Channel::Staging),
            "production" => Some(Channel::Production),
            _ => None,
        }
    }

    /// The userData directory name under `~/Library/Application Support`.
    pub fn user_data_dir_name(self) -> String {
        match self {
            Channel::Production => "ReTone".to_string(),
            other => format!("ReTone-{}", other.as_str()),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `RETONE_CHANNEL` (or `VITE_RETONE_CHANNEL`) wins when valid; otherwise a
/// development build is `development` and anything else is `production`.
pub fn resolve_channel(
    retone_channel: Option<&str>,
    vite_channel: Option<&str>,
    is_dev_build: bool,
) -> Channel {
    let raw = retone_channel
        .filter(|v| !v.trim().is_empty())
        .or(vite_channel)
        .unwrap_or("");
    if let Some(channel) = Channel::parse(raw) {
        return channel;
    }
    if is_dev_build {
        Channel::Development
    } else {
        Channel::Production
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_channel_wins_case_insensitively() {
        assert_eq!(
            resolve_channel(Some(" Staging "), None, true),
            Channel::Staging
        );
        assert_eq!(
            resolve_channel(None, Some("production"), true),
            Channel::Production
        );
    }

    #[test]
    fn invalid_channel_falls_back_to_build_kind() {
        assert_eq!(
            resolve_channel(Some("nightly"), None, true),
            Channel::Development
        );
        assert_eq!(
            resolve_channel(Some("nightly"), None, false),
            Channel::Production
        );
        assert_eq!(resolve_channel(None, None, false), Channel::Production);
    }

    #[test]
    fn profile_directory_names() {
        assert_eq!(Channel::Production.user_data_dir_name(), "ReTone");
        assert_eq!(
            Channel::Development.user_data_dir_name(),
            "ReTone-development"
        );
        assert_eq!(Channel::Staging.user_data_dir_name(), "ReTone-staging");
    }
}
