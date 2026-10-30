import { lazy, Suspense, useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { LoaderCircle } from "lucide-react";
import { DeepLinkImportDialog } from "@/components/DeepLinkImportDialog";
const OsirWorkbench = lazy(() => import("./OsirWorkbench"));
import { useAccountSession } from "./accountApi";
import { OsirAccountPanel } from "./OsirAccountPanel";
import { TOOL_IDS } from "./providerConfig";
import { useClientTelemetrySession } from "./clientTelemetry";
import { OsirClientUpdateDialog } from "./OsirClientUpdateDialog";

export default function OsirRoot() {
  const session = useAccountSession();
  useClientTelemetrySession(
    !!session.data?.connected && !session.isError,
    session.data?.generation,
  );
  const queryClient = useQueryClient();
  const restoringSession =
    !session.isError &&
    (session.isPending ||
      session.data?.recovery === "pending" ||
      session.data?.recovery === "restoring");
  const loading = (
    <main className="osir-welcome">
      <div
        className="osir-welcome-drag"
        data-tauri-drag-region
        aria-hidden="true"
      />
      <div className="osir-welcome-loading" role="status" aria-busy="true">
        <LoaderCircle className="osir-welcome-spinner" aria-hidden="true" />
        <span>
          {session.data?.connected ? "正在打开工作台…" : "正在恢复登录…"}
        </span>
      </div>
    </main>
  );
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    // Retain inactive legacy cache entries so invalidation is observable even
    // when the old provider screen has never been opened. No data is fetched.
    for (const tool of TOOL_IDS) {
      queryClient.getQueryCache().build(queryClient, {
        queryKey: ["providers", tool],
        gcTime: Infinity,
      });
    }
    // The shared importer invalidates the foundation's provider cache. Refresh
    // the OSIR projections too, including an optional immediately active import.
    return queryClient.getQueryCache().subscribe((event) => {
      if (
        event.type !== "updated" ||
        event.action.type !== "invalidate" ||
        event.query.queryKey[0] !== "providers"
      )
        return;
      for (const key of [
        "osir-providers",
        "osir-current",
        "osir-mode",
        "osir-stack",
      ]) {
        void queryClient.invalidateQueries({ queryKey: [key] });
      }
    });
  }, [queryClient]);
  return (
    <>
      {session.data?.connected ? (
        <Suspense fallback={loading}>
          <OsirWorkbench />
        </Suspense>
      ) : restoringSession ? (
        loading
      ) : (
        <OsirAccountPanel available reason="" welcome />
      )}
      {/* Keep the native import owner mounted across account transitions so
          pending confirmation and import remain intact. */}
      {"__TAURI_INTERNALS__" in window && <DeepLinkImportDialog />}
      {"__TAURI_INTERNALS__" in window && <OsirClientUpdateDialog />}
    </>
  );
}
