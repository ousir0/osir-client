import React from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TooltipProvider } from "@/components/ui/tooltip";
import OsirWorkbench from "./OsirWorkbench";
import OsirRoot from "./OsirRoot";
import "@/index.css";
import "@/i18n";
createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryClientProvider client={new QueryClient()}>
      <TooltipProvider>
        {new URLSearchParams(window.location.search).get("view") ===
        "workbench" ? (
          <OsirWorkbench />
        ) : (
          <OsirRoot />
        )}
      </TooltipProvider>
    </QueryClientProvider>
  </React.StrictMode>,
);
