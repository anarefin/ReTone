//! Main-process translations: the same `translation.json` files the renderer
//! uses, looked up by dotted key with `{{var}}` interpolation and an English
//! fallback.

use serde_json::Value;
use std::collections::HashMap;

pub const SUPPORTED_UI_LANGUAGES: &[&str] = &[
    "en", "ar", "bn", "es", "fr", "de", "pt", "it", "ru", "ja", "zh-CN", "zh-TW",
];

pub const FALLBACK_LANGUAGE: &str = "en";

/// Maps an OS or user language tag onto a supported UI language.
pub fn normalize_ui_language(language: &str) -> &'static str {
    let normalized = language.trim().replace('_', "-");

    if let Some(full) = SUPPORTED_UI_LANGUAGES
        .iter()
        .find(|lang| lang.eq_ignore_ascii_case(&normalized))
    {
        return full;
    }

    // Chinese is the only UI language that is not the primary subtag: OS tags
    // are zh-Hans-CN, zh-Hant-TW, zh-HK or bare zh.
    let lower = normalized.to_ascii_lowercase();
    if lower == "zh" || lower.starts_with("zh-") {
        let traditional = lower
            .split('-')
            .any(|part| matches!(part, "hant" | "tw" | "hk" | "mo"));
        return if traditional { "zh-TW" } else { "zh-CN" };
    }

    let base = lower.split('-').next().unwrap_or("");
    SUPPORTED_UI_LANGUAGES
        .iter()
        .find(|lang| **lang == base)
        .copied()
        .unwrap_or(FALLBACK_LANGUAGE)
}

/// Replaces `{{ name }}` placeholders. A placeholder with no value is left as
/// is, as i18next does (`skipOnVariables`).
pub fn interpolate(template: &str, params: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = after[..end].trim();
        match params.iter().find(|(k, _)| *k == name) {
            Some((_, value)) => out.push_str(value),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Translation catalogs keyed by language.
#[derive(Default)]
pub struct Catalog {
    languages: HashMap<String, Value>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_json(&mut self, language: &str, json: &str) -> Result<(), serde_json::Error> {
        self.languages
            .insert(language.to_string(), serde_json::from_str(json)?);
        Ok(())
    }

    pub fn has_language(&self, language: &str) -> bool {
        self.languages.contains_key(language)
    }

    fn lookup(&self, language: &str, key: &str) -> Option<&str> {
        let mut node = self.languages.get(language)?;
        for part in key.split('.') {
            node = node.get(part)?;
        }
        // returnEmptyString: false — an empty translation falls back.
        node.as_str().filter(|s| !s.is_empty())
    }

    /// Looks `key` up in `language`, then English; a missing key returns itself.
    pub fn t(&self, language: &str, key: &str, params: &[(&str, &str)]) -> String {
        let template = self
            .lookup(language, key)
            .or_else(|| self.lookup(FALLBACK_LANGUAGE, key))
            .unwrap_or(key);
        interpolate(template, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_exact_supported_tags_including_underscore_zh() {
        assert_eq!(normalize_ui_language("zh-CN"), "zh-CN");
        assert_eq!(normalize_ui_language("zh_TW"), "zh-TW");
        assert_eq!(normalize_ui_language("en-US"), "en");
        assert_eq!(normalize_ui_language("pt-BR"), "pt");
    }

    #[test]
    fn maps_chinese_script_and_region_tags() {
        assert_eq!(normalize_ui_language("zh-Hans"), "zh-CN");
        assert_eq!(normalize_ui_language("zh-Hans-CN"), "zh-CN");
        assert_eq!(normalize_ui_language("zh"), "zh-CN");
        assert_eq!(normalize_ui_language("zh-Hant"), "zh-TW");
        assert_eq!(normalize_ui_language("zh-Hant-TW"), "zh-TW");
        assert_eq!(normalize_ui_language("zh_Hant_TW"), "zh-TW");
        assert_eq!(normalize_ui_language("zh-HK"), "zh-TW");
    }

    #[test]
    fn falls_back_to_english_for_unknown_languages() {
        assert_eq!(normalize_ui_language("ko-KR"), "en");
        assert_eq!(normalize_ui_language(""), "en");
    }

    #[test]
    fn interpolates_and_keeps_unknown_placeholders() {
        assert_eq!(
            interpolate("Open {{url}}: {{ error }}", &[("url", "u"), ("error", "e")]),
            "Open u: e"
        );
        assert_eq!(interpolate("Hi {{name}}", &[]), "Hi {{name}}");
        assert_eq!(interpolate("broken {{x", &[("x", "1")]), "broken {{x");
    }

    #[test]
    fn looks_up_with_english_fallback_and_key_passthrough() {
        let mut catalog = Catalog::new();
        catalog
            .add_json(
                "en",
                r#"{"tray":{"quit":"Quit ReTone","tooltip":"Tip"},"x":{"msg":"Error {{code}}"}}"#,
            )
            .unwrap();
        catalog
            .add_json("bn", r#"{"tray":{"quit":"প্রস্থান","tooltip":""}}"#)
            .unwrap();
        assert_eq!(catalog.t("bn", "tray.quit", &[]), "প্রস্থান");
        assert_eq!(catalog.t("bn", "tray.tooltip", &[]), "Tip");
        assert_eq!(catalog.t("bn", "x.msg", &[("code", "7")]), "Error 7");
        assert_eq!(catalog.t("bn", "missing.key", &[]), "missing.key");
        assert_eq!(catalog.t("bn", "tray", &[]), "tray");
    }
}
