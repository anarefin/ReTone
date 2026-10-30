import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Cpu, Monitor, Moon, Settings, Sun, WandSparkles } from "./icons";
import { SectionHeader, SettingsPanel, SettingsPanelRow, SettingsRow } from "./ui/SettingsSection";
import { Toggle } from "./ui/toggle";
import { Button } from "./ui/button";
import { Alert, AlertDescription, AlertTitle } from "./ui/alert";
import { AlertDialog } from "./ui/dialog";
import { HotkeyListInput } from "./ui/HotkeyListInput";
import LanguageSelector, { type LanguageOption } from "./ui/LanguageSelector";
import ReToneSection from "./settings/ReToneSection";
import ReToneModelSection from "./settings/ReToneModelSection";
import UpdatesSection from "./settings/UpdatesSection";
import SettingsSidebar, {
  settingsPanelId,
  settingsTabId,
  type SettingsTab,
} from "./settings/SettingsSidebar";
import { useSettingsStore } from "../stores/settingsStore";
import { useDialogs } from "../hooks/useDialogs";
import { useHotkeyModeInfo } from "../hooks/useHotkeyModeInfo";
import { validateHotkeyForSlot } from "../utils/hotkeyValidation";
import { getCachedPlatform } from "../utils/platform";
import logger from "../utils/logger";
import type { AppIconId } from "../types/nativeApi";
import thumbMono from "../assets/app-icons/thumb-mono.png";
import thumbLight from "../assets/app-icons/thumb-light.png";
import thumbAurora from "../assets/app-icons/thumb-aurora.png";
import thumbRing from "../assets/app-icons/thumb-ring.png";

const APP_ICON_OPTIONS: { value: AppIconId; thumb: string; labelKey: string }[] = [
  { value: "mono", thumb: thumbMono, labelKey: "settingsPage.general.appearance.appIconMono" },
  { value: "light", thumb: thumbLight, labelKey: "settingsPage.general.appearance.appIconLight" },
  {
    value: "aurora",
    thumb: thumbAurora,
    labelKey: "settingsPage.general.appearance.appIconAurora",
  },
  { value: "ring", thumb: thumbRing, labelKey: "settingsPage.general.appearance.appIconRing" },
];

const UI_LANGUAGE_OPTIONS: LanguageOption[] = [
  { value: "en", label: "English", flag: "🇺🇸" },
  { value: "ar", label: "العربية", flag: "🇦🇪" },
  { value: "bn", label: "বাংলা", flag: "🇧🇩" },
  { value: "es", label: "Español", flag: "🇪🇸" },
  { value: "fr", label: "Français", flag: "🇫🇷" },
  { value: "de", label: "Deutsch", flag: "🇩🇪" },
  { value: "pt", label: "Português", flag: "🇵🇹" },
  { value: "it", label: "Italiano", flag: "🇮🇹" },
  { value: "ru", label: "Русский", flag: "🇷🇺" },
  { value: "ja", label: "日本語", flag: "🇯🇵" },
  { value: "zh-CN", label: "简体中文", flag: "🇨🇳" },
  { value: "zh-TW", label: "繁體中文", flag: "🇹🇼" },
];

type SettingsTabId = "retone" | "model" | "general";

