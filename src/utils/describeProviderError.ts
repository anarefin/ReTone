import type { TFunction } from "i18next";
import type { TechnicalErrorDetailsData } from "../components/ui/useToast";
import { SELF_HOSTED_NAME, isProviderSettingsTarget } from "../helpers/providerHttpErrors.js";

export type ProviderSettingsTarget = "llms";

interface DescribableError {
  message?: string;
  messageKey?: string;
  messageParams?: Record<string, string | number>;
  settingsTarget?: string;
  technicalDetails?: TechnicalErrorDetailsData;
}

export interface ProviderErrorDescription {
  description: string;
  technicalDetails?: TechnicalErrorDetailsData;
  settingsTarget?: ProviderSettingsTarget;
}

/** One rendering of a (possibly classified) error for every surface. */
export function describeProviderError(error: unknown, t: TFunction): ProviderErrorDescription {
  if (typeof error === "string") return { description: error };
  const source = (error ?? {}) as DescribableError;
  const description = source.messageKey
    ? String(t(source.messageKey, source.messageParams))
    : source.message || "";
  const settingsTarget = isProviderSettingsTarget(source.settingsTarget)
    ? (source.settingsTarget as ProviderSettingsTarget)
    : undefined;
  return {
    description,
    ...(source.technicalDetails ? { technicalDetails: source.technicalDetails } : {}),
    ...(settingsTarget ? { settingsTarget } : {}),
  };
}

export function formatProviderErrorDetails(
  details: TechnicalErrorDetailsData,
  t: TFunction
): string {
  const isProvider = Boolean(details.provider);
  const provider =
    details.provider === SELF_HOSTED_NAME
      ? t("settingsPage.aiModels.modes.selfHosted")
      : details.provider;
  return [
    provider ? `${t("providerErrors.details.provider")}: ${provider}` : "",
    details.status !== undefined
      ? `${t("reasoning.enterprise.technicalDetails.httpStatus")}: ${details.status}`
      : "",
    details.exceptionType
      ? `${t("reasoning.enterprise.technicalDetails.awsException")}: ${details.exceptionType}`
      : "",
    details.requestId
      ? `${t(
          isProvider
            ? "providerErrors.details.requestId"
            : "reasoning.enterprise.technicalDetails.awsRequestId"
        )}: ${details.requestId}`
      : "",
    details.underlyingError
      ? `${t("reasoning.enterprise.technicalDetails.underlyingError")}: ${details.underlyingError}`
      : "",
  ]
    .filter(Boolean)
    .join("\n");
}
