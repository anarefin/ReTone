//! BYOK API keys. Each key is a
//! macOS Keychain generic password under service [`SERVICE`], account = the
//! env name. Values are loaded into memory once at startup; getters read
//! memory, saves write memory then the Keychain.
//!
//! A key set in the process environment (or the userData
//! `.env`) is used until the user saves or clears one in Settings.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, RwLock};

/// Keychain service: the app id.
pub const SERVICE: &str = "com.arefin.retone";

/// A stored secret: its env name and the renderer's settings-store key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretSpec {
    pub env: &'static str,
    pub store_key: &'static str,
}

/// The five BYOK provider keys plus the Custom
/// endpoint's key.
pub const SECRETS: [SecretSpec; 6] = [
    SecretSpec {
        env: "OPENAI_API_KEY",
        store_key: "openaiApiKey",
    },
    SecretSpec {
        env: "ANTHROPIC_API_KEY",
        store_key: "anthropicApiKey",
    },
    SecretSpec {
        env: "GEMINI_API_KEY",
        store_key: "geminiApiKey",
    },
    SecretSpec {
        env: "GROQ_API_KEY",
        store_key: "groqApiKey",
    },
    SecretSpec {
        env: "OPENROUTER_API_KEY",
        store_key: "openrouterApiKey",
    },
    SecretSpec {
        env: "CUSTOM_CLEANUP_API_KEY",
        store_key: "cleanupCustomApiKey",
    },
];

pub fn spec_for_env(env: &str) -> Option<SecretSpec> {
    SECRETS.iter().copied().find(|s| s.env == env)
}

/// Where secrets persist. The Keychain in the app; a map in tests, so tests
/// never touch the real Keychain.
pub trait SecretBackend: Send + Sync {
    fn read(&self, name: &str) -> Result<Option<String>, String>;
    fn write(&self, name: &str, value: &str) -> Result<(), String>;
    /// Deleting a missing entry is not an error.
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// The macOS Keychain through `keyring` (apple-native).
pub struct KeychainBackend {
    service: String,
}

impl KeychainBackend {
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, name).map_err(|e| e.to_string())
    }
}

