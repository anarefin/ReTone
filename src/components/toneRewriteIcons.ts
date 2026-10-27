import {
  AlignLeft,
  Building2,
  Check,
  Keyboard,
  Languages,
  MessageCircle,
  Smile,
  Sparkles,
} from "./icons";
import {
  isToneRewriteToneId,
  type ToneRewriteOption,
  type ToneRewriteToneId,
} from "../config/prompts/toneRewrite";

// Shared by the tone picker and Settings → ReTone.
export const TONE_ICONS: Record<ToneRewriteToneId, typeof Check> = {
  professional: Building2,
  casual: MessageCircle,
  friendly: Smile,
  concise: AlignLeft,
  fixGrammar: Check,
  banglishToEnglish: Languages,
  banglishToBangla: Keyboard,
};

// Every custom option shares one icon.
export const CUSTOM_TONE_ICON = Sparkles;

export function toneRewriteOptionIcon(option: ToneRewriteOption): typeof Check {
  return option.kind === "builtin" && isToneRewriteToneId(option.id)
    ? TONE_ICONS[option.id]
    : CUSTOM_TONE_ICON;
}

// A built-in's label follows the UI language; a custom option keeps the
// user's own name.
export function toneRewriteOptionLabel(
  option: ToneRewriteOption,
  t: (key: string) => string
): string {
  return option.kind === "builtin" ? t(`toneRewrite.tones.${option.id}`) : option.name || "";
}
