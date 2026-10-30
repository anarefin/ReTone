import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { SectionHeader, SettingsPanel, SettingsPanelRow, SettingsRow } from "../ui/SettingsSection";
import { Button } from "../ui/button";
import logger from "../../utils/logger";
import type { UpdateCheckResult } from "../../types/nativeApi";

const LAST_CHECK_KEY = "lastUpdateCheck";
const AUTO_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

function autoCheckDue(): boolean {
  try {
    const last = Number(localStorage.getItem(LAST_CHECK_KEY));
    return !Number.isFinite(last) || Date.now() - last >= AUTO_CHECK_INTERVAL_MS;
  } catch {
    return true;
  }
}

function recordCheck() {
  try {
    localStorage.setItem(LAST_CHECK_KEY, String(Date.now()));
  } catch {
    // Storage unavailable: the next visit checks again.
  }
}

// General › Updates: links to a newer GitHub release; nothing is installed.
// Checks once a day when the tab opens, otherwise on demand.
export default function UpdatesSection() {
  const { t } = useTranslation();
  const [version, setVersion] = useState("");
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<UpdateCheckResult | null>(null);

  const check = useCallback(async () => {
    if (!window.retoneAPI?.checkForUpdate) return;
    setChecking(true);
    try {
      const next = await window.retoneAPI.checkForUpdate();
      setResult(next);
      if (next.status !== "failed") recordCheck();
    } catch (error) {
      logger.error("Update check failed", error, "settings");
      setResult({ status: "failed", currentVersion: version });
    } finally {
      setChecking(false);
    }
  }, [version]);

  useEffect(() => {
    window.retoneAPI
      ?.getAppVersion?.()
      .then(setVersion)
      .catch(() => {});
    if (autoCheckDue()) void check();
    // Runs once per tab visit.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  let description = t("settingsPage.general.updates.idle");
  if (checking) description = t("settingsPage.general.updates.checking");
  else if (result?.status === "upToDate") description = t("settingsPage.general.updates.upToDate");
  else if (result?.status === "available")
    description = t("settingsPage.general.updates.available", { version: result.latestVersion });
  else if (result?.status === "failed") description = t("settingsPage.general.updates.failed");

  return (
    <div>
      <SectionHeader
        title={t("settingsPage.general.updates.title")}
        description={t("settingsPage.general.updates.description")}
      />
      <SettingsPanel>
        <SettingsPanelRow>
          <SettingsRow
            label={t("settingsPage.general.updates.version", { version })}
            description={description}
          >
            {result?.status === "available" && result.url ? (
              <Button onClick={() => void window.retoneAPI?.openExternal(result.url!)} size="sm">
                {t("settingsPage.general.updates.download")}
              </Button>
            ) : (
              <Button onClick={() => void check()} disabled={checking} variant="outline" size="sm">
                {t("settingsPage.general.updates.check")}
              </Button>
            )}
          </SettingsRow>
        </SettingsPanelRow>
      </SettingsPanel>
    </div>
  );
}
