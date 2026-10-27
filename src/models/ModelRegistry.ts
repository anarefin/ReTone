import modelDataRaw from "./modelRegistryData.json";
import type { InferenceMode } from "../types/nativeApi";

export interface ModelDefinition {
  id: string;
  name: string;
  size: string;
  sizeBytes: number;
  description: string;
  descriptionKey?: string;
  fileName: string;
  quantization: string;
  contextLength: number;
  hfRepo: string;
  recommended?: boolean;
  supportsThinking?: boolean;
  // Optional MTP speculative-decoding drafter downloaded alongside the main GGUF.
  draftHfRepo?: string;
  draftFileName?: string;
  draftSizeBytes?: number;
}

export interface LocalProviderData {
  id: string;
  name: string;
  baseUrl: string;
  models: ModelDefinition[];
}

export interface ModelProvider {
  id: string;
  name: string;
  baseUrl: string;
  models: ModelDefinition[];
  getDownloadUrl(model: ModelDefinition): string;
}

export interface CloudModelDefinition {
  id: string;
  name: string;
  description: string;
  descriptionKey?: string;
  disableThinking?: boolean;
  supportsThinking?: boolean;
  supportsVision?: boolean;
  tokenParam?: "max_tokens" | "max_completion_tokens";
  supportsTemperature?: boolean;
  reasoningEffort?: ReasoningEffortCapability;
}

export interface ReasoningEffortCapability {
  options: string[];
  defaultValue?: string;
}

export interface CloudProviderData {
  id: string;
  name: string;
  /** Named default for providers whose list order isn't ours — see pickProviderDefaultModel. */
  defaultModel?: string;
  models: CloudModelDefinition[];
}

interface ModelRegistryData {
  cloudProviders: CloudProviderData[];
  localProviders: LocalProviderData[];
}

const modelData: ModelRegistryData = modelDataRaw as ModelRegistryData;

class ModelRegistry {
  private static instance: ModelRegistry;
  private providers = new Map<string, ModelProvider>();

  private constructor() {
    this.registerProvidersFromData();
  }

  static getInstance(): ModelRegistry {
    if (!ModelRegistry.instance) {
      ModelRegistry.instance = new ModelRegistry();
    }
    return ModelRegistry.instance;
  }

  registerProvider(provider: ModelProvider) {
    this.providers.set(provider.id, provider);
  }

  getProvider(providerId: string): ModelProvider | undefined {
    return this.providers.get(providerId);
  }

  getAllProviders(): ModelProvider[] {
    return Array.from(this.providers.values());
  }

  getModel(modelId: string): { model: ModelDefinition; provider: ModelProvider } | undefined {
    for (const provider of this.providers.values()) {
      const model = provider.models.find((m) => m.id === modelId);
      if (model) {
        return { model, provider };
      }
    }
    return undefined;
  }

  getAllModels(): Array<ModelDefinition & { providerId: string }> {
    const models: Array<ModelDefinition & { providerId: string }> = [];
    for (const provider of this.providers.values()) {
      for (const model of provider.models) {
        models.push({ ...model, providerId: provider.id });
      }
    }
    return models;
  }

  getCloudProviders(): CloudProviderData[] {
    return modelData.cloudProviders;
  }

  private registerProvidersFromData() {
    for (const providerData of modelData.localProviders) {
      this.registerProvider({
        id: providerData.id,
        name: providerData.name,
        baseUrl: providerData.baseUrl,
        models: providerData.models,
        getDownloadUrl(model: ModelDefinition): string {
          return `${providerData.baseUrl}/${model.hfRepo}/resolve/main/${model.fileName}`;
        },
      });
    }
  }
}

export const modelRegistry = ModelRegistry.getInstance();

export interface ReasoningModel {
  value: string;
  label: string;
  description: string;
  descriptionKey?: string;
}

export interface ReasoningProvider {
  name: string;
  models: ReasoningModel[];
}

export type ReasoningProviders = Record<string, ReasoningProvider>;

export function toReasoningModel(m: CloudModelDefinition): ReasoningModel {
  return {
    value: m.id,
    label: m.name,
    description: m.description,
    descriptionKey: m.descriptionKey,
  };
}

export function isProviderValidForMode(provider: string, mode: InferenceMode): boolean {
  switch (mode) {
    case "providers":
      return (
        provider === "custom" ||
        provider === "openrouter" ||
        modelRegistry.getCloudProviders().some((p) => p.id === provider)
      );
    case "local":
      return modelRegistry.getAllProviders().some((p) => p.id === provider);
    default:
      return false;
  }
}

