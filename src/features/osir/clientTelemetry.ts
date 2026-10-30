import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface ClientTelemetryStatus {
  enabled: boolean;
  accountEnabled: boolean;
  available: boolean;
  reason: string | null;
}

export const clientTelemetryQuery = ["osir-client-telemetry"] as const;
export const hasNativeTelemetry = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function readClientTelemetry(): Promise<ClientTelemetryStatus> {
  return invoke("osir_client_telemetry_status");
}

export async function setClientTelemetry(
  enabled: boolean,
): Promise<ClientTelemetryStatus> {
  return invoke("osir_client_telemetry_set_enabled", { enabled });
}

export async function setClientActivity(
  enabled: boolean,
): Promise<ClientTelemetryStatus> {
  return invoke("osir_client_activity_set_enabled", { enabled });
}

/** Session snapshots only wake native coordination; they never authorize a send. */
export function useClientTelemetrySession(
  connected: boolean,
  generation: number | undefined,
) {
  const wasConnected = useRef(false);
  useEffect(() => {
    if (!hasNativeTelemetry()) return;
    if (!connected && !wasConnected.current) return;
    wasConnected.current = connected;
    // Metrics are optional: failure must not block login or show a background toast.
    void invoke("osir_client_telemetry_sync").catch(() => {});
  }, [connected, generation]);
}
