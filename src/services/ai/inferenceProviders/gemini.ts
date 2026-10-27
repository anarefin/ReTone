import type { InferenceProvider } from "./types";
import { withRetry, createApiRetryStrategy } from "../../../utils/retry";
import { API_ENDPOINTS } from "../../../config/constants";
import {
  getLlmRequestTimeoutSeconds,
  llmRequestTimeoutError,
} from "../../../helpers/llmRequestTimeout.js";
import { extractGeminiText } from "../../../helpers/geminiResponse.js";
import { chosenReasoningEffort, emptyOutputError, truncatedOutputError } from "../chatRequestBody";
import logger from "../../../utils/logger";
import {
  asProviderError,
  providerHttpError,
  redactProviderBody,
} from "../../../helpers/providerHttpErrors.js";
import { providerFetch } from "../../../platform/providerFetch";

interface GeminiResponse {
  candidates?: Array<{
    content?: { parts?: Array<{ text?: string; thought?: boolean }> };
    finishReason?: string;
  }>;
  usageMetadata?: {
    promptTokenCount?: number;
    candidatesTokenCount?: number;
    thoughtsTokenCount?: number;
    totalTokenCount?: number;
  };
}

interface GeminiGenerationConfig {
  temperature: number;
  thinkingConfig?: {
    thinkingLevel?: string;
    thinkingBudget?: number;
    includeThoughts: boolean;
  };
}

// Thinking controls differ by model: Pro cannot use minimal, and 2.5 uses budgets.
const minimalThinking: Record<string, GeminiGenerationConfig["thinkingConfig"]> = {
  "gemini-3.5-flash": { thinkingLevel: "minimal", includeThoughts: false },
  "gemini-3.5-flash-lite": { thinkingLevel: "minimal", includeThoughts: false },
  "gemini-3.1-flash-lite": { thinkingLevel: "minimal", includeThoughts: false },
  "gemini-3-flash-preview": { thinkingLevel: "minimal", includeThoughts: false },
  "gemini-3.1-pro-preview": { thinkingLevel: "low", includeThoughts: false },
  "gemini-2.5-flash": { thinkingBudget: 0, includeThoughts: false },
  "gemini-2.5-flash-lite": { thinkingBudget: 0, includeThoughts: false },
  "gemini-2.5-pro": { thinkingBudget: 128, includeThoughts: false },
};

export const geminiProvider: InferenceProvider = {
  id: "gemini",
  async call({ text, model, agentName, config, ctx }) {
    logger.logReasoning("GEMINI_START", { model, agentName, hasApiKey: false });
    const apiKey = await ctx.getApiKey("gemini");
    logger.logReasoning("GEMINI_API_KEY", { hasApiKey: !!apiKey, keyLength: apiKey?.length || 0 });

    const systemPrompt = config.systemPrompt || "";
    const userContent = text;

    const isGemini = model.startsWith("gemini-");
    const isGemini3 = model.startsWith("gemini-3-") || model.startsWith("gemini-3.");
    const generationConfig: GeminiGenerationConfig = {
      // Google recommends 1.0 for Gemini 3 to avoid low-temperature looping.
      // https://ai.google.dev/gemini-api/docs/gemini-3#temperature
      temperature: isGemini3 ? 1 : (config.temperature ?? 0.3),
      // No maxOutputTokens. Gemini bills thinking against it, so any budget sized
      // for the text starved thinking models, and a caller's pinned budget
      // is priced for the local path, not for Gemini. The model's own
      // limit and the request timeout bound the reply.
    };

    // A chosen level is only offered while thinking isn't suppressed, so it wins
    // over the minimal floor that a model without the toggle gets by default.
    const thinkingLevel = chosenReasoningEffort("gemini", model, config);
    if (thinkingLevel) {
      generationConfig.thinkingConfig = { thinkingLevel, includeThoughts: false };
    } else if (config.disableThinking === true && minimalThinking[model]) {
      generationConfig.thinkingConfig = minimalThinking[model];
    }

    const parts = [
      // Gemma models served through the Gemini API take the instructions inline.
      { text: isGemini ? userContent : `${systemPrompt}\n\n${userContent}` },
    ];
    const requestBody = {
      ...(isGemini ? { systemInstruction: { parts: [{ text: systemPrompt }] } } : {}),
      contents: [{ parts }],
      generationConfig,
    };

    const response = await withRetry(async () => {
      // Metadata only: body previews would leak the user's text.
      logger.logReasoning("GEMINI_REQUEST", {
        endpoint: `${API_ENDPOINTS.GEMINI}/models/${model}:generateContent`,
        model,
        hasApiKey: !!apiKey,
        generationConfig,
        textLength: text.length,
      });

      const controller = new AbortController();
      const timeoutSeconds = getLlmRequestTimeoutSeconds({ scope: config.inferenceScope });
      const timeoutId = setTimeout(() => controller.abort(), timeoutSeconds * 1000);
      try {
        const res = await providerFetch(`${API_ENDPOINTS.GEMINI}/models/${model}:generateContent`, {
          method: "POST",
          headers: { "Content-Type": "application/json", "x-goog-api-key": apiKey },
          body: JSON.stringify(requestBody),
          signal: controller.signal,
        });

        if (!res.ok) {
          const errorText = await res.text();
          logger.logReasoning("GEMINI_API_ERROR_DETAIL", {
            status: res.status,
            statusText: res.statusText,
            fullResponse: redactProviderBody(errorText),
          });

          throw providerHttpError({
            provider: "Gemini",
            model,
            status: res.status,
            body: errorText,
            headers: res.headers,
            surface: "llm",
          });
        }

        const jsonResponse = (await res.json()) as GeminiResponse;
        logger.logReasoning("GEMINI_RAW_RESPONSE", {
          hasResponse: !!jsonResponse,
          hasCandidates: !!jsonResponse?.candidates,
          candidatesLength: jsonResponse?.candidates?.length || 0,
          finishReason: jsonResponse?.candidates?.[0]?.finishReason,
          usageMetadata: jsonResponse?.usageMetadata,
        });
        return jsonResponse;
      } catch (error) {
        if ((error as Error).name === "AbortError") {
          throw llmRequestTimeoutError(timeoutSeconds);
        }
        throw error;
      } finally {
        clearTimeout(timeoutId);
      }
    }, createApiRetryStrategy()).catch((error) => {
      // Classified only once it has left withRetry, so the deadline is still
      // attempted exactly once.
      throw asProviderError(error, { provider: "Gemini", model, surface: "llm" });
    });

    const candidate = response.candidates?.[0];
    // Outside withRetry: cutoffs and blocks are not retried.
    if (candidate?.finishReason === "MAX_TOKENS") {
      throw truncatedOutputError();
    }
    if (candidate?.finishReason !== "STOP") {
      throw new Error(
        `Gemini returned incomplete output (${candidate?.finishReason || "missing finish reason"})`
      );
    }
    const responseText = extractGeminiText(candidate);
    if (!responseText) {
      logger.logReasoning("GEMINI_EMPTY_RESPONSE", {
        model,
        finishReason: candidate?.finishReason,
      });
      throw emptyOutputError("Gemini returned empty response");
    }

    logger.logReasoning("GEMINI_RESPONSE", {
      model,
      responseLength: responseText.length,
      tokensUsed: response.usageMetadata?.totalTokenCount || 0,
      success: true,
    });
    return responseText;
  },
};