// The app's only window: ReTone's settings plus a few app-wide preferences.
export default function SettingsWindow() {
  const { t } = useTranslation();
  // Always opens on the first tab; not persisted.
  const [tab, setTab] = useState<SettingsTabId>("retone");
  const toneRewriteKey = useSettingsStore((s) => s.toneRewriteKey);
  const setToneRewriteKey = useSettingsStore((s) => s.setToneRewriteKey);
  const theme = useSettingsStore((s) => s.theme);
  const setTheme = useSettingsStore((s) => s.setTheme);
  const uiLanguage = useSettingsStore((s) => s.uiLanguage);
  const setUiLanguage = useSettingsStore((s) => s.setUiLanguage);
  const { alertDialog, showAlertDialog, hideAlertDialog } = useDialogs();
  const { isUsingNativeShortcut } = useHotkeyModeInfo("settings");
  const platform = getCachedPlatform();

  // The setter resolves to a failure when main-process registration fails;
  // surface it and return the result so HotkeyListInput rolls the row back.
  const [isHotkeyCommitting, setIsHotkeyCommitting] = useState(false);
  const commitHotkey = useCallback(
    async (key: string) => {
      setIsHotkeyCommitting(true);
      try {
        const result = await setToneRewriteKey(key);
        if (!result.success) {
          showAlertDialog({
            title: t("hooks.hotkeyRegistration.titles.notRegistered"),
            description: result.message || t("hooks.hotkeyRegistration.errors.failedToRegister"),
          });
        }
        return result.success;
      } finally {
        setIsHotkeyCommitting(false);
      }
    },
    [setToneRewriteKey, showAlertDialog, t]
  );
  const validateHotkey = useCallback((hotkey: string) => validateHotkeyForSlot(hotkey, {}, t), [t]);

  const [accessibilityTrusted, setAccessibilityTrusted] = useState(true);
  useEffect(() => {
    if (platform !== "darwin") return;
    const check = () =>
      window.retoneAPI
        ?.checkAccessibilityPermission?.(true)
        .then((trusted) => setAccessibilityTrusted(trusted !== false))
        .catch(() => {});
    void check();
    window.addEventListener("focus", check);
    return () => window.removeEventListener("focus", check);
  }, [platform]);

  const [autoStartEnabled, setAutoStartEnabled] = useState(false);
  const [autoStartNeedsApproval, setAutoStartNeedsApproval] = useState(false);
  const [autoStartAvailable, setAutoStartAvailable] = useState(true);
  const [autoStartLoading, setAutoStartLoading] = useState(true);
  const [startMinimized, setStartMinimizedState] = useState(false);

  const readAutoStartState = useCallback(async () => {
    if (!window.retoneAPI?.getAutoStartEnabled) return;
    try {
      const state = await window.retoneAPI.getAutoStartEnabled();
      setAutoStartEnabled(state.enabled);
      setAutoStartNeedsApproval(state.requiresApproval);
      setAutoStartAvailable(state.available ?? true);
    } catch (error) {
      logger.error("Failed to get auto-start status", error, "settings");
    }
  }, []);

  useEffect(() => {
    readAutoStartState().finally(() => setAutoStartLoading(false));
    window.retoneAPI
      ?.getStartMinimized?.()
      .then((value) => setStartMinimizedState(value === true))
      .catch(() => {});
  }, [readAutoStartState]);

  const handleAutoStartChange = async (enabled: boolean) => {
    if (!window.retoneAPI?.setAutoStartEnabled) return;
    try {
      setAutoStartLoading(true);
      const result = await window.retoneAPI.setAutoStartEnabled(enabled);
      if (!result.success) {
        logger.warn("Launch at login was not changed", { error: result.error }, "settings");
      }
      // Read the state back rather than assuming: the item can need approval
      // first, and a failed change leaves the toggle where macOS has it.
      await readAutoStartState();
    } catch (error) {
      logger.error("Failed to set auto-start", error, "settings");
    } finally {
      setAutoStartLoading(false);
    }
  };

  const handleStartMinimizedChange = (enabled: boolean) => {
    setStartMinimizedState(enabled);
    void window.retoneAPI?.setStartMinimized?.(enabled);
  };

  const [appIcon, setAppIconState] = useState<AppIconId>("mono");
  useEffect(() => {
    window.retoneAPI
      ?.getAppIcon?.()
      .then((icon) => setAppIconState(icon))
      .catch(() => {});
  }, []);

  const handleAppIconChange = (icon: AppIconId) => {
    setAppIconState(icon);
    window.retoneAPI
      ?.setAppIcon?.(icon)
      .catch((error) => logger.error("Failed to set the app icon", error, "settings"));
  };

  const themeOptions = [
    { value: "light", icon: Sun, label: t("settingsPage.general.appearance.light") },
    { value: "dark", icon: Moon, label: t("settingsPage.general.appearance.dark") },
    { value: "auto", icon: Monitor, label: t("settingsPage.general.appearance.auto") },
  ] as const;

  const tabs: SettingsTab<SettingsTabId>[] = [
    { id: "retone", label: t("settingsPage.tabs.retone"), Icon: WandSparkles },
    { id: "model", label: t("settingsPage.tabs.model"), Icon: Cpu },
    { id: "general", label: t("settingsPage.tabs.general"), Icon: Settings },
  ];
  const tabLabel = tabs.find((entry) => entry.id === tab)?.label;

  return (
    <div className="flex h-screen bg-background text-foreground">
      <nav className="w-44 shrink-0 space-y-4 border-e border-border px-3 py-8">
        <h1 className="px-2.5 text-base font-semibold">{t("reTone.title")}</h1>
        <SettingsSidebar<SettingsTabId>
          tabs={tabs}
          selectedId={tab}
          onSelect={setTab}
          label={t("settingsPage.tabs.label")}
        />
      </nav>

      <main
        key={tab}
        id={settingsPanelId(tab)}
        role="tabpanel"
        aria-labelledby={settingsTabId(tab)}
        className="flex-1 overflow-y-auto"
      >
        <div className="mx-auto max-w-2xl space-y-8 px-6 py-8">
          <header>
            <h2 className="text-xl font-semibold">{tabLabel}</h2>
            {tab === "retone" && (
              <p className="mt-1 text-sm text-muted-foreground">{t("reTone.description")}</p>
            )}
          </header>

          {tab === "retone" && platform === "darwin" && !accessibilityTrusted && (
            <Alert variant="warning">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>{t("reTone.accessibility.title")}</AlertTitle>
              <AlertDescription className="space-y-2">
                <p>{t("reTone.accessibility.description")}</p>
                <Button
                  onClick={() => void window.retoneAPI?.openAccessibilitySettings?.()}
                  variant="outline"
                  size="sm"
                >
                  {t("reTone.accessibility.action")}
                </Button>
              </AlertDescription>
            </Alert>
          )}

          {tab === "retone" && (
            <ReToneSection
              hotkeyInput={
                <HotkeyListInput
                  value={toneRewriteKey}
                  onChange={(list) => commitHotkey(list)}
                  onClear={() => commitHotkey("")}
                  validate={validateHotkey}
                  disabled={isHotkeyCommitting}
                  maxHotkeys={isUsingNativeShortcut ? 1 : undefined}
                />
              }
            />
          )}

          {tab === "model" && <ReToneModelSection />}

          {tab === "general" && (
            <>
              <div>
                <SectionHeader
                  title={t("settingsPage.general.appearance.title")}
                  description={t("settingsPage.general.appearance.description")}
                />
                <SettingsPanel>
                  <SettingsPanelRow>
                    <SettingsRow
                      label={t("settingsPage.general.appearance.theme")}
                      description={t("settingsPage.general.appearance.themeDescription")}
                    >
                      <div className="inline-flex items-center gap-px p-0.5 bg-muted/60 dark:bg-surface-2 rounded-md">
                        {themeOptions.map((option) => {
                          const Icon = option.icon;
                          const isSelected = theme === option.value;
                          return (
                            <button
                              key={option.value}
                              type="button"
                              onClick={() => setTheme(option.value)}
                              className={`flex items-center gap-1 px-2.5 py-1 rounded-[5px] text-xs font-medium transition-colors duration-100 ${
                                isSelected
                                  ? "bg-background dark:bg-surface-raised text-foreground shadow-sm"
                                  : "text-muted-foreground hover:text-foreground"
                              }`}
                            >
                              <Icon className={`w-3 h-3 ${isSelected ? "text-primary" : ""}`} />
                              {option.label}
                            </button>
                          );
                        })}
                      </div>
                    </SettingsRow>
                  </SettingsPanelRow>
                  <SettingsPanelRow>
                    <SettingsRow
                      label={t("settingsPage.general.appearance.appIcon")}
                      description={t("settingsPage.general.appearance.appIconDescription")}
                    >
                      <div
                        role="radiogroup"
                        aria-label={t("settingsPage.general.appearance.appIcon")}
                        className="flex items-center gap-1.5"
                      >
                        {APP_ICON_OPTIONS.map((option) => {
                          const isSelected = appIcon === option.value;
                          const label = t(option.labelKey);
                          return (
                            <button
                              key={option.value}
                              type="button"
                              role="radio"
                              aria-checked={isSelected}
                              aria-label={label}
                              title={label}
                              onClick={() => handleAppIconChange(option.value)}
                              className={`rounded-lg p-0.5 outline-none transition-colors duration-100 focus-visible:ring-2 focus-visible:ring-ring/30 ${
                                isSelected
                                  ? "ring-2 ring-primary"
                                  : "ring-1 ring-transparent hover:ring-border"
                              }`}
                            >
                              <img
                                src={option.thumb}
                                alt=""
                                aria-hidden="true"
                                width={36}
                                height={36}
                                decoding="async"
                                draggable={false}
                                className="block h-9 w-9"
                              />
                            </button>
                          );
                        })}
                      </div>
                    </SettingsRow>
                  </SettingsPanelRow>
                  <SettingsPanelRow>
                    <SettingsRow
                      label={t("settings.language.uiLabel")}
                      description={t("settings.language.uiDescription")}
                    >
                      <LanguageSelector
                        value={uiLanguage}
                        onChange={setUiLanguage}
                        options={UI_LANGUAGE_OPTIONS}
                        className="min-w-32"
                      />
                    </SettingsRow>
                  </SettingsPanelRow>
                </SettingsPanel>
              </div>

              <div>
                <SectionHeader
                  title={t("settingsPage.general.startup.title")}
                  description={t("settingsPage.general.startup.description")}
                />
                <SettingsPanel>
                  <SettingsPanelRow>
                    <SettingsRow
                      label={t("settingsPage.general.startup.launchAtLogin")}
                      description={t(
                        autoStartAvailable
                          ? "settingsPage.general.startup.launchAtLoginDescription"
                          : "settingsPage.general.startup.launchAtLoginUnavailable"
                      )}
                    >
                      <Toggle
                        checked={autoStartEnabled}
                        onChange={(checked: boolean) => void handleAutoStartChange(checked)}
                        disabled={autoStartLoading || !autoStartAvailable}
                      />
                    </SettingsRow>
                  </SettingsPanelRow>
                  {autoStartNeedsApproval && (
                    <SettingsPanelRow>
                      <Alert variant="warning">
                        <AlertTriangle className="h-4 w-4" />
                        <AlertTitle>
                          {t("settingsPage.general.startup.needsApproval.title")}
                        </AlertTitle>
                        <AlertDescription className="space-y-2">
                          <p>{t("settingsPage.general.startup.needsApproval.description")}</p>
                          <Button
                            onClick={() => void window.retoneAPI?.openLoginItemsSettings?.()}
                            variant="outline"
                            size="sm"
                          >
                            {t("settingsPage.general.startup.needsApproval.action")}
                          </Button>
                        </AlertDescription>
                      </Alert>
                    </SettingsPanelRow>
                  )}
                  <SettingsPanelRow>
                    <SettingsRow
                      label={t("settingsPage.general.startup.startMinimized")}
                      description={t("settingsPage.general.startup.startMinimizedDescription")}
                    >
                      <Toggle checked={startMinimized} onChange={handleStartMinimizedChange} />
                    </SettingsRow>
                  </SettingsPanelRow>
                </SettingsPanel>
              </div>

              <UpdatesSection />
            </>
          )}
        </div>
      </main>

      <AlertDialog
        open={alertDialog.open}
        onOpenChange={(open) => !open && hideAlertDialog()}
        title={alertDialog.title}
        description={alertDialog.description}
        onOk={() => {}}
      />
    </div>
  );
}
