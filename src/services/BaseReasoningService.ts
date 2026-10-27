import type { InferenceScope } from "../config/inferenceScopes";

export interface ReasoningConfig {
  maxTokens?: number;
  temperature?: number;
  contextSize?: number;
  systemPrompt?: string;
  baseUrl?: string;
  customApiKey?: string;
  provider?: string;
  disableThinking?: boolean;
  reasoningEffort?: string;
  requireCompleteOutput?: boolean;
  /**
   * Local models only: when the prompt leaves less than `maxTokens` of room and
   * the reply fills what is left, fail as CONTEXT_TOO_LARGE instead of
   * returning a reply the context window clipped.
   */
  refuseClippedByWindow?: boolean;
  /**
   * Tags the request so a cancel can abort it: `cancelLocalReasoning` for local
   * models, `cancelRequest` (ai/requestCancellation) for the renderer's HTTP routes.
   */
  requestId?: string;
  inferenceScope?: InferenceScope;
}

export abstract class BaseReasoningService {
  protected calculateMaxTokens(
    textLength: number,
    minTokens = 100,
    maxTokens = 2048,
    multiplier = 2
  ): number {
    return Math.max(minTokens, Math.min(textLength * multiplier, maxTokens));
  }

  abstract isAvailable(): Promise<boolean>;

  abstract processText(
    text: string,
    modelId: string,
    agentName?: string | null,
    config?: ReasoningConfig
  ): Promise<string>;
}
