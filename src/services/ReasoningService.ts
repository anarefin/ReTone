import { resolveInferenceProvider, getProviderDisplayName } from "../models/ModelRegistry";
import { BaseReasoningService, ReasoningConfig } from "./BaseReasoningService";
import { SecureCache } from "../utils/SecureCache";
import { withRetry, createApiRetryStrategy, httpError } from "../utils/retry";
import { API_ENDPOINTS, TOKEN_LIMITS, buildApiUrl } from "../config/constants";
import logger from "../utils/logger";
import { stripThinkingTags } from "../helpers/stripThinking.js";
import {
  getLlmRequestTimeoutSeconds,
  getLlmStreamingFirstByteTimeoutSeconds,
  llmRequestTimeoutError,
} from "../helpers/llmRequestTimeout.js";
import { PROVIDER_REGISTRY, type ProviderContext } from "./ai/inferenceProviders";
import { resolveConfiguredOpenAIBase } from "./ai/openaiBase";
import {
  applyChatCompletionsParams,
  emptyResponseError,
  fetchWithParamFallback,
  isTruncatedFinishReason,
  truncatedOutputError,
  type ParamFallbackDetails,
} from "./ai/chatRequestBody";
import { abortOnCancel, isRequestCancelled, requestCancelledError } from "./ai/requestCancellation";
import { openCodeSessionHeaders } from "./ai/openCodeSession";
import { createStreamingThinkFilter } from "./ai/streamingThinkFilter";
import { extractApiErrorMessage } from "./ai/apiErrorMessage";
import {
  PROVIDER_ERROR_CODES,
  asProviderError,
  providerError,
  providerHttpError,
  redactProviderBody,
} from "../helpers/providerHttpErrors.js";
import { providerFetch } from "../platform/providerFetch";

type KeyProvider = "openai" | "anthropic" | "gemini" | "groq" | "openrouter" | "custom";

const BYOK_STREAM_PROVIDERS = ["openai", "groq", "gemini", "custom", "openrouter"] as const;

function logParamFallback(logEvent: string) {
  return (details: ParamFallbackDetails) => logger.logReasoning(logEvent, details);
}

// Runs ReTone's model calls: one-shot through the provider registry, or
// streamed over Chat Completions for the picker's live preview.
class ReasoningService extends BaseReasoningService {
  private apiKeyCache: SecureCache<string>;
  private cacheCleanupStop: (() => void) | undefined;
  private streamAbortController: AbortController | null = null;
  private activeRequestControllers = new Set<AbortController>();
  private requestCancellationGeneration = 0;

  private readonly providerContext: ProviderContext;

  constructor() {
    super();
    this.apiKeyCache = new SecureCache();
    this.cacheCleanupStop = this.apiKeyCache.startAutoCleanup();
    this.providerContext = {
      getApiKey: (provider: string) => this.getApiKey(provider as KeyProvider),
      callChatCompletionsApi: this.callChatCompletionsApi.bind(this),
      calculateMaxTokens: this.calculateMaxTokens.bind(this),
    };

    if (typeof window !== "undefined") {
      window.addEventListener("beforeunload", () => this.destroy());
    }
  }

  private async getApiKey(provider: KeyProvider): Promise<string> {
    if (provider === "custom") {
      // A Custom endpoint may need no key at all.
      try {
        return ((await window.retoneAPI?.getCustomKey?.()) || "").trim();
      } catch (err) {
        logger.logReasoning("CUSTOM_KEY_IPC_ERROR", { error: (err as Error)?.message });
        return "";
      }
    }

    let apiKey = this.apiKeyCache.get(provider);
    if (!apiKey) {
      try {
        const keyGetters = {
          openai: () => window.retoneAPI.getOpenAIKey(),
          anthropic: () => window.retoneAPI.getAnthropicKey(),
          gemini: () => window.retoneAPI.getGeminiKey(),
          groq: () => window.retoneAPI.getGroqKey(),
          openrouter: () => window.retoneAPI.getOpenrouterKey(),
        };
        apiKey = (await keyGetters[provider]()) ?? undefined;
        if (apiKey) this.apiKeyCache.set(provider, apiKey);
      } catch (error) {
        logger.logReasoning(`${provider.toUpperCase()}_KEY_FETCH_ERROR`, {
          provider,
          error: (error as Error).message,
        });
      }
    }

    if (!apiKey) {
      throw providerError(PROVIDER_ERROR_CODES.KEY_MISSING, {
        provider: getProviderDisplayName(provider),
        surface: "llm",
      });
    }
    return apiKey;
  }

