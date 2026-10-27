import type { InferenceProvider } from "./types";
import logger from "../../../utils/logger";
import { buildLocalInferenceError } from "../../../utils/localInferenceError";

export const localProvider: InferenceProvider = {
  id: "local",
  async call({ text, model, agentName, config }) {
    if (typeof window === "undefined" || !window.retoneAPI) {
      throw new Error("Local reasoning is not available in this environment");
    }

    logger.logReasoning("LOCAL_START", { model, agentName, environment: "browser" });
    const startTime = Date.now();

    logger.logReasoning("LOCAL_IPC_CALL", { model, textLength: text.length });

    const result = await window.retoneAPI.processLocalReasoning(text, model, agentName, {
      ...config,
      systemPrompt: config.systemPrompt || "",
    });

    const processingTimeMs = Date.now() - startTime;

    if (!result.success) {
      logger.logReasoning("LOCAL_ERROR", { model, processingTimeMs, error: result.error });
      throw buildLocalInferenceError(result);
    }

    logger.logReasoning("LOCAL_SUCCESS", {
      model,
      processingTimeMs,
      resultLength: result.text.length,
    });
    return result.text;
  },
};
