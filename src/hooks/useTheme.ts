import { useEffect } from "react";
import { useSettingsStore } from "../stores/settingsStore";

function applyTheme(effectiveTheme: "light" | "dark") {
  document.documentElement.classList.toggle("dark", effectiveTheme === "dark");
  document.body.classList.toggle("dark", effectiveTheme === "dark");
}

export function useTheme() {
  const theme = useSettingsStore((s) => s.theme);
  const setTheme = useSettingsStore((s) => s.setTheme);

  useEffect(() => {
    if (theme !== "auto") {
      applyTheme(theme);
      return;
    }
    // Follow the system preference while on auto.
    const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
    applyTheme(mediaQuery.matches ? "dark" : "light");
    const handler = (e: MediaQueryListEvent) => applyTheme(e.matches ? "dark" : "light");
    mediaQuery.addEventListener("change", handler);
    return () => mediaQuery.removeEventListener("change", handler);
  }, [theme]);

  return { theme, setTheme };
}
