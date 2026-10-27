import type { SettingsState } from "../stores/settingsStore";

export interface InferenceScopeStoreKeys {
  mode: keyof SettingsState;
  provider: keyof SettingsState;
  model: keyof SettingsState;
  cloudBaseUrl?: keyof SettingsState;
  customApiKey?: keyof SettingsState;
  disableThinking?: keyof SettingsState;
  reasoningEffort?: keyof SettingsState;
}

export interface InferenceScopeDefinition {
  storeKeys: InferenceScopeStoreKeys;
}

export const INFERENCE_SCOPES = {
  // Rewrites a selection in another app in a picked tone (the ReTone hotkey).
  // Defaults to a local model. The Custom endpoint's key is the shared secret
  // kept in the OS secure store.
  toneRewrite: {
    storeKeys: {
      mode: "toneRewriteMode",
      provider: "toneRewriteProvider",
      model: "toneRewriteModel",
      cloudBaseUrl: "toneRewriteCloudBaseUrl",
      customApiKey: "cleanupCustomApiKey",
      disableThinking: "toneRewriteDisableThinking",
      reasoningEffort: "toneRewriteReasoningEffort",
    },
  },
} as const satisfies Record<string, InferenceScopeDefinition>;

export type InferenceScope = keyof typeof INFERENCE_SCOPES;
