//! The local model registry: `localProviders` of
//! `src/models/modelRegistryData.json`, the renderer's single source of truth,
//! compiled in.

use serde_json::{Map, Value};
use std::sync::OnceLock;

const REGISTRY_JSON: &str = include_str!("../../../src/models/modelRegistryData.json");

const DEFAULT_BASE_URL: &str = "https://huggingface.co";

/// One GGUF model. `raw` is the registry entry as written, which the model
/// list returns as is (the renderer reads its own fields from it).
#[derive(Debug, Clone)]
pub struct LocalModel {
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub hf_repo: String,
    pub size_bytes: u64,
    pub size_mb: u64,
    pub context_length: u64,
    pub draft_hf_repo: Option<String>,
    pub draft_file_name: Option<String>,
    pub draft_size_bytes: u64,
    pub raw: Map<String, Value>,
}

impl LocalModel {
    /// A drafter counts only when the repo, the file and its size are declared.
    pub fn has_drafter(&self) -> bool {
        self.draft_hf_repo.is_some() && self.draft_file_name.is_some() && self.draft_size_bytes > 0
    }
}

#[derive(Debug, Clone)]
pub struct LocalProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<LocalModel>,
}

impl LocalProvider {
    /// `{baseUrl}/{hfRepo}/resolve/main/{fileName}`.
    pub fn download_url(&self, model: &LocalModel) -> String {
        format!(
            "{}/{}/resolve/main/{}",
            self.base_url, model.hf_repo, model.file_name
        )
    }

    pub fn draft_download_url(&self, model: &LocalModel) -> Option<String> {
        Some(format!(
            "{}/{}/resolve/main/{}",
            self.base_url,
            model.draft_hf_repo.as_deref()?,
            model.draft_file_name.as_deref()?
        ))
    }
}

fn text(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn number(map: &Map<String, Value>, key: &str) -> u64 {
    map.get(key).and_then(Value::as_f64).unwrap_or(0.0).max(0.0) as u64
}

fn parse_model(raw: &Value) -> Option<LocalModel> {
    let map = raw.as_object()?;
    Some(LocalModel {
        id: text(map, "id")?,
        name: text(map, "name").unwrap_or_default(),
        file_name: text(map, "fileName")?,
        hf_repo: text(map, "hfRepo").unwrap_or_default(),
        size_bytes: number(map, "sizeBytes"),
        size_mb: number(map, "sizeMb"),
        context_length: number(map, "contextLength"),
        draft_hf_repo: text(map, "draftHfRepo"),
        draft_file_name: text(map, "draftFileName"),
        draft_size_bytes: number(map, "draftSizeBytes"),
        raw: map.clone(),
    })
}

/// Parses the `localProviders` of a registry document.
pub fn parse_local_providers(json: &str) -> Vec<LocalProvider> {
    let Ok(document) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    document
        .get("localProviders")
        .and_then(Value::as_array)
        .map(|providers| {
            providers
                .iter()
                .filter_map(|provider| {
                    let map = provider.as_object()?;
                    Some(LocalProvider {
                        id: text(map, "id")?,
                        name: text(map, "name").unwrap_or_default(),
                        base_url: text(map, "baseUrl")
                            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
                        models: map
                            .get("models")
                            .and_then(Value::as_array)
                            .map(|models| models.iter().filter_map(parse_model).collect())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The bundled registry.
pub fn local_providers() -> &'static [LocalProvider] {
    static PROVIDERS: OnceLock<Vec<LocalProvider>> = OnceLock::new();
    PROVIDERS.get_or_init(|| parse_local_providers(REGISTRY_JSON))
}

/// Finds a model and its provider by id.
pub fn find_model<'a>(
    providers: &'a [LocalProvider],
    model_id: &str,
) -> Option<(&'a LocalModel, &'a LocalProvider)> {
    providers.iter().find_map(|provider| {
        provider
            .models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| (model, provider))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const QAT_ID: &str = "gemma-4-e2b-it-qat-q4_0";
    const NON_QAT_ID: &str = "gemma-4-e2b-it-q4_k_m";

    #[test]
    fn the_bundled_registry_parses() {
        let providers = local_providers();
        assert!(!providers.is_empty());
        let (model, provider) = find_model(providers, "qwen3.5-4b-q4_k_m").unwrap();
        assert_eq!(provider.id, "qwen");
        assert_eq!(model.file_name, "Qwen_Qwen3.5-4B-Q4_K_M.gguf");
        assert_eq!(model.raw["id"], "qwen3.5-4b-q4_k_m");
        assert!(find_model(providers, "nope").is_none());
    }

    #[test]
    fn has_drafter_is_true_only_for_entries_declaring_one() {
        let providers = local_providers();
        assert!(find_model(providers, QAT_ID).unwrap().0.has_drafter());
        assert!(!find_model(providers, NON_QAT_ID).unwrap().0.has_drafter());
    }

    #[test]
    fn download_urls_use_hugging_face_resolve_paths() {
        let (model, provider) = find_model(local_providers(), QAT_ID).unwrap();
        assert_eq!(
            provider.download_url(model),
            format!(
                "{}/{}/resolve/main/{}",
                provider.base_url, model.hf_repo, model.file_name
            )
        );
        assert_eq!(
            provider.draft_download_url(model).unwrap(),
            format!(
                "{}/{}/resolve/main/{}",
                provider.base_url,
                model.draft_hf_repo.as_deref().unwrap(),
                model.draft_file_name.as_deref().unwrap()
            )
        );
    }
}
