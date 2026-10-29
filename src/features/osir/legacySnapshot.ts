import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";

export type LegacySnapshot = {
  version: 1;
  kind: "legacy_subscription_grant";
  captured_at: string;
  user_id: number;
  display_name: string | null;
  subscriptions: {
    id: number;
    group_name: string | null;
    status: string;
    expires_at: string | null;
  }[];
};

export function useLegacySnapshot(enabled: boolean) {
  return useQuery({
    queryKey: ["osir-legacy-snapshot"],
    queryFn: () => invoke<LegacySnapshot | null>("osir_legacy_snapshot"),
    enabled,
    retry: false,
    staleTime: 60_000,
  });
}

export function legacyIdentity(snapshot: LegacySnapshot) {
  return snapshot.display_name || `OSIR 用户 #${snapshot.user_id}`;
}