impl SecretBackend for KeychainBackend {
    fn read(&self, name: &str) -> Result<Option<String>, String> {
        match self.entry(name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn write(&self, name: &str, value: &str) -> Result<(), String> {
        self.entry(name)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub struct SecretStore {
    backend: Box<dyn SecretBackend>,
    values: RwLock<HashMap<&'static str, String>>,
    /// Keys saved since launch: the startup load must not overwrite them.
    saved: Mutex<HashSet<&'static str>>,
}

/// What a save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    /// The value differs from the one held before (peers need a refresh).
    pub changed: bool,
    /// Persisting failed; the value still applies for this session.
    pub persist_error: Option<String>,
}

impl SecretStore {
    /// Seeds each key from `env` (process environment / `.env`).
    pub fn new(backend: Box<dyn SecretBackend>, env: impl Fn(&str) -> Option<String>) -> Self {
        let values = SECRETS
            .iter()
            .filter_map(|s| env(s.env).filter(|v| !v.is_empty()).map(|v| (s.env, v)))
            .collect();
        Self {
            backend,
            values: RwLock::new(values),
            saved: Mutex::new(HashSet::new()),
        }
    }

    /// Reads every stored key into memory. Returns the specs whose value was
    /// loaded. A read failure is logged and leaves that key as it was (the
    /// user re-enters it).
    pub fn load_all(&self) -> Vec<SecretSpec> {
        let mut loaded = Vec::new();
        for spec in SECRETS {
            match self.backend.read(spec.env) {
                Ok(Some(value)) if !value.is_empty() => {
                    // Checked under the values lock so a concurrent save wins.
                    let saved = self.saved.lock().expect("secrets saved lock");
                    if saved.contains(spec.env) {
                        continue;
                    }
                    self.values
                        .write()
                        .expect("secrets lock")
                        .insert(spec.env, value);
                    loaded.push(spec);
                }
                Ok(_) => {}
                Err(error) => tracing::error!(
                    target: "environment",
                    key = spec.env,
                    %error,
                    "Failed to read secret; user must re-enter"
                ),
            }
        }
        loaded
    }

    /// The key, or "" when unset.
    pub fn get(&self, env: &str) -> String {
        self.values
            .read()
            .expect("secrets lock")
            .get(env)
            .cloned()
            .unwrap_or_default()
    }

    /// Sets (or, when empty, clears) a key: memory first, then the backend.
    pub fn save(&self, env: &str, value: &str) -> Result<SaveOutcome, String> {
        let spec = spec_for_env(env).ok_or_else(|| format!("Unknown secret {env}"))?;
        let changed = {
            let mut saved = self.saved.lock().expect("secrets saved lock");
            saved.insert(spec.env);
            let mut values = self.values.write().expect("secrets lock");
            let previous = values.get(spec.env).cloned().unwrap_or_default();
            if value.is_empty() {
                values.remove(spec.env);
            } else {
                values.insert(spec.env, value.to_string());
            }
            previous != value
        };
        let persisted = if value.is_empty() {
            self.backend.delete(spec.env)
        } else {
            self.backend.write(spec.env, value)
        };
        Ok(SaveOutcome {
            changed,
            persist_error: persisted.err(),
        })
    }
}

#[cfg(test)]
pub mod fake {
    use super::SecretBackend;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// An in-memory backend; `fail` makes every call error.
    #[derive(Default)]
    pub struct FakeBackend {
        pub entries: Mutex<HashMap<String, String>>,
        pub fail: bool,
    }

    impl SecretBackend for std::sync::Arc<FakeBackend> {
        fn read(&self, name: &str) -> Result<Option<String>, String> {
            if self.fail {
                return Err("keychain locked".into());
            }
            Ok(self.entries.lock().unwrap().get(name).cloned())
        }
        fn write(&self, name: &str, value: &str) -> Result<(), String> {
            if self.fail {
                return Err("keychain locked".into());
            }
            self.entries
                .lock()
                .unwrap()
                .insert(name.to_string(), value.to_string());
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<(), String> {
            if self.fail {
                return Err("keychain locked".into());
            }
            self.entries.lock().unwrap().remove(name);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBackend;
    use super::*;
    use std::sync::Arc;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn store_with(backend: &Arc<FakeBackend>) -> SecretStore {
        SecretStore::new(Box::new(Arc::clone(backend)), no_env)
    }

    #[test]
    fn the_secret_list_matches_the_renderer_store_keys() {
        let envs: Vec<_> = SECRETS.iter().map(|s| s.env).collect();
        assert_eq!(
            envs,
            [
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "GEMINI_API_KEY",
                "GROQ_API_KEY",
                "OPENROUTER_API_KEY",
                "CUSTOM_CLEANUP_API_KEY"
            ]
        );
        let store = include_str!("../../src/stores/settingsStore.ts");
        for spec in SECRETS {
            assert!(
                store.contains(spec.store_key),
                "{} not in settingsStore",
                spec.store_key
            );
        }
    }

    #[test]
    fn saves_persist_and_reload() {
        let backend = Arc::new(FakeBackend::default());
        let store = store_with(&backend);
        let outcome = store.save("OPENAI_API_KEY", "sk-one").unwrap();
        assert_eq!(
            outcome,
            SaveOutcome {
                changed: true,
                persist_error: None
            }
        );
        assert_eq!(store.get("OPENAI_API_KEY"), "sk-one");
        assert_eq!(backend.entries.lock().unwrap()["OPENAI_API_KEY"], "sk-one");

        let reloaded = store_with(&backend);
        assert_eq!(reloaded.get("OPENAI_API_KEY"), "");
        let loaded = reloaded.load_all();
        assert_eq!(loaded, [SECRETS[0]]);
        assert_eq!(reloaded.get("OPENAI_API_KEY"), "sk-one");
    }

    #[test]
    fn saving_the_same_value_is_not_a_change() {
        let backend = Arc::new(FakeBackend::default());
        let store = store_with(&backend);
        store.save("GROQ_API_KEY", "gsk").unwrap();
        assert!(!store.save("GROQ_API_KEY", "gsk").unwrap().changed);
    }

    #[test]
    fn an_empty_save_clears_memory_and_the_keychain() {
        let backend = Arc::new(FakeBackend::default());
        let store = store_with(&backend);
        store.save("ANTHROPIC_API_KEY", "sk-ant").unwrap();
        let outcome = store.save("ANTHROPIC_API_KEY", "").unwrap();
        assert!(outcome.changed);
        assert_eq!(store.get("ANTHROPIC_API_KEY"), "");
        assert!(backend.entries.lock().unwrap().is_empty());
        // Clearing an absent key is fine.
        assert!(
            store
                .save("ANTHROPIC_API_KEY", "")
                .unwrap()
                .persist_error
                .is_none()
        );
    }

    #[test]
    fn environment_keys_apply_until_the_user_saves_one() {
        let backend = Arc::new(FakeBackend::default());
        let env = |name: &str| (name == "GEMINI_API_KEY").then(|| "AIza-env".to_string());
        let store = SecretStore::new(Box::new(Arc::clone(&backend)), env);
        assert_eq!(store.get("GEMINI_API_KEY"), "AIza-env");
        // A stored key wins over the environment.
        backend
            .entries
            .lock()
            .unwrap()
            .insert("GEMINI_API_KEY".into(), "AIza-stored".into());
        store.load_all();
        assert_eq!(store.get("GEMINI_API_KEY"), "AIza-stored");
        store.save("GEMINI_API_KEY", "").unwrap();
        assert_eq!(store.get("GEMINI_API_KEY"), "");
    }

    #[test]
    fn a_save_before_the_startup_load_is_not_overwritten() {
        let backend = Arc::new(FakeBackend::default());
        backend
            .entries
            .lock()
            .unwrap()
            .insert("OPENROUTER_API_KEY".into(), "old".into());
        let store = store_with(&backend);
        // The user saves while the Keychain load is still pending.
        store.save("OPENROUTER_API_KEY", "new").unwrap();
        assert!(store.load_all().is_empty());
        assert_eq!(store.get("OPENROUTER_API_KEY"), "new");
    }

    #[test]
    fn keychain_failures_keep_the_session_value_and_report_the_error() {
        let backend = Arc::new(FakeBackend {
            fail: true,
            ..Default::default()
        });
        let store = store_with(&backend);
        assert!(store.load_all().is_empty());
        let outcome = store.save("CUSTOM_CLEANUP_API_KEY", "k").unwrap();
        assert_eq!(outcome.persist_error.as_deref(), Some("keychain locked"));
        assert_eq!(store.get("CUSTOM_CLEANUP_API_KEY"), "k");
    }

    #[test]
    fn unknown_names_are_refused() {
        let store = store_with(&Arc::new(FakeBackend::default()));
        assert!(store.save("PATH", "x").is_err());
    }
}
