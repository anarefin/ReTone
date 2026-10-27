//! ReTone app crate: Tauri setup, windows, tray, Dock, menu and IPC commands.

mod anthropic;
mod app;
mod clipboard;
mod commands;
mod dialog;
mod dock;
mod env;
mod hotkeys;
mod http;
mod i18n;
mod local;
mod logging;
mod menu;
mod secrets;
#[cfg(test)]
mod test_server;
mod tone_rewrite;
mod tray;
mod windows;

pub use app::run;

/// Every IPC command. `command_registry_tests` checks this list against the
/// commands `src/platform/nativeBridge.ts` invokes, so the two cannot drift.
pub(crate) fn register_commands(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    use commands::{
        hotkeys, inference, keys, logging as log_commands, models, permissions, system,
        tone_rewrite,
    };
    builder.invoke_handler(tauri::generate_handler![
        // keys
        keys::get_openai_key,
        keys::save_openai_key,
        keys::get_anthropic_key,
        keys::save_anthropic_key,
        keys::get_gemini_key,
        keys::save_gemini_key,
        keys::get_groq_key,
        keys::save_groq_key,
        keys::get_openrouter_key,
        keys::save_openrouter_key,
        keys::get_cleanup_custom_key,
        keys::save_cleanup_custom_key,
        keys::test_provider_connection,
        // system
        system::get_app_version,
        system::open_external,
        system::write_clipboard,
        system::get_auto_start_enabled,
        system::set_auto_start_enabled,
        system::get_start_minimized,
        system::set_start_minimized,
        system::get_ui_language,
        system::set_ui_language,
        system::sync_startup_preferences,
        // permissions
        permissions::check_accessibility_permission,
        permissions::open_accessibility_settings,
        permissions::open_login_items_settings,
        // hotkeys
        hotkeys::set_hotkey_listening_mode,
        hotkeys::get_hotkey_mode_info,
        // tone rewrite
        tone_rewrite::update_tone_rewrite_hotkey,
        tone_rewrite::get_tone_rewrite_key,
        tone_rewrite::tone_rewrite_get_payload,
        tone_rewrite::tone_rewrite_apply,
        tone_rewrite::tone_rewrite_close,
        tone_rewrite::tone_rewrite_set_height,
        // inference
        inference::process_local_reasoning,
        inference::cancel_local_reasoning,
        inference::check_local_reasoning_available,
        inference::process_anthropic_reasoning,
        // models
        models::model_get_all,
        models::model_get_active_downloads,
        models::model_download,
        models::model_delete,
        models::model_cancel_download,
        models::llama_server_start,
        models::llama_server_status,
        models::llama_gpu_reset,
        models::detect_vulkan_gpu,
        models::get_llama_vulkan_status,
        models::download_llama_vulkan_binary,
        models::cancel_llama_vulkan_download,
        models::delete_llama_vulkan_binary,
        // logging
        log_commands::get_log_level,
        log_commands::app_log,
    ])
}

#[cfg(test)]
mod command_registry_tests {
    use std::collections::BTreeSet;

    const LIB_SOURCE: &str = include_str!("lib.rs");
    const BRIDGE_SOURCE: &str = include_str!("../../src/platform/nativeBridge.ts");

    fn registered() -> BTreeSet<String> {
        let start = LIB_SOURCE.find("generate_handler![").expect("handler list")
            + "generate_handler![".len();
        let end = start + LIB_SOURCE[start..].find("])").expect("handler list end");
        LIB_SOURCE[start..end]
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .map(|l| {
                l.trim_end_matches(',')
                    .rsplit("::")
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    /// Names passed to `call("...")` in the native bridge.
    fn invoked() -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        let mut rest = BRIDGE_SOURCE;
        while let Some(idx) = rest.find("call(\"") {
            rest = &rest[idx + "call(\"".len()..];
            let end = rest.find('"').expect("closing quote");
            let name = &rest[..end];
            // Skips prose such as the `call("…")` in the bridge's header comment.
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                names.insert(name.to_string());
            }
            rest = &rest[end..];
        }
        names
    }

    #[test]
    fn every_command_the_bridge_invokes_is_registered() {
        let registered = registered();
        let invoked = invoked();
        assert!(
            !invoked.is_empty(),
            "native bridge invokes nothing; parser out of date?"
        );
        let missing: Vec<_> = invoked.difference(&registered).collect();
        assert!(
            missing.is_empty(),
            "native bridge invokes unregistered commands: {missing:?}"
        );
    }

    #[test]
    fn every_registered_command_is_reachable_from_the_bridge() {
        let registered = registered();
        let invoked = invoked();
        let unused: Vec<_> = registered.difference(&invoked).collect();
        assert!(
            unused.is_empty(),
            "registered commands the native bridge never invokes: {unused:?}"
        );
    }
}
