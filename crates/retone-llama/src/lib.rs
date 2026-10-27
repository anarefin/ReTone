//! Local inference for ReTone: the bundled `llama-server` sidecar, the model
//! manager and downloads, GGUF metadata, the context policy, the one-at-a-time
//! bridge and the sidecar registry, reaper and pid file. macOS only: Metal, no
//! Vulkan.

pub mod bridge;
pub mod cache_dir;
pub mod client;
pub mod context;
pub mod download;
pub mod download_status;
pub mod gguf;
pub mod models;
pub mod process;
pub mod registry;
pub mod server;
pub mod sidecar;

#[cfg(test)]
pub(crate) mod test_support;

/// The only sidecar ReTone runs.
pub const SIDECAR_NAME: &str = server::SIDECAR_NAME;

#[cfg(test)]
mod tests {
    #[test]
    fn sidecar_name_matches_the_reaper_table() {
        assert_eq!(super::SIDECAR_NAME, "llama");
        assert!(
            super::sidecar::EXPECTED_BINARY_FRAGMENTS
                .iter()
                .any(|(name, fragments)| *name == super::SIDECAR_NAME
                    && fragments.contains(&"llama-server"))
        );
    }
}
