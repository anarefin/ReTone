import { create } from "zustand";
import i18n, { normalizeUiLanguage } from "../i18n";
import logger from "../utils/logger";
import type { InferenceMode } from "../types/nativeApi";
import { normalizeToneRewriteOptions, type ToneRewriteOption } from "../config/prompts/toneRewrite";
import {
  INFERENCE_SCOPES,
  type InferenceScope,
  type InferenceScopeDefinition,
  type InferenceScopeStoreKeys,
} from "../config/inferenceScopes";
import { findStaleLocalModelKeys } from "../helpers/localModelSelections";
import ReasoningService from "../services/ReasoningService";

// Requires localStorage as well as window: test harnesses import this store
// with partial window stubs.
const isBrowser = typeof window !== "undefined" && typeof localStorage !== "undefined";

// `message` is main's translated reason, when it gave one.
export type HotkeyRegistrationResult = { success: boolean; message?: string };

export type ThemeSetting = "light" | "dark" | "auto";

export interface SettingsState {
  uiLanguage: string;
  setUiLanguage: (language: string) => void;
  theme: ThemeSetting;
  setTheme: (value: ThemeSetting) => void;

  // BYOK keys live in the OS secure store; these hydrate from the main process.
  openaiApiKey: string;
  anthropicApiKey: string;
  geminiApiKey: string;
  groqApiKey: string;
  openrouterApiKey: string;
  // The Custom (OpenAI-compatible) endpoint's key.
  cleanupCustomApiKey: string;
  setOpenaiApiKey: (key: string) => void;
  setAnthropicApiKey: (key: string) => void;
  setGeminiApiKey: (key: string) => void;
  setGroqApiKey: (key: string) => void;
  setOpenrouterApiKey: (key: string) => void;
  setCleanupCustomApiKey: (key: string) => void;

  toneRewriteKey: string;
  setToneRewriteKey: (key: string) => Promise<HotkeyRegistrationResult>;
  toneRewriteMode: InferenceMode;
  toneRewriteProvider: string;
  toneRewriteModel: string;
  toneRewriteCloudBaseUrl: string;
  toneRewriteDisableThinking: boolean;
  toneRewriteReasoningEffort: string;
  toneRewriteOptions: ToneRewriteOption[];
  setToneRewriteOptions: (options: ToneRewriteOption[]) => void;
  toneRewriteLivePreview: boolean;
  setToneRewriteLivePreview: (value: boolean) => void;
  toneRewriteWholeField: boolean;
  setToneRewriteWholeField: (value: boolean) => void;

  keepLocalModelLoaded: boolean;
  setKeepLocalModelLoaded: (value: boolean) => void;
}

function readString(key: string, fallback: string): string {
  if (!isBrowser) return fallback;
  return localStorage.getItem(key) ?? fallback;
}

function readBoolean(key: string, fallback: boolean): boolean {
  if (!isBrowser) return fallback;
  const stored = localStorage.getItem(key);
  if (stored === null) return fallback;
  if (fallback === true) return stored !== "false";
  return stored === "true";
}

