import { useTranslation } from "react-i18next";
import { SectionHeader } from "../ui/SettingsSection";
import { useSettingsStore } from "../../stores/settingsStore";
import InferenceConfigEditor from "./InferenceConfigEditor";

// Above this size a local model is noticeably slow for a quick rewrite.
const FAST_LOCAL_MODEL_MAX_BILLIONS = 4;

function localModelBillions(modelId: string): number | null {
  const match = modelId.match(/(\d+(?:\.\d+)?)b(?![a-z])/i);
  return match ? Number(match[1]) : null;
}

export default function ReToneModelSection() {
  const { t } = useTranslation();
  const mode = useSettingsStore((s) => s.toneRewriteMode);
  const model = useSettingsStore((s) => s.toneRewriteModel);
  const modelBillions = mode === "local" ? localModelBillions(model) : null;
  const showSpeedHint = modelBillions !== null && modelBillions > FAST_LOCAL_MODEL_MAX_BILLIONS;

  return (
    <div className="space-y-4">
      <SectionHeader title={t("reTone.model.title")} description={t("reTone.model.description")} />
      <InferenceConfigEditor scope="toneRewrite" />
      {showSpeedHint && (
        <p className="text-xs text-muted-foreground">{t("reTone.model.speedHint")}</p>
      )}
    </div>
  );
}
