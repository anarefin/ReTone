import { useState, type DragEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { SectionHeader, SettingsPanel, SettingsPanelRow, SettingsRow } from "../ui/SettingsSection";
import { Toggle } from "../ui/toggle";
import { Button } from "../ui/button";
import { ConfirmDialog } from "../ui/dialog";
import { Plus } from "../icons";
import { useDialogs } from "../../hooks/useDialogs";
import { useSettingsStore } from "../../stores/settingsStore";
import {
  MAX_ENABLED_TONE_REWRITE_OPTIONS,
  createCustomToneRewriteOptionId,
  getDefaultToneRewriteInstruction,
  isToneRewriteToneId,
  toneRewriteInstruction,
  type ToneRewriteOption,
} from "../../config/prompts/toneRewrite";
import {
  CUSTOM_TONE_ICON,
  toneRewriteOptionIcon,
  toneRewriteOptionLabel,
} from "../toneRewriteIcons";
import InferenceConfigEditor from "./InferenceConfigEditor";
import ReToneOptionRow, { type ReToneOptionDraft } from "./ReToneOptionRow";

interface ReToneSectionProps {
  // The hotkey input, owned by SettingsWindow with its registration handling.
  hotkeyInput: ReactNode;
}

const NEW_OPTION_ID = "new";

// Above this size a local model is noticeably slow for a quick rewrite.
const FAST_LOCAL_MODEL_MAX_BILLIONS = 4;

function localModelBillions(modelId: string): number | null {
  const match = modelId.match(/(\d+(?:\.\d+)?)b(?![a-z])/i);
  return match ? Number(match[1]) : null;
}

// Where a drop lands: before the row under the pointer, or after it when the
// pointer is in its lower half.
function dropIndexFor(event: DragEvent<HTMLElement>, index: number): number {
  const rect = event.currentTarget.getBoundingClientRect();
  return event.clientY > rect.top + rect.height / 2 ? index + 1 : index;
}

export default function ReToneSection({ hotkeyInput }: ReToneSectionProps) {
  const { t } = useTranslation();
  const options = useSettingsStore((s) => s.toneRewriteOptions);
  const setOptions = useSettingsStore((s) => s.setToneRewriteOptions);
  const livePreview = useSettingsStore((s) => s.toneRewriteLivePreview);
  const setLivePreview = useSettingsStore((s) => s.setToneRewriteLivePreview);
  const wholeField = useSettingsStore((s) => s.toneRewriteWholeField);
  const setWholeField = useSettingsStore((s) => s.setToneRewriteWholeField);
  const mode = useSettingsStore((s) => s.toneRewriteMode);
  const model = useSettingsStore((s) => s.toneRewriteModel);
  const modelBillions = mode === "local" ? localModelBillions(model) : null;
  const showSpeedHint = modelBillions !== null && modelBillions > FAST_LOCAL_MODEL_MAX_BILLIONS;
  const { confirmDialog, showConfirmDialog, hideConfirmDialog } = useDialogs();
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);

  const enabledCount = options.filter((option) => option.enabled).length;
  const atMax = enabledCount >= MAX_ENABLED_TONE_REWRITE_OPTIONS;

  const update = (id: string, patch: Partial<ToneRewriteOption>) =>
    setOptions(options.map((option) => (option.id === id ? { ...option, ...patch } : option)));

  const moveTo = (id: string, targetIndex: number) => {
    const from = options.findIndex((option) => option.id === id);
    if (from < 0) return;
    const next = [...options];
    const [moved] = next.splice(from, 1);
    // Removing the row shifts every later index down by one.
    const to = Math.max(
      0,
      Math.min(next.length, targetIndex > from ? targetIndex - 1 : targetIndex)
    );
    if (to === from) return;
    next.splice(to, 0, moved);
    setOptions(next);
  };

  // Alt+Up/Down on a handle; focus follows the row it moved.
  const moveBy = (id: string, direction: -1 | 1) => {
    const from = options.findIndex((option) => option.id === id);
    moveTo(id, direction < 0 ? from - 1 : from + 2);
    requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(`[data-retone-handle="${CSS.escape(id)}"]`)?.focus();
    });
  };

  const save = (option: ToneRewriteOption, draft: ReToneOptionDraft) => {
    if (option.kind === "custom") {
      update(option.id, { name: draft.name, prompt: draft.prompt });
    } else {
      update(option.id, { prompt: draft.prompt });
    }
    setExpandedId(null);
  };

  const addOption = (draft: ReToneOptionDraft) => {
    // A new option starts on unless the picker is already full.
    setOptions([
      ...options,
      {
        id: createCustomToneRewriteOptionId(),
        kind: "custom",
        enabled: !atMax,
        name: draft.name,
        prompt: draft.prompt,
      },
    ]);
    setExpandedId(null);
  };

  const confirmDelete = (option: ToneRewriteOption) =>
    showConfirmDialog({
      title: t("reTone.options.deleteConfirmTitle", { name: option.name }),
      description: t("reTone.options.deleteConfirmDescription"),
      confirmText: t("reTone.options.delete"),
      variant: "destructive",
      onConfirm: () => {
        setOptions(options.filter((entry) => entry.id !== option.id));
        setExpandedId(null);
      },
    });

  const endDrag = () => {
    setDragId(null);
    setDropIndex(null);
  };

  return (
    <div className="space-y-6">
      <div className="space-y-4">
        <SectionHeader
          title={t("reTone.model.title")}
          description={t("reTone.model.description")}
        />
        <InferenceConfigEditor scope="toneRewrite" />
        {showSpeedHint && (
          <p className="text-xs text-muted-foreground">{t("reTone.model.speedHint")}</p>
        )}
      </div>

      <div>
        <SectionHeader
          title={t("reTone.picker.title")}
          description={t("reTone.picker.description")}
        />
        <SettingsPanel>
          <SettingsPanelRow>
            <SettingsRow
              label={t("reTone.picker.livePreview")}
              description={t("reTone.picker.livePreviewDescription")}
            >
              <Toggle checked={livePreview} onChange={setLivePreview} />
            </SettingsRow>
          </SettingsPanelRow>
          <SettingsPanelRow>
            <SettingsRow
              label={t("reTone.picker.wholeField")}
              description={t("reTone.picker.wholeFieldDescription")}
            >
              <Toggle checked={wholeField} onChange={setWholeField} />
            </SettingsRow>
          </SettingsPanelRow>
        </SettingsPanel>
      </div>

      <div>
        <SectionHeader
          title={t("reTone.hotkey.title")}
          description={t("reTone.hotkey.description")}
        />
        <SettingsPanel>
          <SettingsPanelRow>{hotkeyInput}</SettingsPanelRow>
        </SettingsPanel>
      </div>

      <div>
        <SectionHeader
          title={t("reTone.options.title")}
          description={t("reTone.options.description")}
        />
        <SettingsPanel>
          {options.map((option, index) => {
            const isLastEnabled = option.enabled && enabledCount === 1;
            const blockedByMax = !option.enabled && atMax;
            return (
              <SettingsPanelRow key={option.id}>
                <ReToneOptionRow
                  optionId={option.id}
                  kind={option.kind}
                  label={toneRewriteOptionLabel(option, t)}
                  Icon={toneRewriteOptionIcon(option)}
                  prompt={toneRewriteInstruction(option)}
                  defaultPrompt={
                    option.kind === "builtin" && isToneRewriteToneId(option.id)
                      ? getDefaultToneRewriteInstruction(option.id)
                      : undefined
                  }
                  enabled={option.enabled}
                  toggleDisabled={isLastEnabled || blockedByMax}
                  expanded={expandedId === option.id}
                  isDragging={dragId === option.id}
                  dropPosition={
                    dragId === null
                      ? null
                      : dropIndex === index
                        ? "before"
                        : dropIndex === options.length && index === options.length - 1
                          ? "after"
                          : null
                  }
                  onExpandedChange={(next) => setExpandedId(next ? option.id : null)}
                  onEnabledChange={(next) => update(option.id, { enabled: next })}
                  onSave={(draft) => save(option, draft)}
                  onDelete={option.kind === "custom" ? () => confirmDelete(option) : undefined}
                  onReset={
                    option.kind === "builtin"
                      ? () => {
                          update(option.id, { prompt: undefined });
                          setExpandedId(null);
                        }
                      : undefined
                  }
                  onMove={(direction) => moveBy(option.id, direction)}
                  onDragStart={(event) => {
                    event.dataTransfer.effectAllowed = "move";
                    event.dataTransfer.setData("text/plain", option.id);
                    const row = event.currentTarget.closest("[data-retone-option]");
                    if (row) event.dataTransfer.setDragImage(row, 16, 16);
                    setDragId(option.id);
                  }}
                  onDragEnd={endDrag}
                  onDragOver={(event) => {
                    if (!dragId) return;
                    event.preventDefault();
                    event.dataTransfer.dropEffect = "move";
                    setDropIndex(dropIndexFor(event, index));
                  }}
                  onDrop={(event) => {
                    if (!dragId) return;
                    event.preventDefault();
                    moveTo(dragId, dropIndexFor(event, index));
                    endDrag();
                  }}
                />
              </SettingsPanelRow>
            );
          })}
          {expandedId === NEW_OPTION_ID && (
            <SettingsPanelRow>
              <ReToneOptionRow
                optionId={NEW_OPTION_ID}
                kind="custom"
                label=""
                Icon={CUSTOM_TONE_ICON}
                prompt=""
                expanded
                isDraft
                onExpandedChange={(next) => !next && setExpandedId(null)}
                onSave={addOption}
              />
            </SettingsPanelRow>
          )}
        </SettingsPanel>
        <div className="mt-2 flex items-start justify-between gap-4">
          <p className="text-xs text-muted-foreground">
            {atMax
              ? t("reTone.options.maxEnabled", { max: MAX_ENABLED_TONE_REWRITE_OPTIONS })
              : t("reTone.options.atLeastOne")}
          </p>
          {expandedId !== NEW_OPTION_ID && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setExpandedId(NEW_OPTION_ID)}
            >
              <Plus className="size-3.5" />
              {t("reTone.options.add")}
            </Button>
          )}
        </div>
      </div>

      <ConfirmDialog
        open={confirmDialog.open}
        onOpenChange={(open) => !open && hideConfirmDialog()}
        title={confirmDialog.title}
        description={confirmDialog.description}
        confirmText={confirmDialog.confirmText}
        cancelText={confirmDialog.cancelText}
        onConfirm={confirmDialog.onConfirm}
        variant={confirmDialog.variant}
      />
    </div>
  );
}