function readStringArray(key: string): string[] {
  if (!isBrowser) return [];
  try {
    const parsed = JSON.parse(localStorage.getItem(key) ?? "[]");
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

// Settings → ReTone's option list; before it existed, only which built-in
// tones were enabled was saved (toneRewriteEnabledTones).
function readToneRewriteOptions(): ToneRewriteOption[] {
  if (!isBrowser) return normalizeToneRewriteOptions(null);
  const stored = localStorage.getItem("toneRewriteOptions");
  if (stored !== null) {
    try {
      return normalizeToneRewriteOptions(JSON.parse(stored));
    } catch {
      // Unreadable: rebuild from the legacy setting below.
    }
  }
  return normalizeToneRewriteOptions(null, readStringArray("toneRewriteEnabledTones"));
}

function readToneRewriteMode(): InferenceMode {
  return readString("toneRewriteMode", "local") === "providers" ? "providers" : "local";
}

function readTheme(): ThemeSetting {
  const value = readString("theme", "auto");
  return value === "light" || value === "dark" ? value : "auto";
}

const BOOLEAN_SETTINGS = new Set([
  "keepLocalModelLoaded",
  "toneRewriteDisableThinking",
  "toneRewriteLivePreview",
  "toneRewriteWholeField",
]);

function createStringSetter(key: string) {
  return (value: string) => {
    if (isBrowser) localStorage.setItem(key, value);
    useSettingsStore.setState({ [key]: value });
  };
}

function createBooleanSetter(key: string) {
  return (value: boolean) => {
    if (isBrowser) localStorage.setItem(key, String(value));
    useSettingsStore.setState({ [key]: value });
  };
}

/** Writes a string setting whose key is computed rather than known up front. */
export function setStringSetting(key: keyof SettingsState, value: string): void {
  createStringSetter(key)(value);
}

// Store key → the native bridge saver that writes it to the OS secure store.
const SECRET_IPC_SAVERS = {
  openaiApiKey: "saveOpenAIKey",
  anthropicApiKey: "saveAnthropicKey",
  geminiApiKey: "saveGeminiKey",
  groqApiKey: "saveGroqKey",
  openrouterApiKey: "saveOpenrouterKey",
  cleanupCustomApiKey: "saveCleanupCustomKey",
} as const;

type SecretStoreKey = keyof typeof SECRET_IPC_SAVERS;

const SECRET_STORE_KEYS = Object.keys(SECRET_IPC_SAVERS) as SecretStoreKey[];

const secretGetterFor = (storeKey: SecretStoreKey) =>
  SECRET_IPC_SAVERS[storeKey].replace(/^save/, "get") as
    | "getOpenAIKey"
    | "getAnthropicKey"
    | "getGeminiKey"
    | "getGroqKey"
    | "getOpenrouterKey"
    | "getCleanupCustomKey";

function invalidateApiKeyCaches() {
  ReasoningService.clearApiKeyCache();
}

const secretSaveTimers: Partial<Record<SecretStoreKey, ReturnType<typeof setTimeout>>> = {};

function debouncedSaveSecret(storeKey: SecretStoreKey, key: string) {
  if (!isBrowser) return;
  const timer = secretSaveTimers[storeKey];
  if (timer) clearTimeout(timer);
  secretSaveTimers[storeKey] = setTimeout(() => {
    const save = window.retoneAPI?.[SECRET_IPC_SAVERS[storeKey]] as
      ((k: string) => Promise<unknown>) | undefined;
    save?.(key)?.catch((err) => {
      logger.warn(
        "Failed to persist secret",
        { storeKey, error: (err as Error).message },
        "settings"
      );
    });
  }, 250);
}

function createSecretSetter(storeKey: SecretStoreKey) {
  return (key: string) => {
    useSettingsStore.setState({ [storeKey]: key });
    debouncedSaveSecret(storeKey, key);
    invalidateApiKeyCaches();
  };
}

// The hotkey must register with the main process before it is persisted; a
// failed registration keeps the previous key.
async function registerToneRewriteHotkey(hotkey: string): Promise<HotkeyRegistrationResult> {
  const register = isBrowser ? window.retoneAPI?.updateToneRewriteHotkey : undefined;
  if (!register) {
    createStringSetter("toneRewriteKey")(hotkey);
    return { success: true };
  }
  try {
    const result = await register(hotkey);
    if (!result?.success) {
      logger.warn(
        "Failed to update tone rewrite hotkey",
        { hotkey, message: result?.message },
        "settings"
      );
      return { success: false, message: result?.message };
    }
    createStringSetter("toneRewriteKey")(hotkey);
    return { success: true };
  } catch (error) {
    logger.warn(
      "Failed to update tone rewrite hotkey",
      { hotkey, error: error instanceof Error ? error.message : String(error) },
      "settings"
    );
    return { success: false };
  }
}

export const useSettingsStore = create<SettingsState>()((set) => ({
  uiLanguage: normalizeUiLanguage(
    isBrowser ? localStorage.getItem("uiLanguage") || i18n.language : null
  ),
  setUiLanguage: (language: string) => {
    const normalized = normalizeUiLanguage(language);
    if (isBrowser) localStorage.setItem("uiLanguage", normalized);
    set({ uiLanguage: normalized });
    void i18n.changeLanguage(normalized);
    window.retoneAPI?.setUiLanguage?.(normalized).catch((err) => {
      logger.warn(
        "Failed to sync UI language to main process",
        { error: (err as Error).message },
        "settings"
      );
    });
  },
  theme: readTheme(),
  setTheme: (value: ThemeSetting) => {
    if (isBrowser) localStorage.setItem("theme", value);
    set({ theme: value });
  },

  openaiApiKey: "",
  anthropicApiKey: "",
  geminiApiKey: "",
  groqApiKey: "",
  openrouterApiKey: "",
  cleanupCustomApiKey: "",
  setOpenaiApiKey: createSecretSetter("openaiApiKey"),
  setAnthropicApiKey: createSecretSetter("anthropicApiKey"),
  setGeminiApiKey: createSecretSetter("geminiApiKey"),
  setGroqApiKey: createSecretSetter("groqApiKey"),
  setOpenrouterApiKey: createSecretSetter("openrouterApiKey"),
  setCleanupCustomApiKey: createSecretSetter("cleanupCustomApiKey"),

  toneRewriteKey: readString("toneRewriteKey", ""),
  setToneRewriteKey: registerToneRewriteHotkey,
  toneRewriteMode: readToneRewriteMode(),
  toneRewriteProvider: readString("toneRewriteProvider", "qwen"),
  toneRewriteModel: readString("toneRewriteModel", "qwen3.5-4b-q4_k_m"),
  toneRewriteCloudBaseUrl: readString("toneRewriteCloudBaseUrl", ""),
  toneRewriteDisableThinking: readBoolean("toneRewriteDisableThinking", true),
  toneRewriteReasoningEffort: readString("toneRewriteReasoningEffort", ""),
  toneRewriteOptions: readToneRewriteOptions(),
  setToneRewriteOptions: (options: ToneRewriteOption[]) => {
    const normalized = normalizeToneRewriteOptions(options);
    if (isBrowser) {
      localStorage.setItem("toneRewriteOptions", JSON.stringify(normalized));
      localStorage.removeItem("toneRewriteEnabledTones");
    }
    set({ toneRewriteOptions: normalized });
  },
  toneRewriteLivePreview: readBoolean("toneRewriteLivePreview", true),
  setToneRewriteLivePreview: createBooleanSetter("toneRewriteLivePreview"),
  toneRewriteWholeField: readBoolean("toneRewriteWholeField", false),
  setToneRewriteWholeField: createBooleanSetter("toneRewriteWholeField"),

  keepLocalModelLoaded: readBoolean("keepLocalModelLoaded", false),
  setKeepLocalModelLoaded: createBooleanSetter("keepLocalModelLoaded"),
}));

export function getSettings(): SettingsState {
  return useSettingsStore.getState();
}

export interface ResolvedLLMConfig {
  scope: InferenceScope;
  mode: InferenceMode;
  provider: string;
  model: string;
  cloudBaseUrl?: string;
  customApiKey?: string;
  disableThinking: boolean;
  reasoningEffort?: string;
}

export const selectResolvedLLMConfig = (
  state: SettingsState,
  scope: InferenceScope
): ResolvedLLMConfig => {
  const def: InferenceScopeDefinition = INFERENCE_SCOPES[scope];
  const read = (field: keyof InferenceScopeStoreKeys): string | undefined => {
    const key = def.storeKeys[field];
    if (!key) return undefined;
    return (state[key] as string | undefined) || undefined;
  };
  const disableThinkingKey = def.storeKeys.disableThinking;
  return {
    scope,
    mode: state[def.storeKeys.mode] as InferenceMode,
    provider: read("provider") || "",
    model: read("model") || "",
    cloudBaseUrl: read("cloudBaseUrl"),
    customApiKey: read("customApiKey"),
    disableThinking: disableThinkingKey ? (state[disableThinkingKey] as boolean) : true,
    reasoningEffort: read("reasoningEffort"),
  };
};

// Secrets go through their own setters so they reach the OS secure store
// rather than localStorage.
export function setResolvedLLMConfig(
  scope: InferenceScope,
  patch: Partial<Omit<ResolvedLLMConfig, "scope">>
): void {
  const def: InferenceScopeDefinition = INFERENCE_SCOPES[scope];
  const updates: Partial<SettingsState> = {};
  for (const [field, value] of Object.entries(patch)) {
    if (value === undefined) continue;
    const storeKey = def.storeKeys[field as keyof InferenceScopeStoreKeys];
    if (!storeKey) continue;
    if ((SECRET_STORE_KEYS as string[]).includes(storeKey)) {
      createSecretSetter(storeKey as SecretStoreKey)(value as string);
      continue;
    }
    if (isBrowser) {
      localStorage.setItem(
        storeKey as string,
        typeof value === "boolean" ? String(value) : (value as string)
      );
    }
    (updates as Record<string, unknown>)[storeKey as string] = value;
  }
  if (Object.keys(updates).length > 0) useSettingsStore.setState(updates);
}

/**
 * Drops any local model selection the model cache no longer backs — a scope left
 * pointing at a deleted model fails at inference time with an error the user has
 * no way to act on. Cleared rather than repointed so the picker asks again.
 */
export function clearMissingLocalModelSelections(isInstalled: (modelId: string) => boolean): void {
  const settings = useSettingsStore.getState() as unknown as Record<string, unknown>;
  const staleKeys: string[] = findStaleLocalModelKeys(
    Object.values(INFERENCE_SCOPES),
    settings,
    isInstalled
  );
  for (const key of staleKeys) {
    setStringSetting(key as keyof SettingsState, "");
  }
}

// The picker window's whole-field setting and the shared llama-server's
// keep-loaded preference live in the main process.
function syncStartupPreferences(state: SettingsState) {
  window.retoneAPI
    ?.syncStartupPreferences?.({
      keepLocalModelLoaded: state.keepLocalModelLoaded,
      toneRewriteWholeField: state.toneRewriteWholeField,
    })
    .catch((err) => {
      logger.warn(
        "Failed to sync startup preferences",
        { error: (err as Error).message },
        "settings"
      );
    });
}

let hasInitialized = false;

export async function initializeSettings(): Promise<void> {
  if (hasInitialized || !isBrowser) return;
  hasInitialized = true;

  // A key saved in another window: re-read it from the secure store.
  window.retoneAPI?.onApiKeyUpdated?.(async (storeKey: string) => {
    if (!(SECRET_STORE_KEYS as string[]).includes(storeKey)) return;
    const getter = window.retoneAPI?.[secretGetterFor(storeKey as SecretStoreKey)];
    try {
      const key = await getter?.();
      useSettingsStore.setState({ [storeKey]: key || "" });
      invalidateApiKeyCaches();
    } catch {
      logger.warn("Failed to refresh API key", { storeKey }, "settings");
    }
  });

  syncStartupPreferences(useSettingsStore.getState());
  useSettingsStore.subscribe((state, previous) => {
    if (
      state.keepLocalModelLoaded !== previous.keepLocalModelLoaded ||
      state.toneRewriteWholeField !== previous.toneRewriteWholeField
    ) {
      syncStartupPreferences(state);
    }
  });

  if (window.retoneAPI) {
    try {
      const keys = await Promise.all(
        SECRET_STORE_KEYS.map((storeKey) => window.retoneAPI[secretGetterFor(storeKey)]?.())
      );
      useSettingsStore.setState(
        Object.fromEntries(SECRET_STORE_KEYS.map((storeKey, i) => [storeKey, keys[i] || ""]))
      );
    } catch (err) {
      logger.warn(
        "Failed to hydrate secrets from main process",
        { error: (err as Error).message },
        "settings"
      );
    }

    // The main process holds the registered hotkey.
    try {
      const envKey = await window.retoneAPI.getToneRewriteKey?.();
      const state = useSettingsStore.getState();
      if (envKey !== undefined && envKey !== state.toneRewriteKey) {
        createStringSetter("toneRewriteKey")(envKey || "");
      }
    } catch (err) {
      logger.warn(
        "Failed to sync tone rewrite hotkey on startup",
        { error: (err as Error).message },
        "settings"
      );
    }

    try {
      const envLanguage = await window.retoneAPI.getUiLanguage?.();
      const state = useSettingsStore.getState();
      const resolved = normalizeUiLanguage(envLanguage || state.uiLanguage);
      if (resolved !== state.uiLanguage) {
        localStorage.setItem("uiLanguage", resolved);
        useSettingsStore.setState({ uiLanguage: resolved });
      }
      await i18n.changeLanguage(resolved);
    } catch (err) {
      logger.warn(
        "Failed to sync UI language on startup",
        { error: (err as Error).message },
        "settings"
      );
    }
  }

  // Settings written in another window (the picker reads what Settings writes).
  window.addEventListener("storage", (event) => {
    const { key, newValue } = event;
    if (!key || newValue === null) return;
    const state = useSettingsStore.getState() as unknown as Record<string, unknown>;
    if (!(key in state) || typeof state[key] === "function") return;
    if ((SECRET_STORE_KEYS as string[]).includes(key)) return;

    let value: unknown = newValue;
    if (BOOLEAN_SETTINGS.has(key)) {
      value = newValue === "true";
    } else if (key === "toneRewriteOptions") {
      try {
        value = normalizeToneRewriteOptions(JSON.parse(newValue));
      } catch {
        value = normalizeToneRewriteOptions(null);
      }
    }
    useSettingsStore.setState({ [key]: value });
    if (key === "uiLanguage" && typeof value === "string") {
      void i18n.changeLanguage(value);
    }
  });
}
