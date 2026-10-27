import { useEffect, useState } from "react";
import logger from "../utils/logger";

export interface HotkeyModeInfo {
  // GNOME, KDE and Hyprland register through their own shortcut services,
  // which bind one hotkey per slot.
  isUsingNativeShortcut: boolean;
  /** Linux only: the evdev listener cannot read /dev/input. */
  linuxInputAccessDenied: boolean;
  /** False until main has answered; the defaults above are optimistic placeholders. */
  loaded: boolean;
}

const DEFAULT_INFO: HotkeyModeInfo = {
  isUsingNativeShortcut: false,
  linuxInputAccessDenied: false,
  loaded: false,
};

// How hotkeys are registered for the current session. `scope` tags log output
// for the calling surface.
export function useHotkeyModeInfo(scope: string): HotkeyModeInfo {
  const [modeInfo, setModeInfo] = useState<HotkeyModeInfo>(DEFAULT_INFO);

  useEffect(() => {
    let cancelled = false;
    window.retoneAPI
      ?.getHotkeyModeInfo?.()
      .then((info) => {
        if (!info || cancelled) return;
        setModeInfo({
          isUsingNativeShortcut: info.isUsingNativeShortcut,
          linuxInputAccessDenied: info.linuxInputAccessDenied ?? false,
          loaded: true,
        });
      })
      .catch((error) => {
        logger.error("Failed to check hotkey mode", { error }, scope);
      });
    return () => {
      cancelled = true;
    };
  }, [scope]);

  return modeInfo;
}
