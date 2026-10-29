import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "../index.css";
import "./tray-panel.css";
import { configureProductTranslations } from "../i18n";
import { resolveStartupPolicy } from "@/lib/startupPolicy";
import { productThemeStorageKey } from "@/lib/productBranding";
import { getName } from "@tauri-apps/api/app";
import { FrontendErrorBoundary } from "@/components/FrontendErrorBoundary";
import { installGlobalErrorHandlers } from "@/lib/frontendLogger";
import { TrayPanel } from "./TrayPanel";

installGlobalErrorHandlers();

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { retry: 1, refetchOnWindowFocus: false },
  },
});

async function bootstrap() {
  const policy = await resolveStartupPolicy("__TAURI_INTERNALS__" in window);
  const osir = policy.kind !== "ccswitch";
  configureProductTranslations(osir);
  let productName = osir ? "OSIR Client" : "CC Switch";
  if (policy.kind === "osir" || policy.kind === "ccswitch") {
    try {
      productName = await getName();
    } catch {
      // Native identity already selected the safe product fallback.
    }
  }
  document.title = productName;
  const themeStorageKey = productThemeStorageKey(osir);
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <FrontendErrorBoundary>
        <QueryClientProvider client={queryClient}>
          <TrayPanel
            productName={productName}
            themeStorageKey={themeStorageKey}
          />
        </QueryClientProvider>
      </FrontendErrorBoundary>
    </React.StrictMode>,
  );
}

void bootstrap();
