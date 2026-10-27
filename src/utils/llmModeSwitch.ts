import type { ResolvedLLMConfig } from "../stores/settingsStore";
import { isProviderValidForMode, reasoningModelBelongsToProvider } from "../models/ModelRegistry";
import type { InferenceMode } from "../types/nativeApi";

type LLMConfigPatch = Partial<Omit<ResolvedLLMConfig, "scope">>;

// Every mode shares the scope's provider and model, so a switch clears what the
// new mode can't use. In Providers that includes a model the kept provider
// doesn't serve.
export function modeSwitchPatch(
  current: Pick<ResolvedLLMConfig, "provider" | "model">,
  mode: InferenceMode
): LLMConfigPatch {
  const patch: LLMConfigPatch = { mode };
  if (!isProviderValidForMode(current.provider, mode)) {
    patch.provider = "";
    patch.model = "";
  } else if (
    mode === "providers" &&
    !reasoningModelBelongsToProvider(current.provider, current.model)
  ) {
    patch.model = "";
  }
  return patch;
}
