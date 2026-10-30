import type { ModelDefinition } from "../models/ModelRegistry";
import type { ReasoningConfig } from "../services/BaseReasoningService";
import type { TechnicalErrorDetailsData } from "../components/ui/useToast";

export type InferenceMode = "providers" | "local";

export interface IpcErrorFields {
  error: string;
  code?: string;
  messageKey?: string;
  messageParams?: Record<string, string | number>;
  settingsTarget?: string;
  technicalDetails?: TechnicalErrorDetailsData;
  status?: number;
  surface?: "transcription" | "llm";
}

export type ToneRewritePickerPayload =
  | {
      state: "ready";
      sessionId: string;
      text: string;
      characterCount: number;
      selectedAll: boolean;
    }
  | { state: "error"; code: string; maxCharacters?: number };

export interface LocalModelDownloadStatus {
  modelType: "llm";
  modelId: string;
  phase: "downloading" | "installing";
  progress: number;
  downloadedBytes: number;
  totalBytes: number;
  sequence: number;
}

export type GpuBackend = "vulkan" | "cpu" | "metal" | null;

/** The app icons offered in Settings; `mono` is the bundle icon. */
export type AppIconId = "mono" | "light" | "aurora" | "ring";

export interface LlamaServerStatus {
  available: boolean;
  running: boolean;
  port: number | null;
  /** The running server's per-launch API key (Bearer token). */
  apiKey: string | null;
  modelPath: string | null;
  modelName: string | null;
  backend: GpuBackend;
  gpuAccelerated: boolean;
}

export interface VulkanGpuResult {
  available: boolean;
  deviceName?: string;
  reason?: string;
  error?: string;
}

export interface LlamaVulkanStatus {
  supported: boolean;
  downloaded: boolean;
  downloading?: boolean;
  error?: string;
}

export interface LlamaVulkanDownloadProgress {
  downloaded: number;
  total: number;
  percentage: number;
}

export interface LocalLLMModelStatus extends ModelDefinition {
  providerId?: string;
  providerName?: string;
  isDownloaded: boolean;
  isDownloading: boolean;
  downloadProgress: number;
  downloadedSize: number;
  totalSize: number;
  path: string | null;
}

export type LocalLLMDownloadProgressEvent =
  | {
      type?: "progress";
      modelId: string;
      progress: number;
      downloadedSize: number;
      totalSize: number;
      sequence?: number;
    }
  | {
      type: "complete";
      modelId: string;
      progress: 100;
      downloadedSize?: number;
      totalSize?: number;
      sequence?: number;
    }
  | {
      type: "error";
      modelId: string;
      error: string;
      code?: string;
      details?: unknown;
      sequence?: number;
    };

export interface ProviderConnectionCheck {
  scope: "reasoning";
  provider: string;
  apiKey?: string;
  baseUrl?: string;
  model?: string;
}

export interface ProviderConnectionResult {
  success: boolean;
  error?: string;
  errorCode?: string;
  status?: number;
  /** The provider gave no verdict (busy, down, unreachable); the setup may be used. */
  unverified?: boolean;
}

type ModelOperationResult = {
  success: boolean;
  error?: string;
  code?: string;
  details?: string;
};

