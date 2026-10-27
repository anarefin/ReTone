import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, X } from "./icons";
import { toneRewriteOptionIcon, toneRewriteOptionLabel } from "./toneRewriteIcons";
import {
  enabledToneRewriteOptions,
  previewToneRewriteOutput,
  type ToneRewriteOption,
} from "../config/prompts/toneRewrite";
import {
  canStreamToneRewrite,
  rewriteInTone,
  rewriteInToneStreaming,
  ToneRewriteError,
  warmToneRewriteModel,
  type ToneRewriteRequest,
} from "../helpers/toneRewrite";
import type { ToneRewritePickerPayload } from "../types/nativeApi";
import { useSettingsStore } from "../stores/settingsStore";
import { describeProviderError } from "../utils/describeProviderError";
import logger from "../utils/logger";

type PickerState =
  | { kind: "loading" }
  | { kind: "choose" }
  | {
      kind: "rewriting";
      optionId: string;
      phase: "loading-model" | "generating";
      // null while the rewrite runs without a live preview.
      preview: string | null;
    }
  | { kind: "error"; messageKey: string; params?: Record<string, unknown>; message?: string };

const CAPTURE_ERROR_KEYS: Record<string, string> = {
  nothing_selected: "toneRewrite.errors.nothingSelected",
  no_text: "toneRewrite.errors.noText",
  too_large: "toneRewrite.errors.tooLarge",
  target_changed: "toneRewrite.errors.targetChanged",
  modifiers_held: "toneRewrite.errors.modifiersHeld",
  accessibility_unavailable: "toneRewrite.errors.accessibility",
};

const PASTE_ERROR_KEYS: Record<string, string> = {
  selection_changed: "toneRewrite.errors.selectionChanged",
  target_changed: "toneRewrite.errors.selectionChanged",
  session_expired: "toneRewrite.errors.selectionChanged",
  modifiers_held: "toneRewrite.errors.modifiersHeld",
};