function buildReasoningProviders(): ReasoningProviders {
  const providers: ReasoningProviders = {};

  for (const cloudProvider of modelRegistry.getCloudProviders()) {
    providers[cloudProvider.id] = {
      name: cloudProvider.name,
      models: cloudProvider.models.map(toReasoningModel),
    };
  }

  providers.local = {
    name: "Local AI",
    models: modelRegistry.getAllModels().map((model) => ({
      value: model.id,
      label: model.name,
      description: `${model.description} (${model.size})`,
      descriptionKey: model.descriptionKey,
    })),
  };

  return providers;
}

export const REASONING_PROVIDERS = buildReasoningProviders();

export function getCloudProviderDefaultModelId(providerId: string): string | undefined {
  return modelData.cloudProviders.find((provider) => provider.id === providerId)?.defaultModel;
}

const NON_REGISTRY_PROVIDER_NAMES: Record<string, string> = {
  openrouter: "OpenRouter",
  custom: "Custom",
};

export function getProviderDisplayName(provider: string): string {
  return (
    REASONING_PROVIDERS[provider as keyof typeof REASONING_PROVIDERS]?.name ??
    NON_REGISTRY_PROVIDER_NAMES[provider] ??
    provider
  );
}

// Local catalog IDs group models for selection and downloads, but all execute
// through the single local llama.cpp inference provider. No provider fails
// closed at dispatch instead of guessing one from the model id.
export function resolveInferenceProvider(configuredProvider: string | undefined): string {
  const provider = configuredProvider?.trim();
  if (provider && modelRegistry.getProvider(provider)) return "local";
  return provider || "";
}

// Whether a model id belongs to a cloud provider's catalog. Custom and
// OpenRouter ids are free-form, so any id is valid there.
export function reasoningModelBelongsToProvider(provider: string, model: string): boolean {
  if (!model) return false;
  if (provider === "custom" || provider === "openrouter") return true;
  const providerDef = modelData.cloudProviders.find((p) => p.id === provider);
  return !!providerDef?.models.some((m) => m.id === model);
}

export function getCloudModel(
  modelId: string,
  providerId?: string
): CloudModelDefinition | undefined {
  for (const provider of modelData.cloudProviders) {
    const model = provider.models.find((m) => m.id === modelId);
    if (model) return model;
  }
  // OpenRouter ids carry a vendor prefix (google/gemini-3.5-flash-lite) that the
  // registry stores without, so retry on the upstream id.
  if (providerId === "openrouter" && modelId.includes("/")) {
    return getCloudModel(modelId.slice(modelId.lastIndexOf("/") + 1));
  }
  return undefined;
}

export function getLocalModel(modelId: string): ModelDefinition | undefined {
  return modelRegistry.getModel(modelId)?.model;
}

export interface OpenAiApiConfig {
  tokenParam: "max_tokens" | "max_completion_tokens";
  supportsTemperature: boolean;
}

export function getOpenAiApiConfig(modelId: string, provider?: string): OpenAiApiConfig {
  const model = getCloudModel(modelId);
  if (model?.tokenParam) {
    return {
      tokenParam: model.tokenParam,
      supportsTemperature: model.supportsTemperature ?? true,
    };
  }

  // OpenRouter's vendor-prefixed ids (openai/gpt-4o, anthropic/claude-…) speak
  // standard Chat Completions. Sampling params pass through to the upstream
  // vendor, so a model the registry knows rejects temperature (Claude Opus 4.7+)
  // must keep it omitted here too.
  if (provider === "openrouter" && modelId.includes("/")) {
    const upstream = getCloudModel(modelId, provider);
    return { tokenParam: "max_tokens", supportsTemperature: upstream?.supportsTemperature ?? true };
  }

  // Fallback for models not in the registry (custom model IDs, etc.)
  const isLegacy =
    modelId.startsWith("gpt-3") ||
    modelId.startsWith("gpt-4o") ||
    modelId.startsWith("gpt-4-") ||
    modelId === "gpt-4";

  if (isLegacy) {
    return { tokenParam: "max_tokens", supportsTemperature: true };
  }

  // gpt-4.1* supports temperature but uses max_completion_tokens
  if (modelId.startsWith("gpt-4.1")) {
    return { tokenParam: "max_completion_tokens", supportsTemperature: true };
  }

  // gpt-5* reasoning models: no temperature
  return { tokenParam: "max_completion_tokens", supportsTemperature: false };
}
