//! IPC commands, one file per area. Each command is
//! the renderer channel name in snake_case (`get-ui-language` →
//! `get_ui_language`); `src/platform/nativeBridge.ts` maps
//! `window.retoneAPI` onto them.

pub mod hotkeys;
pub mod inference;
pub mod keys;
pub mod logging;
pub mod models;
pub mod permissions;
pub mod system;
pub mod tone_rewrite;
