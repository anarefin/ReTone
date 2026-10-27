import { useEffect, useState, type DragEvent, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Textarea } from "../ui/textarea";
import { Toggle } from "../ui/toggle";
import { ChevronDown, GripVertical, type IconComponent } from "../icons";
import {
  MAX_TONE_REWRITE_OPTION_NAME_LENGTH,
  MAX_TONE_REWRITE_OPTION_PROMPT_LENGTH,
} from "../../config/prompts/toneRewrite";

export interface ReToneOptionDraft {
  name: string;
  prompt: string;
}

interface ReToneOptionRowProps {
  optionId: string;
  kind: "builtin" | "custom";
  label: string;
  Icon: IconComponent;
  // The instruction the picker sends for this option today.
  prompt: string;
  // Built-ins only: the shipped instruction Reset restores.
  defaultPrompt?: string;
  enabled?: boolean;
  toggleDisabled?: boolean;
  expanded: boolean;
  isDraft?: boolean;
  isDragging?: boolean;
  // Where a dragged row would land relative to this one.
  dropPosition?: "before" | "after" | null;
  onExpandedChange: (expanded: boolean) => void;
  onEnabledChange?: (enabled: boolean) => void;
  onSave: (draft: ReToneOptionDraft) => void;
  onDelete?: () => void;
  onReset?: () => void;
  onMove?: (direction: -1 | 1) => void;
  onDragStart?: (event: DragEvent<HTMLElement>) => void;
  onDragEnd?: () => void;
  onDragOver?: (event: DragEvent<HTMLElement>) => void;
  onDrop?: (event: DragEvent<HTMLElement>) => void;
}

