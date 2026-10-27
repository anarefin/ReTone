import { useCallback } from "react";
import { useShallow } from "zustand/react/shallow";
import { useTranslation } from "react-i18next";
import { Key, Cpu } from "../icons";
import {
  useSettingsStore,
  selectResolvedLLMConfig,
  setResolvedLLMConfig,
  type ResolvedLLMConfig,
} from "../../stores/settingsStore";
import { InferenceModeSelector } from "../ui/SettingsSection";
import type { InferenceModeOption } from "../ui/SettingsSection";
import ReasoningModelSelector from "../ReasoningModelSelector";
import { Toggle } from "../ui/toggle";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import type { InferenceMode } from "../../types/nativeApi";
import type { InferenceScope } from "../../config/inferenceScopes";
import { AUTO_REASONING_EFFORT, isReasoningEffortSupported } from "../../models/reasoningEffort";
import { selectableReasoningEffort } from "../../services/ai/chatRequestBody";
import { offersThinkingToggle } from "../../services/ai/thinkingSuppression";
import { modeSwitchPatch } from "../../utils/llmModeSwitch";

// Radix Select can't hold an empty value, so Auto ("") gets a sentinel.
const AUTO_REASONING_EFFORT_VALUE = "__auto__";

interface InferenceConfigEditorProps {
  scope: InferenceScope;
}

export default function InferenceConfigEditor({ scope }: InferenceConfigEditorProps) {
  const { t } = useTranslation();
  const config = useSettingsStore(
    useShallow((settings): ResolvedLLMConfig => selectResolvedLLMConfig(settings, scope))
  );
  const keepLocalModelLoaded = useSettingsStore((s) => s.keepLocalModelLoaded);
  const setKeepLocalModelLoaded = useSettingsStore((s) => s.setKeepLocalModelLoaded);

  const modes: InferenceModeOption[] = [
    {
      id: "local",
      label: t("settingsPage.aiModels.modes.local"),
      description: t("settingsPage.aiModels.modes.localDesc"),
      icon: <Cpu className="w-4 h-4" />,
    },
    {
      id: "providers",
      label: t("settingsPage.aiModels.modes.providers"),
      description: t("settingsPage.aiModels.modes.providersDesc"),
      icon: <Key className="w-4 h-4" />,
    },
  ];
  const effectiveMode: InferenceMode = config.mode === "providers" ? "providers" : "local";

  const setField = useCallback(
    <K extends keyof Omit<typeof config, "scope">>(field: K) =>
      (value: NonNullable<(typeof config)[K]>) => {
        setResolvedLLMConfig(scope, { [field]: value });
      },
    [scope]
  );

  const handleModeSelect = useCallback(
    (mode: InferenceMode) => {
      if (mode === effectiveMode) return;
      setResolvedLLMConfig(
        scope,
        modeSwitchPatch({ provider: config.provider, model: config.model }, mode)
      );
    },
    [scope, config.provider, config.model, effectiveMode]
  );

  const showThinkingToggle = offersThinkingToggle(effectiveMode, config.provider, config.model);

  const effortCapability =
    effectiveMode === "providers"
      ? selectableReasoningEffort(config.provider, config.model, config, config.cloudBaseUrl)
      : null;

  return (
    <div className="space-y-3">
      <InferenceModeSelector modes={modes} activeMode={effectiveMode} onSelect={handleModeSelect} />

      <ReasoningModelSelector
        reasoningModel={config.model}
        setReasoningModel={setField("model")}
        localReasoningProvider={config.provider}
        setLocalReasoningProvider={setField("provider")}
        cloudReasoningBaseUrl={config.cloudBaseUrl ?? ""}
        setCloudReasoningBaseUrl={setField("cloudBaseUrl")}
        customReasoningApiKey={config.customApiKey ?? ""}
        setCustomReasoningApiKey={setField("customApiKey")}
        mode={effectiveMode === "local" ? "local" : "cloud"}
      />

      {showThinkingToggle && (
        <div className="flex items-start justify-between gap-3 pt-1">
          <div className="flex-1 min-w-0">
            <h4 className="text-sm font-medium text-foreground">
              {t("reasoning.disableThinking.label")}
            </h4>
            <p className="text-xs text-muted-foreground">{t("reasoning.disableThinking.help")}</p>
          </div>
          <Toggle checked={config.disableThinking} onChange={setField("disableThinking")} />
        </div>
      )}

      {effortCapability && (
        <div className="flex items-start justify-between gap-3 pt-1">
          <div className="flex-1 min-w-0">
            <h4 className="text-sm font-medium text-foreground">{t("reasoning.effort.title")}</h4>
            <p className="text-xs text-muted-foreground">
              {effortCapability.defaultValue
                ? t("reasoning.effort.descriptionWithDefault", {
                    value: t(`reasoning.effort.options.${effortCapability.defaultValue}`),
                  })
                : t("reasoning.effort.description")}
            </p>
          </div>
          <Select
            value={
              isReasoningEffortSupported(effortCapability, config.reasoningEffort)
                ? config.reasoningEffort
                : AUTO_REASONING_EFFORT_VALUE
            }
            onValueChange={(value) =>
              setField("reasoningEffort")(
                value === AUTO_REASONING_EFFORT_VALUE ? AUTO_REASONING_EFFORT : value
              )
            }
          >
            <SelectTrigger className="h-7 w-44 shrink-0 text-xs rounded-lg px-2.5 [&>svg]:h-3 [&>svg]:w-3">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                value={AUTO_REASONING_EFFORT_VALUE}
                className="text-xs py-1.5 ps-2.5 pe-7 rounded-md"
              >
                {t("reasoning.effort.auto")}
              </SelectItem>
              {effortCapability.options.map((effort) => (
                <SelectItem
                  key={effort}
                  value={effort}
                  className="text-xs py-1.5 ps-2.5 pe-7 rounded-md"
                >
                  {t(`reasoning.effort.options.${effort}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}

      {effectiveMode === "local" && (
        <div className="flex items-start justify-between gap-3 pt-1">
          <div className="flex-1 min-w-0">
            <h4 className="text-sm font-medium text-foreground">
              {t("reasoning.keepModelLoaded.label")}
            </h4>
            <p className="text-xs text-muted-foreground">{t("reasoning.keepModelLoaded.help")}</p>
          </div>
          <Toggle checked={keepLocalModelLoaded} onChange={setKeepLocalModelLoaded} />
        </div>
      )}
    </div>
  );
}