  private async callChatCompletionsApi(
    endpoint: string,
    apiKey: string,
    model: string,
    text: string,
    _agentName: string | null,
    config: ReasoningConfig,
    providerName: string
  ): Promise<string> {
    const messages = [
      { role: "system", content: config.systemPrompt || "" },
      { role: "user", content: text },
    ];

    const requestBody: Record<string, unknown> = { model, messages };
    applyChatCompletionsParams(requestBody, {
      model,
      provider: providerName,
      endpoint,
      config,
      maxTokens:
        config.maxTokens ||
        Math.max(
          4096,
          this.calculateMaxTokens(
            text.length,
            TOKEN_LIMITS.MIN_TOKENS,
            TOKEN_LIMITS.MAX_TOKENS,
            TOKEN_LIMITS.TOKEN_MULTIPLIER
          )
        ),
    });

    logger.logReasoning(`${providerName.toUpperCase()}_REQUEST`, {
      endpoint,
      model,
      hasApiKey: !!apiKey,
    });

    // Minted before the retry loop so every attempt of this call is one conversation.
    const openCodeHeaders = openCodeSessionHeaders(endpoint);
    const errorContext = { provider: providerName, model, surface: "llm" };

    const requestGeneration = this.requestCancellationGeneration;
    const response = await withRetry(async () => {
      if (requestGeneration !== this.requestCancellationGeneration) {
        throw httpError("Request cancelled", 499);
      }
      const controller = new AbortController();
      this.activeRequestControllers.add(controller);
      const timeoutSeconds = getLlmRequestTimeoutSeconds({ scope: config.inferenceScope });
      const timeoutId = setTimeout(() => controller.abort(), timeoutSeconds * 1000);
      const unlinkCancel = abortOnCancel(config.requestId, controller);
      try {
        const headers: Record<string, string> = {
          "Content-Type": "application/json",
          ...openCodeHeaders,
        };
        if (apiKey) headers["Authorization"] = `Bearer ${apiKey}`;

        const res = await fetchWithParamFallback(
          endpoint,
          () =>
            providerFetch(endpoint, {
              method: "POST",
              headers,
              body: JSON.stringify(requestBody),
              signal: controller.signal,
            }),
          requestBody,
          logParamFallback(`${providerName.toUpperCase()}_PARAM_FALLBACK`)
        );

        if (!res.ok) {
          const errorText = await res.text();
          // Providers echo the rejected key back, so every logged field is redacted.
          logger.logReasoning(`${providerName.toUpperCase()}_API_ERROR_DETAIL`, {
            status: res.status,
            body: redactProviderBody(errorText),
          });
          throw providerHttpError({
            ...errorContext,
            status: res.status,
            body: errorText,
            headers: res.headers,
          });
        }

        return await res.json();
      } catch (error) {
        if ((error as Error).name === "AbortError") {
          if (requestGeneration !== this.requestCancellationGeneration) {
            throw httpError("Request cancelled", 499);
          }
          if (isRequestCancelled(config.requestId)) throw requestCancelledError();
          throw llmRequestTimeoutError(timeoutSeconds);
        }
        throw error;
      } finally {
        clearTimeout(timeoutId);
        unlinkCancel();
        this.activeRequestControllers.delete(controller);
      }
    }, createApiRetryStrategy()).catch((error) => {
      // Classified only once it has left withRetry, so the deadline is still
      // attempted exactly once.
      throw asProviderError(error, errorContext);
    });

    const choice = response?.choices?.[0];
    if (!choice) {
      throw new Error(`Invalid response structure from ${providerName} API`);
    }
    if (config.requireCompleteOutput && isTruncatedFinishReason(choice?.finish_reason)) {
      throw truncatedOutputError();
    }
    // Reasoning models leak <think> blocks into non-streamed output; strip them
    // unless the user explicitly enabled thinking.
    const rawContent = choice.message?.content?.trim() || "";
    const responseText =
      config.disableThinking !== false ? stripThinkingTags(rawContent) : rawContent;

    if (!responseText) {
      throw (
        emptyResponseError(providerName, config, isTruncatedFinishReason(choice.finish_reason)) ??
        new Error(`${providerName} returned empty response`)
      );
    }
    return responseText;
  }