export default function ReToneOptionRow({
  optionId,
  kind,
  label,
  Icon,
  prompt,
  defaultPrompt,
  enabled = false,
  toggleDisabled = false,
  expanded,
  isDraft = false,
  isDragging = false,
  dropPosition = null,
  onExpandedChange,
  onEnabledChange,
  onSave,
  onDelete,
  onReset,
  onMove,
  onDragStart,
  onDragEnd,
  onDragOver,
  onDrop,
}: ReToneOptionRowProps) {
  const { t } = useTranslation();
  const [name, setName] = useState(kind === "custom" ? label : "");
  const [draftPrompt, setDraftPrompt] = useState(prompt);

  // Opening the editor starts from what is saved, never from an earlier
  // cancelled edit.
  useEffect(() => {
    if (!expanded) return;
    setName(kind === "custom" ? label : "");
    setDraftPrompt(prompt);
  }, [expanded, kind, label, prompt]);

  const trimmedName = name.trim();
  const trimmedPrompt = draftPrompt.trim();
  const canSave =
    !!trimmedPrompt &&
    trimmedPrompt.length <= MAX_TONE_REWRITE_OPTION_PROMPT_LENGTH &&
    (kind === "builtin" ||
      (!!trimmedName && trimmedName.length <= MAX_TONE_REWRITE_OPTION_NAME_LENGTH));
  const isEdited = kind === "builtin" && !!defaultPrompt && prompt !== defaultPrompt;
  const displayLabel = label || t("reTone.options.newOption");

  const onHandleKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (!event.altKey || !onMove) return;
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      onMove(event.key === "ArrowUp" ? -1 : 1);
    }
  };

  return (
    <div
      data-retone-option={optionId}
      onDragOver={onDragOver}
      onDrop={onDrop}
      className={`${isDragging ? "opacity-50" : ""} ${
        dropPosition === "before"
          ? "shadow-[0_-2px_0_0_var(--color-primary)]"
          : dropPosition === "after"
            ? "shadow-[0_2px_0_0_var(--color-primary)]"
            : ""
      }`}
    >
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2 text-sm">
          {isDraft ? (
            <span className="size-5 shrink-0" aria-hidden="true" />
          ) : (
            <button
              type="button"
              draggable
              data-retone-handle={optionId}
              onDragStart={onDragStart}
              onDragEnd={onDragEnd}
              onKeyDown={onHandleKeyDown}
              title={t("reTone.options.dragHandle", { name: displayLabel })}
              aria-label={t("reTone.options.dragHandle", { name: displayLabel })}
              className="flex size-5 shrink-0 cursor-grab items-center justify-center rounded text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-primary/40 focus-visible:outline-none active:cursor-grabbing"
            >
              <GripVertical className="size-3.5" />
            </button>
          )}
          <Icon className="size-4 shrink-0 text-muted-foreground" />
          <span className="truncate">{displayLabel}</span>
          {kind === "custom" && !isDraft && (
            <span className="shrink-0 rounded bg-surface-2 px-1.5 py-0.5 text-[10px] text-muted-foreground">
              {t("reTone.options.customBadge")}
            </span>
          )}
          {isEdited && (
            <span className="shrink-0 rounded bg-surface-2 px-1.5 py-0.5 text-[10px] text-muted-foreground">
              {t("reTone.options.editedBadge")}
            </span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {!isDraft && (
            <button
              type="button"
              onClick={() => onExpandedChange(!expanded)}
              aria-expanded={expanded}
              title={t("reTone.options.edit", { name: displayLabel })}
              aria-label={t("reTone.options.edit", { name: displayLabel })}
              className="rounded p-1 text-muted-foreground hover:bg-surface-2 hover:text-foreground"
            >
              <ChevronDown
                className={`size-4 transition-transform ${expanded ? "rotate-180" : ""}`}
              />
            </button>
          )}
          {!isDraft && onEnabledChange && (
            <Toggle
              checked={enabled}
              disabled={toggleDisabled}
              onChange={onEnabledChange}
              ariaLabel={displayLabel}
            />
          )}
        </div>
      </div>

      {expanded && (
        <div className="mt-3 space-y-3">
          {kind === "custom" && (
            <div className="space-y-1.5">
              <label
                htmlFor={`retone-name-${optionId}`}
                className="text-xs font-medium text-muted-foreground"
              >
                {t("reTone.options.name")}
              </label>
              <Input
                id={`retone-name-${optionId}`}
                dir="auto"
                value={name}
                maxLength={MAX_TONE_REWRITE_OPTION_NAME_LENGTH}
                placeholder={t("reTone.options.namePlaceholder")}
                onChange={(event) => setName(event.target.value)}
                autoFocus={isDraft}
              />
            </div>
          )}
          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <label
                htmlFor={`retone-prompt-${optionId}`}
                className="text-xs font-medium text-muted-foreground"
              >
                {t("reTone.options.prompt")}
              </label>
              <span className="text-[11px] text-muted-foreground tabular-nums">
                {t("reTone.options.characterCount", {
                  count: draftPrompt.length,
                  max: MAX_TONE_REWRITE_OPTION_PROMPT_LENGTH,
                })}
              </span>
            </div>
            <Textarea
              id={`retone-prompt-${optionId}`}
              dir="auto"
              value={draftPrompt}
              maxLength={MAX_TONE_REWRITE_OPTION_PROMPT_LENGTH}
              rows={4}
              placeholder={t("reTone.options.promptPlaceholder")}
              onChange={(event) => setDraftPrompt(event.target.value)}
            />
            <p className="text-xs text-muted-foreground">{t("reTone.options.promptHint")}</p>
          </div>
          <div className="flex items-center justify-between gap-2">
            <div>
              {kind === "custom" && onDelete && (
                <Button type="button" variant="ghost" size="sm" onClick={onDelete}>
                  {t("reTone.options.delete")}
                </Button>
              )}
              {kind === "builtin" && onReset && (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  disabled={!isEdited && draftPrompt === defaultPrompt}
                  onClick={onReset}
                >
                  {t("reTone.options.reset")}
                </Button>
              )}
            </div>
            <div className="flex items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => onExpandedChange(false)}
              >
                {t("reTone.options.cancel")}
              </Button>
              <Button
                type="button"
                size="sm"
                disabled={!canSave}
                onClick={() => onSave({ name: trimmedName, prompt: trimmedPrompt })}
              >
                {t("reTone.options.save")}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