// The window is created once and shown again for every press: main pushes each
// open's payload ("tone-rewrite-show") and resets the picker when it hides.
export default function ToneRewritePickerOverlay(): ReactElement {
  const { t } = useTranslation();
  const [payload, setPayload] = useState<ToneRewritePickerPayload | null>(null);
  const [state, setState] = useState<PickerState>({ kind: "loading" });
  // Bumped on every open, so the height is reported even when it didn't change.
  const [openCount, setOpenCount] = useState(0);
  // The option Enter runs; arrow keys and hover move it.
  const [activeIndex, setActiveIndex] = useState(0);
  const requestRef = useRef<ToneRewriteRequest | null>(null);
  const warmRef = useRef<{ promise: Promise<void>; done: boolean } | null>(null);
  const shownRef = useRef(false);
  const allOptions = useSettingsStore((s) => s.toneRewriteOptions);
  const livePreview = useSettingsStore((s) => s.toneRewriteLivePreview);
  const options = useMemo(() => enabledToneRewriteOptions(allOptions), [allOptions]);
  const cardRef = useRef<HTMLDivElement | null>(null);
  const previewRef = useRef<HTMLDivElement | null>(null);

  const cancelRequest = useCallback(() => {
    requestRef.current?.cancel();
    requestRef.current = null;
  }, []);

  const close = useCallback(() => {
    cancelRequest();
    void window.retoneAPI?.toneRewriteClose?.();
  }, [cancelRequest]);

  const open = useCallback(
    (next: ToneRewritePickerPayload) => {
      cancelRequest();
      setPayload(next);
      setOpenCount((count) => count + 1);
      setActiveIndex(0);
      if (next.state !== "ready") {
        setState({
          kind: "error",
          messageKey: CAPTURE_ERROR_KEYS[next.code] || "toneRewrite.errors.unavailable",
          params: { max: next.maxCharacters },
        });
        return;
      }
      setState({ kind: "choose" });
      // The window is reused, so a button focused last time could keep the
      // keys; hand them to the document for the arrow/Enter handler.
      (document.activeElement as HTMLElement | null)?.blur?.();
      window.focus();
      // Load the model while the user is still choosing.
      const startedAt = performance.now();
      const warm = { promise: warmToneRewriteModel(), done: false };
      warmRef.current = warm;
      void warm.promise.then(() => {
        warm.done = true;
        void logger.debug(
          "Tone rewrite model warm",
          { warmMs: Math.round(performance.now() - startedAt) },
          "tone-rewrite"
        );
      });
    },
    [cancelRequest]
  );

  useEffect(() => {
    let active = true;
    const offShow = window.retoneAPI?.onToneRewriteShow?.((next) => {
      shownRef.current = true;
      open(next);
    });
    // Main may keep the window up while it hands focus back to the target, so
    // the view stays as it was until the next open replaces it; clearing it
    // here would flash the option list.
    const offReset = window.retoneAPI?.onToneRewriteReset?.(() => {
      cancelRequest();
    });
    // A window created by the press itself may mount after main pushed the
    // payload; pull it once in case the push was missed.
    void window.retoneAPI?.toneRewriteGetPayload?.().then((next) => {
      if (!active || !next || shownRef.current) return;
      open(next);
    });
    return () => {
      active = false;
      offShow?.();
      offReset?.();
    };
  }, [open, cancelRequest]);

  const pick = useCallback(
    async (option: ToneRewriteOption) => {
      if (payload?.state !== "ready" || state.kind !== "choose") return;
      const streaming = livePreview && canStreamToneRewrite();
      const warm = warmRef.current;
      const modelLoading = !!warm && !warm.done;
      setState({
        kind: "rewriting",
        optionId: option.id,
        phase: modelLoading ? "loading-model" : "generating",
        preview: streaming ? "" : null,
      });
      const startedAt = performance.now();
      let firstTokenLogged = false;
      const updateRewriting = (patch: { phase?: "generating"; preview?: string }) =>
        setState((prev) =>
          prev.kind === "rewriting" && prev.optionId === option.id ? { ...prev, ...patch } : prev
        );
      if (modelLoading) {
        void warm.promise.then(() => {
          if (requestRef.current === request) updateRewriting({ phase: "generating" });
        });
      }

      const request = streaming
        ? rewriteInToneStreaming(payload.text, option, (soFar) => {
            if (requestRef.current !== request) return;
            if (!firstTokenLogged) {
              firstTokenLogged = true;
              void logger.debug(
                "Tone rewrite first token",
                { firstTokenMs: Math.round(performance.now() - startedAt) },
                "tone-rewrite"
              );
            }
            updateRewriting({ phase: "generating", preview: previewToneRewriteOutput(soFar) });
          })
        : rewriteInTone(payload.text, option);
      requestRef.current = request;
      let rewritten: string;
      try {
        rewritten = await request.promise;
      } catch (error) {
        if (requestRef.current !== request) return;
        requestRef.current = null;
        const { message, code, messageKey } = (error ?? {}) as {
          message?: string;
          code?: string;
          messageKey?: string;
        };
        void logger.warn("Tone rewrite failed", { message, code, messageKey }, "tone-rewrite");
        if (error instanceof ToneRewriteError) {
          setState({
            kind: "error",
            messageKey:
              error.code === "not_configured"
                ? "toneRewrite.errors.notConfigured"
                : error.code === "empty_output"
                  ? "toneRewrite.errors.emptyOutput"
                  : "toneRewrite.errors.rewriteFailed",
          });
          return;
        }
        // Local and provider failures arrive classified (model busy, server
        // down, bad API key, …); say which rather than a bare "failed".
        const classified = (error as { messageKey?: string })?.messageKey
          ? describeProviderError(error, t).description
          : "";
        setState({
          kind: "error",
          messageKey: "toneRewrite.errors.rewriteFailed",
          message: classified || undefined,
        });
        return;
      }
      if (requestRef.current !== request) return;
      requestRef.current = null;
      void logger.debug(
        "Tone rewrite done",
        { totalMs: Math.round(performance.now() - startedAt), streaming },
        "tone-rewrite"
      );
      const result = await window.retoneAPI?.toneRewriteApply?.(payload.sessionId, rewritten);
      if (result?.success) return;
      setState({
        kind: "error",
        messageKey:
          (result?.code && PASTE_ERROR_KEYS[result.code]) || "toneRewrite.errors.pasteFailed",
        params: { copied: result?.copied },
      });
    },
    [payload, state.kind, livePreview, t]
  );

  // The window fits the card, so the picker grows with the number of options
  // and the streamed preview; main clamps it to the work area and the list
  // scrolls past that.
  useEffect(() => {
    const card = cardRef.current;
    if (!card || typeof ResizeObserver === "undefined") return;
    const report = () => {
      const list = card.querySelector("ul");
      const overflow = list ? list.scrollHeight - list.clientHeight : 0;
      // p-2 around the card.
      void window.retoneAPI?.toneRewriteSetHeight?.(card.offsetHeight + overflow + 16);
    };
    const observer = new ResizeObserver(report);
    observer.observe(card);
    report();
    return () => observer.disconnect();
  }, [options.length, state.kind, openCount]);

  const previewText = state.kind === "rewriting" ? state.preview : null;
  useEffect(() => {
    const box = previewRef.current;
    if (box) box.scrollTop = box.scrollHeight;
  }, [previewText]);

  useEffect(() => {
    if (activeIndex >= options.length) setActiveIndex(0);
  }, [activeIndex, options.length]);

  useEffect(() => {
    cardRef.current?.querySelector('[data-active="true"]')?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close();
        return;
      }
      if (state.kind === "choose" && options.length > 0) {
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          const step = event.key === "ArrowDown" ? 1 : -1;
          setActiveIndex((current) => (current + step + options.length) % options.length);
          return;
        }
        // Also keeps a focused option button from firing its own click.
        if (event.key === "Enter") {
          event.preventDefault();
          const option = options[activeIndex];
          if (option) void pick(option);
          return;
        }
      }
      const index = Number(event.key) - 1;
      if (Number.isInteger(index) && index >= 0 && index < options.length) {
        event.preventDefault();
        void pick(options[index]);
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [close, pick, options, activeIndex, state.kind]);

  const copied = state.kind === "error" && state.params?.copied === true;
  const rewriting = state.kind === "rewriting" ? state : null;
  // A live preview takes the list's place, leaving only the picked option.
  const visibleOptions =
    rewriting?.preview != null
      ? options.filter((option) => option.id === rewriting.optionId)
      : options;

  return (
    <div className="tone-rewrite-window h-screen w-screen bg-transparent p-2 select-none">
      <div
        ref={cardRef}
        className="flex max-h-full flex-col overflow-hidden rounded-xl border border-border bg-popover text-foreground shadow-lg"
      >
        <div className="flex items-center justify-between px-3 pt-2.5 pb-1.5">
          <span className="text-xs font-medium text-muted-foreground">
            {payload?.state === "ready" && payload.selectedAll
              ? t("toneRewrite.titleWholeField")
              : t("toneRewrite.title")}
          </span>
          <button
            type="button"
            onClick={close}
            title={t("toneRewrite.close")}
            aria-label={t("toneRewrite.close")}
            className="rounded p-0.5 text-muted-foreground hover:bg-surface-2 hover:text-foreground"
          >
            <X className="size-3.5" />
          </button>
        </div>

        {state.kind === "error" ? (
          <div
            className="flex flex-1 flex-col justify-center gap-1.5 px-3 pb-3 text-sm"
            role="alert"
          >
            <p>{state.message || t(state.messageKey, state.params)}</p>
            {copied && (
              <p className="text-xs text-muted-foreground">{t("toneRewrite.copiedToClipboard")}</p>
            )}
          </div>
        ) : (
          <ul className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto px-1.5 pb-1.5">
            {visibleOptions.map((option, index) => {
              const Icon = toneRewriteOptionIcon(option);
              const isRewriting = rewriting?.optionId === option.id;
              const isActive = state.kind === "choose" && index === activeIndex;
              const disabled = state.kind !== "choose";
              return (
                <li key={option.id}>
                  <button
                    type="button"
                    disabled={disabled}
                    data-active={isActive}
                    aria-current={isActive || undefined}
                    onMouseEnter={() => setActiveIndex(index)}
                    onClick={() => void pick(option)}
                    // surface-2 matches the popover in light mode, so the
                    // selection needs a tint of its own to show.
                    className={`flex w-full items-center gap-2.5 rounded-lg px-2 py-2 text-left text-sm focus-visible:outline-none disabled:cursor-default ${
                      isRewriting
                        ? "bg-foreground/[0.07]"
                        : isActive
                          ? "bg-foreground/[0.07] disabled:opacity-50"
                          : "disabled:opacity-50"
                    }`}
                  >
                    {isRewriting ? (
                      <Loader2 className="size-4 shrink-0 animate-spin" />
                    ) : (
                      <Icon
                        className={`size-4 shrink-0 ${isActive ? "text-foreground" : "text-muted-foreground"}`}
                      />
                    )}
                    <span className="flex-1 truncate">{toneRewriteOptionLabel(option, t)}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}

        {rewriting && rewriting.preview != null && rewriting.preview !== "" && (
          <div
            ref={previewRef}
            className="mx-1.5 mb-1.5 max-h-48 overflow-y-auto rounded-lg border border-border/70 bg-surface-1 px-2.5 py-2 text-sm whitespace-pre-wrap select-text"
            aria-live="polite"
            aria-label={t("toneRewrite.preview.label")}
          >
            {rewriting.preview}
            <span
              aria-hidden="true"
              className="ml-0.5 inline-block h-[1em] w-[2px] translate-y-[2px] bg-foreground/70"
              style={{ animation: "agent-cursor-blink 1s ease-in-out infinite" }}
            />
          </div>
        )}

        {rewriting && (rewriting.preview == null || rewriting.preview === "") && (
          <div className="px-3 pb-2.5" aria-live="polite">
            <div className="mb-2 h-0.5 overflow-hidden rounded-full bg-surface-2">
              <div
                className="h-full w-1/3 rounded-full bg-primary/60"
                style={{ animation: "indeterminate 1.4s ease-in-out infinite" }}
              />
            </div>
            {rewriting.preview == null && (
              <div className="mb-2 space-y-1.5" aria-hidden="true">
                <div className="h-2 w-full animate-pulse rounded bg-surface-2" />
                <div className="h-2 w-11/12 animate-pulse rounded bg-surface-2" />
                <div className="h-2 w-2/3 animate-pulse rounded bg-surface-2" />
              </div>
            )}
            <p className="text-xs text-muted-foreground">
              {rewriting.phase === "loading-model"
                ? t("toneRewrite.loadingModel")
                : t("toneRewrite.rewriting")}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
