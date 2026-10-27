// First: installs window.retoneAPI (over Tauri) before any module reads it.
import "./platform/nativeBridge";
import React from "react";
import ReactDOM from "react-dom/client";
import { I18nextProvider } from "react-i18next";
import AppRouter from "./AppRouter.jsx";
import ErrorBoundary from "./components/ErrorBoundary.tsx";
import { I18nDirectionProvider } from "./components/I18nDirectionProvider.tsx";
import { ToastProvider } from "./components/ui/Toast.tsx";
import { initializeSettings } from "./stores/settingsStore";
import { bindDocumentLanguage } from "./utils/i18nDocument";
import logger from "./utils/logger";

import i18n from "./i18n";
import "./index.css";

bindDocumentLanguage(i18n, document.documentElement, import.meta.hot);

// Anything the app's Content-Security-Policy (tauri.conf.json) blocks shows up
// in the app log, not only in the web inspector.
document.addEventListener("securitypolicyviolation", (event) => {
  logger.warn(
    "Content-Security-Policy violation",
    {
      directive: event.effectiveDirective,
      blocked: event.blockedURI,
      source: event.sourceFile,
      line: event.lineNumber,
    },
    "window"
  );
});

initializeSettings().catch((err) => {
  logger.warn("Failed to initialize settings", { error: err?.message }, "settings");
});

const root = ReactDOM.createRoot(document.getElementById("root"));
root.render(
  <React.StrictMode>
    <ErrorBoundary>
      <I18nextProvider i18n={i18n}>
        <I18nDirectionProvider>
          <ToastProvider>
            <AppRouter />
          </ToastProvider>
        </I18nDirectionProvider>
      </I18nextProvider>
    </ErrorBoundary>
  </React.StrictMode>
);

if (import.meta.hot) {
  import.meta.hot.accept();
}
