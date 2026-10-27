//! Endpoint URL helpers for the connection check. Mirrors `normalizeBaseUrl`
//! and `getModelListBaseCandidates` in `src/config/constants.ts` and
//! `isPrivateHost` in `src/utils/urlUtils.ts`; keep them in sync, or a passing
//! check commits a config every real request rejects.

use regex::Regex;
use std::sync::LazyLock;

/// Splits `path?query#hash` into its three parts (query and hash keep their
/// leading `?`/`#`).
fn split_url_decorators(value: &str) -> (&str, &str, &str) {
    let (rest, hash) = match value.find('#') {
        Some(i) => (&value[..i], &value[i..]),
        None => (value, ""),
    };
    match rest.find('?') {
        Some(i) => (&rest[..i], &rest[i..], hash),
        None => (rest, "", hash),
    }
}

static SUFFIXES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(?i)/v1/chat/completions$", "/v1"),
        (r"(?i)/chat/completions$", ""),
        (r"(?i)/v1/responses$", "/v1"),
        (r"(?i)/responses$", ""),
        (r"(?i)/v1/models$", "/v1"),
        (r"(?i)/models$", ""),
        (r"(?i)/v1/audio/transcriptions$", "/v1"),
        (r"(?i)/audio/transcriptions$", ""),
        (r"(?i)/v1/audio/translations$", "/v1"),
        (r"(?i)/audio/translations$", ""),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("suffix pattern"), r))
    .collect()
});

static NATIVE_API: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(.+?)/api/v[01]$").expect("native api pattern"));

/// Strips endpoint suffixes (`/chat/completions`, `/models`, …) and trailing
/// slashes, keeping any query or hash.
pub fn normalize_base_url(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let (raw_path, query, hash) = split_url_decorators(trimmed);
    let mut normalized = raw_path.to_string();
    for (pattern, replacement) in SUFFIXES.iter() {
        if pattern.is_match(&normalized) {
            normalized = pattern
                .replace(&normalized, *replacement)
                .trim_end_matches('/')
                .to_string();
        }
    }
    format!("{}{query}{hash}", normalized.trim_end_matches('/'))
}

/// Self-hosted servers (LM Studio, Ollama, vLLM) serve the API under `/v1` even
/// when users enter the bare origin, and LM Studio's native REST base
/// (`/api/v1` or `/api/v0`) has its OpenAI-compatible sibling at `/v1`.
pub fn model_list_base_candidates(base: &str) -> Vec<String> {
    let normalized = normalize_base_url(base);
    if normalized.is_empty() {
        return Vec::new();
    }
    let (path, query, hash) = split_url_decorators(&normalized);
    if let Some(caps) = NATIVE_API.captures(path) {
        let sibling = format!("{}/v1{query}{hash}", &caps[1]);
        return vec![normalized.clone(), sibling];
    }
    if path.ends_with("/v1") {
        return vec![normalized.clone()];
    }
    let sibling = format!("{path}/v1{query}{hash}");
    vec![normalized.clone(), sibling]
}

/// `<candidate>/models` for every candidate base.
pub fn model_endpoints(base: &str) -> Vec<String> {
    model_list_base_candidates(base)
        .iter()
        .map(|candidate| {
            let (path, query, hash) = split_url_decorators(candidate);
            format!("{path}/models{query}{hash}")
        })
        .collect()
}

fn parse_ipv4_literal(hostname: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = hostname.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (i, part) in parts.iter().enumerate() {
        let valid = !part.is_empty()
            && part.len() <= 3
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0'));
        if !valid {
            return None;
        }
        let value: u16 = part.parse().ok()?;
        if value > 255 {
            return None;
        }
        octets[i] = value as u8;
    }
    Some(octets)
}

/// Loopback, RFC 1918, CGNAT (Tailscale), link-local, `.local` and `.ts.net`
/// hosts: the only places plain HTTP is allowed.
pub fn is_private_host(hostname: &str) -> bool {
    let lower = hostname.to_ascii_lowercase();
    let h = lower.trim_start_matches('[').trim_end_matches(']');

    if h == "localhost" || h == "0.0.0.0" || h == "::1" {
        return true;
    }
    if let Some([a, b, ..]) = parse_ipv4_literal(h) {
        if a == 127 || a == 10 {
            return true;
        }
        if (a == 192 && b == 168)
            || (a == 172 && (16..=31).contains(&b))
            || (a == 100 && (64..=127).contains(&b))
            || (a == 169 && b == 254)
        {
            return true;
        }
    }
    let is_ipv6 = h.contains(':');
    if is_ipv6 && (h.starts_with("fe80") || h.starts_with("fc") || h.starts_with("fd")) {
        return true;
    }
    h.ends_with(".local") || h.ends_with(".ts.net")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_endpoint_suffixes_and_keeps_query() {
        assert_eq!(
            normalize_base_url(" https://api.example.com/v1/chat/completions "),
            "https://api.example.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://proxy.example.com/v1/"),
            "https://proxy.example.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://x.example.com/openai/models?api-version=1"),
            "https://x.example.com/openai?api-version=1"
        );
        assert_eq!(normalize_base_url("   "), "");
    }

    #[test]
    fn candidates_add_the_v1_sibling() {
        assert_eq!(
            model_endpoints("http://localhost:1234"),
            [
                "http://localhost:1234/models",
                "http://localhost:1234/v1/models"
            ]
        );
        assert_eq!(
            model_endpoints("http://localhost:1234/api/v0"),
            [
                "http://localhost:1234/api/v0/models",
                "http://localhost:1234/v1/models"
            ]
        );
        assert_eq!(
            model_endpoints("https://localhost:11434/v1"),
            ["https://localhost:11434/v1/models"]
        );
    }

    #[test]
    fn private_hosts() {
        for host in [
            "localhost",
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.10",
            "172.16.0.1",
            "100.101.102.103",
            "169.254.1.1",
            "[::1]",
            "fd00::1",
            "printer.local",
            "ollama.ts.net",
        ] {
            assert!(is_private_host(host), "{host}");
        }
        for host in [
            "10.example.com",
            "myserver.example.com",
            "8.8.8.8",
            "172.32.0.1",
            "010.0.0.1",
        ] {
            assert!(!is_private_host(host), "{host}");
        }
    }
}
