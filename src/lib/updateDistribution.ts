import { invoke } from "@tauri-apps/api/core";

export const OSIR_UPDATE_UNCONFIGURED =
  "OSIR 更新渠道未配置：尚无可用的 OSIR 签名更新源。";
export type UpdateDistribution = {
  kind: "ccswitch" | "osir" | "unavailable";
  updatesEnabled: boolean;
  reason: string | null;
};

export async function getUpdateDistribution(): Promise<UpdateDistribution> {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    return {
      kind: "unavailable",
      updatesEnabled: false,
      reason: "浏览器预览无法检查客户端更新。",
    };
  }
  try {
    const capability = await invoke<UpdateDistribution>(
      "get_update_distribution",
    );
    if (
      !capability ||
      !["ccswitch", "osir", "unavailable"].includes(capability.kind) ||
      typeof capability.updatesEnabled !== "boolean" ||
      (capability.reason !== null && typeof capability.reason !== "string") ||
      (capability.kind === "unavailable" && capability.updatesEnabled)
    ) {
      throw new Error("Invalid update capability");
    }
    // Return only capability metadata, never a feed URL, key or config override.
    return {
      kind: capability.kind,
      updatesEnabled: capability.updatesEnabled,
      reason: capability.reason,
    };
  } catch {
    return {
      kind: "unavailable",
      updatesEnabled: false,
      reason: "无法确认客户端发行版，更新不可用。",
    };
  }
}