  async processText(
    text: string,
    model: string = "",
    agentName: string | null = null,
    config: ReasoningConfig = {}
  ): Promise<string> {
    const trimmedModel = model?.trim?.() || "";
    const providerId = resolveInferenceProvider(config.provider);
    if (!providerId) {
      throw new Error("No reasoning provider selected");
    }
    if (!trimmedModel) {
      throw new Error("No reasoning model selected");
    }

    const handler = PROVIDER_REGISTRY[providerId];
    if (!handler) {
      throw new Error(`Unsupported reasoning provider: ${providerId}`);
    }

    logger.logReasoning("PROVIDER_SELECTION", {
      provider: providerId,
      model: trimmedModel,
      textLength: text.length,
    });

    const startTime = Date.now();
    try {
      const result = await handler.call({
        text,
        model: trimmedModel,
        agentName,
        config,
        ctx: this.providerContext,
      });
      logger.logReasoning("PROVIDER_SUCCESS", {
        provider: providerId,
        model: trimmedModel,
        processingTimeMs: Date.now() - startTime,
        resultLength: result.length,
      });
      return result;
    } catch (error) {
      logger.logReasoning("PROVIDER_ERROR", {
        provider: providerId,
        model: trimmedModel,
        error: (error as Error).message,
      });
      throw error;
    }
  }

  async *processTextStreaming(
    messages: Array<{ role: string; content: string }>,
    model: string,
    provider: string,
    config: ReasoningConfig & { systemPrompt: string }
  ): AsyncGenerator<string, void, unknown> {
    const abortController = new AbortController();
    this.streamAbortController = abortController;
    try {
      yield* this.processTextStreamingRaw(messages, model, provider, config, abortController);
    } finally {
      if (this.streamAbortController === abortController) {
        this.streamAbortController = null;
      }
    }
  }

  // Chat Completions streaming: the local llama-server's port, or a BYOK
  // provider's endpoint. Anthropic has no such transport here.
  private async resolveStreamEndpoint(
    model: string,
    provider: string,
    config: ReasoningConfig
  ): Promise<{ endpoint: string; apiKey: string; isLocal: boolean }> {
    if (provider === "local") {
      const serverResult = await window.retoneAPI.llamaServerStart(model);
      if (!serverResult.success || !serverResult.port) {
        throw new Error(serverResult.error || "Failed to start local model server");
      }
      // llama-server runs with a per-launch API key; without it every
      // request but /health is refused (401).
      return {
        endpoint: `http://127.0.0.1:${serverResult.port}/v1/chat/completions`,
        apiKey: serverResult.apiKey ?? "",
        isLocal: true,
      };
    }

    if (!(BYOK_STREAM_PROVIDERS as readonly string[]).includes(provider)) {
      throw new Error(`${provider} streaming is not supported`);
    }
    const isCustom = provider === "custom";
    const apiKey =
      (isCustom ? config.customApiKey?.trim() : "") ||
      (await this.getApiKey(provider as KeyProvider));
    const chatBase = isCustom
      ? resolveConfiguredOpenAIBase(provider, config.baseUrl)
      : (
          {
            openai: API_ENDPOINTS.OPENAI_BASE,
            groq: API_ENDPOINTS.GROQ_BASE,
            gemini: API_ENDPOINTS.GEMINI,
            openrouter: API_ENDPOINTS.OPENROUTER_BASE,
          } as Record<string, string>
        )[provider];
    const endpoint = buildApiUrl(
      chatBase,
      provider === "gemini" ? "/openai/chat/completions" : "/chat/completions"
    );
    return { endpoint, apiKey, isLocal: false };
  }

  private async *processTextStreamingRaw(
    messages: Array<{ role: string; content: string }>,
    model: string,
    provider: string,
    config: ReasoningConfig & { systemPrompt: string },
    abortController: AbortController
  ): AsyncGenerator<string, void, unknown> {
    const { endpoint, apiKey, isLocal } = await this.resolveStreamEndpoint(model, provider, config);
    if (abortController.signal.aborted) return;

    const requestBody: Record<string, unknown> = { model, messages, stream: true };
    applyChatCompletionsParams(requestBody, {
      model,
      provider: isLocal ? "local" : provider,
      endpoint,
      config,
      maxTokens: config.maxTokens || Math.max(4096, TOKEN_LIMITS.MAX_TOKENS),
    });

    logger.logReasoning("STREAM_REQUEST", {
      endpoint,
      model,
      provider,
      isLocal,
      messageCount: messages.length,
    });

    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...openCodeSessionHeaders(endpoint),
    };
    if (apiKey) headers["Authorization"] = `Bearer ${apiKey}`;

    const timeoutSeconds = getLlmRequestTimeoutSeconds({ streaming: true });
    let timeoutTriggered = false;
    let timeoutId: ReturnType<typeof setTimeout> | undefined;
    const resetTimeout = (seconds: number) => {
      clearTimeout(timeoutId);
      if (abortController.signal.aborted) return;
      timeoutId = setTimeout(() => {
        if (abortController.signal.aborted) return;
        timeoutTriggered = true;
        abortController.abort();
      }, seconds * 1000);
    };
    resetTimeout(getLlmStreamingFirstByteTimeoutSeconds({ selfHosted: isLocal }));