declare global {
  interface Window {
    retoneAPI: {
      // BYOK keys, kept in the OS secure store
      getOpenAIKey: () => Promise<string>;
      saveOpenAIKey: (key: string) => Promise<{ success: boolean }>;
      getAnthropicKey: () => Promise<string>;
      saveAnthropicKey: (key: string) => Promise<{ success: boolean }>;
      getGeminiKey: () => Promise<string>;
      saveGeminiKey: (key: string) => Promise<{ success: boolean }>;
      getGroqKey: () => Promise<string>;
      saveGroqKey: (key: string) => Promise<{ success: boolean }>;
      getOpenrouterKey: () => Promise<string>;
      saveOpenrouterKey: (key: string) => Promise<{ success: boolean }>;
      getCleanupCustomKey: () => Promise<string>;
      saveCleanupCustomKey: (key: string) => Promise<{ success: boolean }>;
      onApiKeyUpdated?: (callback: (storeKey: string) => void) => () => void;
      testProviderConnection?: (
        config: ProviderConnectionCheck
      ) => Promise<ProviderConnectionResult>;

      getPlatform: () => "darwin" | "win32" | "linux";
      getAppVersion?: () => Promise<string>;
      openExternal: (url: string) => Promise<{ success: boolean; error?: string }>;
      writeClipboard: (text: string) => Promise<{ success: boolean; xwaylandOnly?: boolean }>;

      getAutoStartEnabled?: () => Promise<{
        enabled: boolean;
        requiresApproval: boolean;
        available: boolean;
      }>;
      setAutoStartEnabled?: (enabled: boolean) => Promise<{ success: boolean; error?: string }>;
      getStartMinimized?: () => Promise<boolean>;
      setStartMinimized?: (enabled: boolean) => Promise<{ success: boolean }>;
      getAppIcon?: () => Promise<AppIconId>;
      setAppIcon?: (icon: AppIconId) => Promise<{ success: boolean; icon?: AppIconId; error?: string }>;
      getUiLanguage?: () => Promise<string>;
      setUiLanguage?: (language: string) => Promise<{ success: boolean; language: string }>;
      syncStartupPreferences?: (prefs: {
        keepLocalModelLoaded: boolean;
        toneRewriteWholeField: boolean;
      }) => Promise<void>;

      checkAccessibilityPermission?: (silent?: boolean) => Promise<boolean>;
      openAccessibilitySettings?: () => Promise<{ success: boolean; error?: string }>;
      openLoginItemsSettings?: () => Promise<{ success: boolean; error?: string }>;

      // Hotkeys
      setHotkeyListeningMode?: (enabled: boolean) => Promise<{ success: boolean }>;
      getHotkeyModeInfo?: () => Promise<{
        isUsingGnome: boolean;
        isUsingHyprland: boolean;
        isUsingKDE: boolean;
        isUsingNativeShortcut: boolean;
        linuxInputAccessDenied?: boolean;
      }>;
      onGlobeKeyPressed?: (callback: () => void) => () => void;
      onGlobeKeyReleased?: (callback: () => void) => () => void;

      // ReTone
      updateToneRewriteHotkey?: (hotkey: string) => Promise<{ success: boolean; message?: string }>;
      getToneRewriteKey?: () => Promise<string>;
      toneRewriteGetPayload?: () => Promise<ToneRewritePickerPayload | null>;
      toneRewriteApply?: (
        sessionId: string,
        text: string
      ) => Promise<{ success: boolean; code?: string; copied?: boolean }>;
      toneRewriteClose?: () => Promise<void>;
      toneRewriteSetHeight?: (height: number) => Promise<void>;
      onToneRewriteShow?: (callback: (payload: ToneRewritePickerPayload) => void) => () => void;
      onToneRewriteReset?: (callback: () => void) => () => void;

      // Model calls that run in the main process
      processLocalReasoning: (
        text: string,
        modelId: string,
        agentName: string | null,
        config: ReasoningConfig
      ) => Promise<{
        success: boolean;
        text?: string;
        error?: string;
        code?: string;
        details?: Record<string, unknown>;
      }>;
      /** Aborts the local request tagged with this `requestId`, if it is still in flight. */
      cancelLocalReasoning: (requestId: string) => Promise<void>;
      checkLocalReasoningAvailable: () => Promise<boolean>;
      processAnthropicReasoning: (
        text: string,
        modelId: string,
        agentName: string | null,
        config: ReasoningConfig & { supportsTemperature?: boolean }
      ) => Promise<{ success: boolean; text?: string } & Partial<IpcErrorFields>>;

      // Local models
      modelGetAll: () => Promise<LocalLLMModelStatus[]>;
      modelGetActiveDownloads: () => Promise<LocalModelDownloadStatus[]>;
      modelDownload: (modelId: string) => Promise<ModelOperationResult & { path?: string }>;
      modelDelete: (modelId: string) => Promise<ModelOperationResult>;
      modelCancelDownload: (modelId: string) => Promise<{ success: boolean; error?: string }>;
      onModelDownloadProgress: (
        callback: (event: unknown, data: LocalLLMDownloadProgressEvent) => void
      ) => () => void;
      llamaServerStart: (
        modelId: string
      ) => Promise<{ success: boolean; port?: number; apiKey?: string; error?: string }>;
      llamaServerStatus: () => Promise<LlamaServerStatus>;
      llamaGpuReset: () => Promise<{ success: boolean; error?: string }>;
      detectVulkanGpu?: () => Promise<VulkanGpuResult>;
      getLlamaVulkanStatus?: () => Promise<LlamaVulkanStatus>;
      downloadLlamaVulkanBinary?: () => Promise<{
        success: boolean;
        cancelled?: boolean;
        error?: string;
      }>;
      cancelLlamaVulkanDownload?: () => Promise<{ success: boolean }>;
      deleteLlamaVulkanBinary?: () => Promise<{ success: boolean; error?: string }>;
      onLlamaVulkanDownloadProgress?: (
        callback: (data: LlamaVulkanDownloadProgress) => void
      ) => () => void;

      // Logging
      getLogLevel?: () => Promise<string>;
      log?: (entry: {
        level: string;
        message: string;
        meta?: unknown;
        scope?: string;
        source?: string;
      }) => Promise<void>;
    };
  }
}
