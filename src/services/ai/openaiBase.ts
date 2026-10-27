import { API_ENDPOINTS, ensureV1Suffix } from "../../config/constants";
import { isSecureHttpEndpoint } from "../../utils/urlUtils";
import logger from "../../utils/logger";
import i18n from "../../i18n";

// Never falls back: rerouting an invalid Custom endpoint to api.openai.com
// would send the prompt and the custom API key to a host the user never chose.
function invalidCustomEndpoint(reason: string, attempted?: string): never {
  logger.logReasoning("OPENAI_BASE_REJECTED", { reason, attempted });
  throw Object.assign(new Error(i18n.t("reasoning.custom.endpointInvalid")), {
    code: "CUSTOM_ENDPOINT_INVALID",
  });
}

export function resolveConfiguredOpenAIBase(provider: string, configuredBaseUrl?: string): string {
  if (provider !== "custom") return API_ENDPOINTS.OPENAI_BASE;

  const trimmed = configuredBaseUrl?.trim() ?? "";
  if (!trimmed) {
    return invalidCustomEndpoint("Custom endpoint is not configured");
  }

  const normalized = ensureV1Suffix(trimmed);
  if (!normalized) {
    return invalidCustomEndpoint("Custom endpoint could not be normalized", trimmed);
  }

  const knownNonOpenAIUrls = [
    "api.groq.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
  ];
  if (knownNonOpenAIUrls.some((url) => normalized.includes(url))) {
    return invalidCustomEndpoint("Custom URL is a known non-OpenAI provider", normalized);
  }

  if (!isSecureHttpEndpoint(normalized)) {
    return invalidCustomEndpoint(
      "HTTPS required (HTTP allowed for local network only)",
      normalized
    );
  }

  logger.logReasoning("CUSTOM_ENDPOINT_RESOLVED", {
    customEndpoint: normalized,
    isCustom: true,
    provider,
  });

  return normalized;
}
