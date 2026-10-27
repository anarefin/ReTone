//! `userData/.env`: the non-secret preferences main persists (hotkey, start
//! minimized, UI language, llama backend). Secrets never go here; they live in
//! the Keychain.
//!
//! Updates rewrite only the managed key's line, so hand-added lines such as
//! `RETONE_LOG_LEVEL=debug` survive.

pub const ENV_FILE_HEADER: &str = "# ReTone Environment Variables";

/// Keys main persists to `userData/.env`.
pub const PERSISTED_KEYS: &[&str] = &[
    "LLAMA_GPU_BACKEND",
    "TONE_REWRITE_KEY",
    "START_MINIMIZED",
    "UI_LANGUAGE",
];

/// Parses dotenv content into ordered `(key, value)` pairs. Later duplicates win
/// when collected into a map.
pub fn parse(content: &str) -> Vec<(String, String)> {
    content.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let (key, raw_value) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
    {
        return None;
    }
    Some((key.to_string(), parse_value(raw_value.trim())))
}

fn parse_value(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 {
        let quote = bytes[0];
        if (quote == b'"' || quote == b'\'' || quote == b'`')
            && let Some(end) = raw[1..].rfind(quote as char)
        {
            let inner = &raw[1..1 + end];
            if quote == b'"' {
                return inner
                    .replace("\\n", "\n")
                    .replace("\\r", "\r")
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\");
            }
            return inner.to_string();
        }
    }
    // Unquoted: an inline comment starts at " #".
    match raw.find(" #") {
        Some(idx) => raw[..idx].trim_end().to_string(),
        None => raw.to_string(),
    }
}

fn format_value(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '`' | '\\'));
    if !needs_quotes {
        return value.to_string();
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{escaped}\"")
}

fn line_key(line: &str) -> Option<String> {
    parse_line(line).map(|(k, _)| k)
}

/// Sets (`Some`) or removes (`None`) `key`, keeping every other line verbatim.
/// A new file starts with the ReTone header.
pub fn upsert(content: &str, key: &str, value: Option<&str>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut written = false;
    for line in content.lines() {
        if line_key(line).as_deref() == Some(key) {
            if !written && let Some(v) = value {
                out.push(format!("{key}={}", format_value(v)));
                written = true;
            }
            continue;
        }
        out.push(line.to_string());
    }
    if !written && let Some(v) = value {
        if out.is_empty() {
            out.push(ENV_FILE_HEADER.to_string());
        }
        out.push(format!("{key}={}", format_value(v)));
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_exports_quotes_and_inline_comments() {
        let parsed = parse(
            "# header\nexport A=1\nB=\"two words\"\nC='x#y'\nD=plain # note\n\nbad line\nE=\"line\\nbreak\"\n",
        );
        assert_eq!(
            parsed,
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two words".into()),
                ("C".into(), "x#y".into()),
                ("D".into(), "plain".into()),
                ("E".into(), "line\nbreak".into()),
            ]
        );
    }

    #[test]
    fn upsert_creates_a_file_with_the_header() {
        assert_eq!(
            upsert("", "UI_LANGUAGE", Some("bn")),
            "# ReTone Environment Variables\nUI_LANGUAGE=bn\n"
        );
    }

    #[test]
    fn upsert_replaces_in_place_and_keeps_other_lines() {
        let before = "# ReTone Environment Variables\nRETONE_LOG_LEVEL=debug\nTONE_REWRITE_KEY=F8\nUI_LANGUAGE=en\n";
        let after = upsert(before, "TONE_REWRITE_KEY", Some("Control+Shift+Space"));
        assert_eq!(
            after,
            "# ReTone Environment Variables\nRETONE_LOG_LEVEL=debug\nTONE_REWRITE_KEY=Control+Shift+Space\nUI_LANGUAGE=en\n"
        );
    }

    #[test]
    fn upsert_removes_every_definition_of_a_key() {
        let after = upsert(
            "A=1\nSTART_MINIMIZED=true\nexport START_MINIMIZED=false\n",
            "START_MINIMIZED",
            None,
        );
        assert_eq!(after, "A=1\n");
    }

    #[test]
    fn values_that_need_quoting_round_trip() {
        for value in ["a b", "#", "say \"hi\"", "back\\slash", ""] {
            let text = upsert("", "K", Some(value));
            assert_eq!(
                parse(&text),
                vec![("K".to_string(), value.to_string())],
                "{value:?}"
            );
        }
    }
}
