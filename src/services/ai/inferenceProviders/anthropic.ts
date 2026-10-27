import type { InferenceProvider } from "./types";
import { getCloudModel } from "../../../models/ModelRegistry";
import logger from "../../../utils/logger";
import { errorFromIpcResult } from "../../../helpers/ipcErrorFields";

export const anthropicProvider: InferenceProvider = {
  id: "anthropic",
  async call({ text, model, agentName, config }) {
    if (typeof window === "undefined" || !window.retoneAPI) {
      throw new Error("Anthropic reasoning is not available in this environment");
    }

    logger.logReasoning("ANTHROPIC_START", { model, agentName, environment: "browser" });
    const startTime = Date.now();

    logger.logReasoning("ANTHROPIC_IPC_CALL", { model, textLength: text.length });

    // Claude models from Opus 4.7 onward reject `temperature` with a 400, so
    // unknown models default to omitting it, which every model accepts.
    const supportsTemperature = getCloudModel(model)?.supportsTemperature ?? false;
    const result = await window.retoneAPI.processAnthropicReasoning(text, model, agentName, {
      ...config,
      systemPrompt: config.systemPrompt || "",
      supportsTemperature,
    });

    const processingTimeMs = Date.now() - startTime;

    if (!result.success) {
      logger.logReasoning("ANTHROPIC_ERROR", { model, processingTimeMs, error: result.error });
      // Main resolves with the serialized error (code, i18n key, status…) so the
      // picker can translate it.
      throw errorFromIpcResult(result);
    }

    logger.logReasoning("ANTHROPIC_SUCCESS", {
      model,
      processingTimeMs,
      resultLength: result.text.length,
    });
    return result.text;
  },
};
