import ReasoningService from "../services/ReasoningService";
import { cancelRequest } from "../services/ai/requestCancellation";
import { getSettings, selectResolvedLLMConfig } from "../stores/settingsStore";
import { isProviderValidForMode } from "../models/ModelRegistry";
import {
  buildToneRewriteSystemPrompt,
  buildToneRewriteUserPrompt,
  cleanToneRewriteOutput,
  type ToneRewriteOption,
} from "../config/prompts/toneRewrite";

export class ToneRewriteError extends Error {
  constructor(
    readonly code: "not_configured" | "empty_output" | "failed",
    message: string
  ) {
    super(message);
  }
}

// The tone rewrite scope as a request: provider, model and endpoint, plus
// whether it can run at all (a model must be picked).
export function resolveToneRewriteInference(settings = getSettings()) {
  const resolved = selectResolvedLLMConfig(settings, "toneRewrite");
  const storedProvider = resolved.provider?.trim() || "";
  const provider =
    resolved.mode === "local"
      ? "local"
      : isProviderValidForMode(storedProvider, resolved.mode)
        ? storedProvider
        : undefined;
  const model = resolved.model?.trim() || "";
  const isCustom = resolved.mode === "providers" && provider === "custom";
  return {
    reachable: !!provider && model.length > 0,
    model,
    config: {
      inferenceScope: "toneRewrite" as const,
      provider,
      baseUrl: isCustom ? resolved.cloudBaseUrl || undefined : undefined,
      customApiKey: isCustom ? resolved.customApiKey || undefined : undefined,
      disableThinking: resolved.disableThinking,
      reasoningEffort: resolved.reasoningEffort,
    },
  };
}

export interface ToneRewriteRequest {
  promise: Promise<string>;
  cancel: () => void;
}

// Providers whose replies the picker can stream: local llama-server and the
// Chat Completions transports processTextStreaming speaks. Anthropic runs the
// whole reply through rewriteInTone instead.
const STREAMING_PROVIDERS = new Set(["local", "openai", "groq", "gemini", "custom", "openrouter"]);

export function canStreamToneRewrite(settings = getSettings()): boolean {
  const { reachable, config } = resolveToneRewriteInference(settings);
  return reachable && !!config.provider && STREAMING_PROVIDERS.has(config.provider);
}

// Starts the local model while the user is still choosing an option, so the
// pick doesn't pay the load. Resolves once the server is up; a failure is left
// for the rewrite itself to report. Not local → resolves at once.
export function warmToneRewriteModel(settings = getSettings()): Promise<void> {
  const { reachable, model, config } = resolveToneRewriteInference(settings);
  if (!reachable || config.provider !== "local" || !model) return Promise.resolve();
  const start = window.retoneAPI?.llamaServerStart;
  if (!start) return Promise.resolve();
  return start(model).then(
    () => undefined,
    () => undefined
  );
}

const toRewriteError = (error: unknown, model: string): unknown => {
  // A local model can stay selected after its file is gone (Settings
  // clears it only while open), which is a setup problem, not a failure.
  const code = (error as { code?: string })?.code;
  if (code === "MODEL_NOT_DOWNLOADED" || code === "MODEL_NOT_FOUND") {
    return new ToneRewriteError("not_configured", `Tone rewrite model ${model} is missing`);
  }
  return error;
};

export function rewriteInTone(text: string, option: ToneRewriteOption): ToneRewriteRequest {
  const requestId = `tone-rewrite-${crypto.randomUUID()}`;
  const { reachable, model, config } = resolveToneRewriteInference();
  const isLocal = config.provider === "local";
  let cancelled = false;

  const run = async () => {
    if (!reachable) {
      throw new ToneRewriteError("not_configured", "Tone rewrite model is not configured");
    }
    let output: string;
    try {
      output = await ReasoningService.processText(buildToneRewriteUserPrompt(text), model, null, {
        ...config,
        systemPrompt: buildToneRewriteSystemPrompt(option),
        temperature: 0.3,
        maxTokens: 4096,
        contextSize: 16384,
        requireCompleteOutput: true,
        requestId,
        ...(isLocal && { disableThinking: true }),
      });
    } catch (error) {
      throw toRewriteError(error, model);
    }
    const rewritten = cleanToneRewriteOutput(output, text);
    if (!rewritten.trim()) {
      throw new ToneRewriteError("empty_output", "Tone rewrite returned no text");
    }
    return rewritten;
  };

  return {
    promise: run(),
    cancel: () => {
      if (cancelled) return;
      cancelled = true;
      if (isLocal) void window.retoneAPI?.cancelLocalReasoning?.(requestId);
      else cancelRequest(requestId);
    },
  };
}

// Same request as rewriteInTone, streamed: onDelta gets the reply so far as it
// arrives. The local stream goes straight to llama-server's port, outside
// localReasoningBridge's busy lock; llama-server queues it behind any running
// request itself.
export function rewriteInToneStreaming(
  text: string,
  option: ToneRewriteOption,
  onDelta: (soFar: string) => void
): ToneRewriteRequest {
  const { reachable, model, config } = resolveToneRewriteInference();
  const isLocal = config.provider === "local";
  let cancelled = false;

  const run = async () => {
    if (!reachable || !config.provider) {
      throw new ToneRewriteError("not_configured", "Tone rewrite model is not configured");
    }
    const systemPrompt = buildToneRewriteSystemPrompt(option);
    let output = "";
    try {
      const stream = ReasoningService.processTextStreaming(
        [
          { role: "system", content: systemPrompt },
          { role: "user", content: buildToneRewriteUserPrompt(text) },
        ],
        model,
        config.provider,
        {
          ...config,
          systemPrompt,
          temperature: 0.3,
          maxTokens: 4096,
          ...(isLocal && { disableThinking: true }),
        }
      );
      for await (const chunk of stream) {
        if (cancelled) break;
        output += chunk;
        onDelta(output);
      }
    } catch (error) {
      throw toRewriteError(error, model);
    }
    if (cancelled) throw new ToneRewriteError("failed", "Tone rewrite cancelled");
    const rewritten = cleanToneRewriteOutput(output, text);
    if (!rewritten.trim()) {
      throw new ToneRewriteError("empty_output", "Tone rewrite returned no text");
    }
    return rewritten;
  };

  return {
    promise: run(),
    cancel: () => {
      if (cancelled) return;
      cancelled = true;
      ReasoningService.cancelActiveStream();
    },
  };
}
