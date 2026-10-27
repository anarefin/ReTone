import React from "react";
import ToneRewritePickerOverlay from "./components/ToneRewritePickerOverlay.tsx";
import SettingsWindow from "./components/SettingsWindow.tsx";
import { useTheme } from "./hooks/useTheme";

export default function AppRouter() {
  useTheme();

  if (window.location.search.includes("tone-rewrite-picker=true")) {
    return <ToneRewritePickerOverlay />;
  }

  return <SettingsWindow />;
}
