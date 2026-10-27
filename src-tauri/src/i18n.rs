//! Main-process strings: the renderer's 12
//! `translation.json` files, compiled in.

use retone_core::i18n::{Catalog, normalize_ui_language};
use std::sync::{LazyLock, RwLock};

macro_rules! locale {
    ($lang:literal) => {
        (
            $lang,
            include_str!(concat!("../../src/locales/", $lang, "/translation.json")),
        )
    };
}

const LOCALES: &[(&str, &str)] = &[
    locale!("en"),
    locale!("ar"),
    locale!("bn"),
    locale!("es"),
    locale!("fr"),
    locale!("de"),
    locale!("pt"),
    locale!("it"),
    locale!("ru"),
    locale!("ja"),
    locale!("zh-CN"),
    locale!("zh-TW"),
];

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let mut catalog = Catalog::new();
    for (lang, json) in LOCALES {
        if let Err(error) = catalog.add_json(lang, json) {
            tracing::error!(target: "i18n", "Failed to parse {lang} translations: {error}");
        }
    }
    catalog
});

static LANGUAGE: RwLock<&'static str> = RwLock::new("en");

pub fn current_language() -> &'static str {
    *LANGUAGE.read().expect("language lock")
}

/// Switches the main-process language; returns the normalized tag.
pub fn change_language(language: &str) -> &'static str {
    let normalized = normalize_ui_language(language);
    *LANGUAGE.write().expect("language lock") = normalized;
    normalized
}

pub fn t(key: &str) -> String {
    CATALOG.t(current_language(), key, &[])
}

/// [`t`] with `{{var}}` interpolation.
pub fn t_with(key: &str, vars: &[(&str, &str)]) -> String {
    CATALOG.t(current_language(), key, vars)
}

/// The OS UI language, normalized.
pub fn system_language() -> &'static str {
    normalize_ui_language(&sys_locale::get_locale().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use retone_core::i18n::SUPPORTED_UI_LANGUAGES;

    fn supported_languages() -> &'static [&'static str] {
        SUPPORTED_UI_LANGUAGES
    }

    #[test]
    fn every_supported_language_is_compiled_in_and_parses() {
        for lang in supported_languages() {
            assert!(CATALOG.has_language(lang), "{lang} missing");
        }
        assert_eq!(LOCALES.len(), supported_languages().len());
    }

    #[test]
    fn main_process_keys_resolve_in_every_language() {
        for lang in supported_languages() {
            for key in [
                "tray.quit",
                "tray.tooltip",
                "tray.openSettings",
                "tray.hideSettings",
                "menu.appLabel",
                "menu.quit",
                "window.settingsTitle",
            ] {
                let value = CATALOG.t(lang, key, &[]);
                assert_ne!(value, key, "{lang}: {key} unresolved");
            }
        }
    }

    #[test]
    fn interpolates_dialog_strings() {
        let text = CATALOG.t(
            "en",
            "dialog.openLink.message",
            &[("url", "https://x"), ("error", "boom")],
        );
        assert!(
            text.contains("https://x") && text.contains("boom"),
            "{text}"
        );
    }
}
