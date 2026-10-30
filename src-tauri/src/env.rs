//! `userData/.env`: non-secret preferences. Values in the file win over the process environment,
//! so settings saved in the app always apply.

use retone_core::env_file;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

pub const TONE_REWRITE_KEY: &str = "TONE_REWRITE_KEY";
pub const START_MINIMIZED: &str = "START_MINIMIZED";
pub const UI_LANGUAGE: &str = "UI_LANGUAGE";
pub const APP_ICON: &str = "APP_ICON";

pub struct EnvStore {
    path: PathBuf,
    values: RwLock<HashMap<String, String>>,
    write_lock: Mutex<()>,
}

impl EnvStore {
    /// Loads `<user_data>/.env` (missing is fine).
    pub fn load(user_data: &Path) -> Self {
        let path = user_data.join(".env");
        let values = fs::read_to_string(&path)
            .map(|content| env_file::parse(&content).into_iter().collect())
            .unwrap_or_default();
        Self {
            path,
            values: RwLock::new(values),
            write_lock: Mutex::new(()),
        }
    }

    /// The file's value, else the process environment's; empty counts as unset.
    pub fn get(&self, key: &str) -> Option<String> {
        self.values
            .read()
            .ok()
            .and_then(|v| v.get(key).cloned())
            .or_else(|| std::env::var(key).ok())
            .filter(|v| !v.is_empty())
    }

    /// Sets (`Some`) or clears (`None`/empty) a key and rewrites its line in
    /// the file atomically, keeping every other line.
    pub fn set(&self, key: &str, value: Option<&str>) -> io::Result<()> {
        let value = value.filter(|v| !v.is_empty());
        let _guard = self.write_lock.lock().expect("env write lock");
        {
            let mut values = self.values.write().expect("env values lock");
            match value {
                Some(v) => values.insert(key.to_string(), v.to_string()),
                None => values.remove(key),
            };
        }
        let current = fs::read_to_string(&self.path).unwrap_or_default();
        let next = env_file::upsert(&current, key, value);
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, next)?;
        fs::rename(&tmp, &self.path)
    }

    pub fn tone_rewrite_key(&self) -> String {
        self.get(TONE_REWRITE_KEY).unwrap_or_default()
    }

    pub fn start_minimized(&self) -> bool {
        self.get(START_MINIMIZED).as_deref() == Some("true")
    }

    /// The app icon picked in Settings; the bundle icon when none is saved.
    pub fn app_icon(&self) -> retone_core::app_icon::AppIcon {
        retone_core::app_icon::AppIcon::from_saved(self.get(APP_ICON).as_deref())
    }

    /// The saved UI language, normalized, or "" when none is saved.
    pub fn ui_language(&self) -> String {
        self.get(UI_LANGUAGE)
            .map(|l| retone_core::i18n::normalize_ui_language(&l).to_string())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_reloads_preferences_keeping_hand_added_lines() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "RETONE_LOG_LEVEL=debug\n").unwrap();

        let store = EnvStore::load(dir.path());
        store.set(START_MINIMIZED, Some("true")).unwrap();
        store.set(UI_LANGUAGE, Some("bn")).unwrap();
        store.set(APP_ICON, Some("aurora")).unwrap();
        store
            .set(TONE_REWRITE_KEY, Some("Control+Shift+Space"))
            .unwrap();
        store.set(TONE_REWRITE_KEY, None).unwrap();

        let reloaded = EnvStore::load(dir.path());
        assert!(reloaded.start_minimized());
        assert_eq!(reloaded.ui_language(), "bn");
        assert_eq!(reloaded.app_icon(), retone_core::app_icon::AppIcon::Aurora);
        assert_eq!(reloaded.tone_rewrite_key(), "");
        assert_eq!(reloaded.get("RETONE_LOG_LEVEL").as_deref(), Some("debug"));
        let text = fs::read_to_string(dir.path().join(".env")).unwrap();
        assert!(text.starts_with("RETONE_LOG_LEVEL=debug\n"), "{text}");
        assert!(!dir.path().join(".env.tmp").exists());
    }

    #[test]
    fn unsaved_language_is_empty_and_saved_ones_normalize() {
        let dir = tempfile::tempdir().unwrap();
        let store = EnvStore::load(dir.path());
        assert_eq!(store.ui_language(), "");
        store.set(UI_LANGUAGE, Some("zh_Hant_TW")).unwrap();
        assert_eq!(store.ui_language(), "zh-TW");
    }
}