    let response: Response;
    try {
      response = await fetchWithParamFallback(
        endpoint,
        () =>
          providerFetch(endpoint, {
            method: "POST",
            headers,
            body: JSON.stringify(requestBody),
            signal: abortController.signal,
          }),
        requestBody,
        logParamFallback("STREAM_PARAM_FALLBACK")
      );
    } catch (error) {
      clearTimeout(timeoutId);
      if ((error as Error).name === "AbortError" && abortController.signal.aborted) {
        if (!timeoutTriggered) return;
        throw new Error("Streaming request timed out");
      }
      throw error;
    }

    if (!response.ok) {
      clearTimeout(timeoutId);
      const errorText = await response.text();
      logger.logReasoning("STREAM_ERROR", {
        status: response.status,
        body: redactProviderBody(errorText),
      });
      if (!isLocal) {
        throw providerHttpError({
          provider: getProviderDisplayName(provider),
          model,
          status: response.status,
          body: errorText,
          headers: response.headers,
          surface: "llm",
        });
      }
      let errorMessage: string;
      try {
        errorMessage = extractApiErrorMessage(
          JSON.parse(errorText),
          `API error: ${response.status}`
        );
      } catch {
        errorMessage = errorText || `API error: ${response.status}`;
      }
      throw new Error(errorMessage);
    }

    const reader = response.body?.getReader();
    if (!reader) {
      clearTimeout(timeoutId);
      throw new Error("No response body");
    }

    const decoder = new TextDecoder();
    let buffer = "";
    const filterThinkTags =
      isLocal && config.disableThinking !== false ? createStreamingThinkFilter() : null;

    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        // Reset on bytes, including keepalives and partial SSE frames. A reply
        // may stream for much longer than the idle timeout.
        if (value.byteLength > 0) resetTimeout(timeoutSeconds);

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() || "";

        for (const line of lines) {
          const trimmed = line.trim();
          if (!trimmed || !trimmed.startsWith("data: ")) continue;

          const data = trimmed.slice(6);
          if (data === "[DONE]") {
            const trailing = filterThinkTags?.finish();
            if (trailing) yield trailing;
            return;
          }

          try {
            const parsed = JSON.parse(data);
            let content = parsed.choices?.[0]?.delta?.content;
            if (!content) continue;
            if (filterThinkTags) {
              content = filterThinkTags(content);
              if (!content) continue;
            }
            yield content;
          } catch {
            // skip malformed SSE chunks
          }
        }
      }

      const trailing = filterThinkTags?.finish();
      if (trailing) yield trailing;
    } catch (error) {
      if ((error as Error).name === "AbortError" && abortController.signal.aborted) {
        if (!timeoutTriggered) return;
        throw new Error("Streaming request timed out");
      }
      throw error;
    } finally {
      clearTimeout(timeoutId);
      reader.releaseLock();
    }
  }

  cancelActiveStream(): void {
    this.streamAbortController?.abort();
    this.streamAbortController = null;
  }

  // Aborts everything in flight in this renderer.
  cancelAllRequests(): void {
    this.requestCancellationGeneration += 1;
    for (const controller of this.activeRequestControllers) controller.abort();
    this.activeRequestControllers.clear();
    this.cancelActiveStream();
  }

  async isAvailable(): Promise<boolean> {
    try {
      const keys = await Promise.all([
        window.retoneAPI?.getOpenAIKey?.(),
        window.retoneAPI?.getAnthropicKey?.(),
        window.retoneAPI?.getGeminiKey?.(),
        window.retoneAPI?.getGroqKey?.(),
        window.retoneAPI?.getOpenrouterKey?.(),
        window.retoneAPI?.checkLocalReasoningAvailable?.(),
      ]);
      return keys.some(Boolean);
    } catch (error) {
      logger.logReasoning("API_KEY_CHECK_ERROR", { error: (error as Error).message });
      return false;
    }
  }

  clearApiKeyCache(provider?: KeyProvider): void {
    if (provider) {
      if (provider !== "custom") this.apiKeyCache.delete(provider);
    } else {
      this.apiKeyCache.clear();
    }
    logger.logReasoning("API_KEY_CACHE_CLEARED", { provider: provider ?? "all" });
  }

  destroy(): void {
    this.cancelAllRequests();
    this.cacheCleanupStop?.();
  }
}

export default new ReasoningService();
