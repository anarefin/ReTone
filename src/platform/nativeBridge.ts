// The renderer's native bridge: installs `window.retoneAPI` on top of Tauri.
// Every method maps 1:1 onto a Rust command named after the method's channel
// in snake_case (`get-ui-language` → `get_ui_language`), or onto a Tauri event
// with the channel name.
//
// Keep `call("…")` literal: a Rust test (src-tauri/src/lib.rs) reads the names
// out of this file and checks each one is registered, and
// test/platform/nativeBridge.test.js checks every method in
// src/types/nativeApi.ts is implemented here.
import { invoke } from "@tauri-apps/api/core";
import type { EventCallback, UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

type RetoneAPI = Window["retoneAPI"];

function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args);
}

// Listens on this window (events emitted to it or to every window) and returns
// a synchronous cleanup.
function subscribe<T>(event: string, handler: (payload: T) => void): () => void {
  if (typeof handler !== "function") return () => {};
  let disposed = false;
  let unlisten: UnlistenFn | null = null;
  const onEvent: EventCallback<T> = (e) => {
    if (!disposed) handler(e.payload);
  };
  getCurrentWebviewWindow()
    .listen<T>(event, onEvent)
    .then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    })
    .catch(() => {});
  return () => {
    disposed = true;
    unlisten?.();
    unlisten = null;
  };
}

export const nativeBridge: RetoneAPI = {
  // BYOK keys
  getOpenAIKey: () => call("get_openai_key"),
  saveOpenAIKey: (key) => call("save_openai_key", { key }),
  getAnthropicKey: () => call("get_anthropic_key"),
  saveAnthropicKey: (key) => call("save_anthropic_key", { key }),
  getGeminiKey: () => call("get_gemini_key"),
  saveGeminiKey: (key) => call("save_gemini_key", { key }),
  getGroqKey: () => call("get_groq_key"),
  saveGroqKey: (key) => call("save_groq_key", { key }),
  getOpenrouterKey: () => call("get_openrouter_key"),
  saveOpenrouterKey: (key) => call("save_openrouter_key", { key }),
  getCleanupCustomKey: () => call("get_cleanup_custom_key"),
  saveCleanupCustomKey: (key) => call("save_cleanup_custom_key", { key }),
  onApiKeyUpdated: (callback) =>
    subscribe<string>("api-key-updated", (storeKey) => callback(storeKey)),
  testProviderConnection: (config) => call("test_provider_connection", { config }),

  // System
  getPlatform: () => "darwin",
  getAppVersion: () => call("get_app_version"),
  openExternal: (url) => call("open_external", { url }),
  writeClipboard: (text) => call("write_clipboard", { text }),
  getAutoStartEnabled: () => call("get_auto_start_enabled"),
  setAutoStartEnabled: (enabled) => call("set_auto_start_enabled", { enabled }),
  getStartMinimized: () => call("get_start_minimized"),
  setStartMinimized: (enabled) => call("set_start_minimized", { enabled }),
  getUiLanguage: () => call("get_ui_language"),
  setUiLanguage: (language) => call("set_ui_language", { language }),
  syncStartupPreferences: (prefs) => call("sync_startup_preferences", { prefs }),

  // macOS permissions
  checkAccessibilityPermission: (silent) =>
    call("check_accessibility_permission", { silent: silent === true }),
  openAccessibilitySettings: () => call("open_accessibility_settings"),
  openLoginItemsSettings: () => call("open_login_items_settings"),

  // Hotkeys
  setHotkeyListeningMode: (enabled) => call("set_hotkey_listening_mode", { enabled }),
  getHotkeyModeInfo: () => call("get_hotkey_mode_info"),
  onGlobeKeyPressed: (callback) => subscribe<void>("globe-key-pressed", () => callback?.()),
  onGlobeKeyReleased: (callback) => subscribe<void>("globe-key-released", () => callback?.()),

  // ReTone
  updateToneRewriteHotkey: (hotkey) => call("update_tone_rewrite_hotkey", { hotkey }),
  getToneRewriteKey: () => call("get_tone_rewrite_key"),
  toneRewriteGetPayload: () => call("tone_rewrite_get_payload"),
  toneRewriteApply: (sessionId, text) => call("tone_rewrite_apply", { sessionId, text }),
  toneRewriteClose: () => call("tone_rewrite_close"),
  toneRewriteSetHeight: (height) => call("tone_rewrite_set_height", { height }),
  onToneRewriteShow: (callback) =>
    subscribe("tone-rewrite-show", (payload) => callback(payload as never)),
  onToneRewriteReset: (callback) => subscribe<void>("tone-rewrite-reset", () => callback()),

  // Model calls that run in the main process
  processLocalReasoning: (text, modelId, agentName, config) =>
    call("process_local_reasoning", { text, modelId, agentName, config }),
  cancelLocalReasoning: (requestId) => call("cancel_local_reasoning", { requestId }),
  checkLocalReasoningAvailable: () => call("check_local_reasoning_available"),
  processAnthropicReasoning: (text, modelId, agentName, config) =>
    call("process_anthropic_reasoning", { text, modelId, agentName, config }),

  // Local models
  modelGetAll: () => call("model_get_all"),
  modelGetActiveDownloads: () => call("model_get_active_downloads"),
  modelDownload: (modelId) => call("model_download", { modelId }),
  modelDelete: (modelId) => call("model_delete", { modelId }),
  modelCancelDownload: (modelId) => call("model_cancel_download", { modelId }),
  // The callback takes (event, data); event is always null.
  onModelDownloadProgress: (callback) =>
    subscribe("model-download-progress", (data) => callback(null, data as never)),
  llamaServerStart: (modelId) => call("llama_server_start", { modelId }),
  llamaServerStatus: () => call("llama_server_status"),
  llamaGpuReset: () => call("llama_gpu_reset"),
  detectVulkanGpu: () => call("detect_vulkan_gpu"),
  getLlamaVulkanStatus: () => call("get_llama_vulkan_status"),
  downloadLlamaVulkanBinary: () => call("download_llama_vulkan_binary"),
  cancelLlamaVulkanDownload: () => call("cancel_llama_vulkan_download"),
  deleteLlamaVulkanBinary: () => call("delete_llama_vulkan_binary"),
  onLlamaVulkanDownloadProgress: (callback) =>
    subscribe("llama-vulkan-download-progress", (data) => callback(data as never)),

  // Logging
  getLogLevel: () => call("get_log_level"),
  log: (entry) => call("app_log", { entry }),
};

export function installNativeBridge(target: Window = window): void {
  target.retoneAPI = nativeBridge;
}

installNativeBridge();
