import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ToolName } from "@/components/apps/useToolManagement";
import { TOOL_IDS, TOOL_NAMES, type ToolId } from "./providerConfig";

export type SupportedToolId = ToolName | "workbuddy" | "claude-desktop";
export type ManagedToolId = Exclude<SupportedToolId, ToolId>;
// Keep lifecycle-only tools separate from the four OSIR provider integrations.
export const LOCAL_TOOL_CATALOG: Record<
  SupportedToolId,
  { name: string; description: string; desktop?: boolean }
> = {
  codex: {
    name: TOOL_NAMES.codex,
    description: "桌面会话、供应商与多模型管理",
    desktop: true,
  },
  claude: {
    name: TOOL_NAMES.claude,
    description: "供应商切换、模型与本机会话",
  },
  "claude-desktop": {
    name: "Claude 桌面客户端",
    description: "Claude 官方桌面应用；与 Claude Code CLI 分开启动",
    desktop: true,
  },
  opencode: {
    name: TOOL_NAMES.opencode,
    description: "模型连接、默认模型与本机会话",
  },
  gemini: { name: TOOL_NAMES.gemini, description: "供应商连接与本机会话" },
  grok: { name: "Grok Build", description: "本机安装与更新管理" },
  openclaw: { name: "OpenClaw", description: "本机安装与更新管理" },
  hermes: { name: "Hermes", description: "本机安装与更新管理" },
  pi: { name: "Pi", description: "本机安装与更新管理" },
  mcode: { name: "MiniMax Code", description: "本机安装与更新管理" },
  kimi: { name: "Kimi Code", description: "本机安装与更新管理" },
  agy: { name: "Antigravity CLI", description: "本机安装与更新管理" },
  omp: { name: "Oh My Pi", description: "本机安装与更新管理" },
  workbuddy: {
    name: "WorkBuddy",
    description: "官方下载安装与 OSIRAPI 自定义模型",
    desktop: true,
  },
};
export const LOCAL_TOOL_IDS = Object.keys(
  LOCAL_TOOL_CATALOG,
) as SupportedToolId[];
export const isProviderTool = (id: string): id is ToolId =>
  TOOL_IDS.includes(id as ToolId);
export const isManagedTool = (id: string): id is ManagedToolId =>
  Object.prototype.hasOwnProperty.call(LOCAL_TOOL_CATALOG, id) &&
  !isProviderTool(id);
export interface LocalToolInstallation {
  id: SupportedToolId;
  installed: boolean;
  version: string | null;
  path: string | null;
  status: "installed" | "not_installed" | "error" | "unavailable";
  error: string | null;
  kind?: "desktop" | "cli";
  runtimeStatus?: "running" | "stopped" | "unavailable";
  runtimeError?: string | null;
  launchSupported?: boolean;
  closeSupported?: boolean;
  restartSupported?: boolean;
}
export type ToolLaunchTarget = "desktop" | "cli";
export type ToolLaunchResult = {
  runtimeStatus: "running" | "stopped" | "unavailable";
  foreground: boolean;
  isolated: boolean;
  alreadyRunning: boolean;
  /** Only this launch request's exit code; no terminal output is captured. */
  exitCode?: number | null;
  restarted?: boolean;
};
export type ToolCloseResult = {
  runtimeStatus: "running" | "stopped" | "unavailable";
  closed: boolean;
  alreadyStopped: boolean;
};

export function toolRuntimeLabel(info?: LocalToolInstallation) {
  if (
    !info?.installed ||
    info.kind === "cli" ||
    !LOCAL_TOOL_CATALOG[info.id].desktop
  )
    return "";
  return info.runtimeStatus === "running"
    ? "运行中"
    : info.runtimeStatus === "stopped"
      ? "未运行"
      : "运行状态暂不可用";
}
export const installedToolIds = (tools: LocalToolInstallation[] = []) =>
  LOCAL_TOOL_IDS.filter((id) =>
    tools.some((tool) => tool.id === id && tool.installed),
  );
export const LOCAL_TOOLS_QUERY = ["osir-installed-tools"] as const;
export const DESKTOP_RUNTIME_QUERY = ["osir-desktop-runtime"] as const;

async function detectDesktopRuntime() {
  const snapshot = await invoke<LocalToolInstallation[]>(
    "osir_desktop_runtime",
  );
  if (!Array.isArray(snapshot)) throw new Error("运行状态暂不可用");
  return snapshot;
}

// Mount once in the workbench. Additional inventory consumers subscribe to the
// shared cache without creating their own interval or running CLI probes.
export function useDesktopRuntimeHeartbeat(enabled: boolean) {
  useQuery({
    queryKey: DESKTOP_RUNTIME_QUERY,
    queryFn: detectDesktopRuntime,
    enabled,
    retry: false,
    networkMode: "always",
    staleTime: 1_000,
    refetchInterval: 3_000,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: "always",
  });
}

export function mergeDesktopRuntime(
  installations: LocalToolInstallation[] | undefined,
  runtime: LocalToolInstallation[] | undefined,
  failed: boolean,
) {
  return installations?.map((info) => {
    if (!LOCAL_TOOL_CATALOG[info.id].desktop || info.kind === "cli")
      return info;
    if (failed)
      return {
        ...info,
        runtimeStatus: "unavailable" as const,
        runtimeError: "运行状态暂不可用，请重新检测",
        launchSupported: false,
        closeSupported: false,
        restartSupported: false,
      };
    const current = runtime?.find((item) => item.id === info.id);
    // A per-tool failure still resolves the IPC successfully on Windows. It is
    // unknown installation state, not evidence that the user uninstalled it.
    // Preserve the last inventory while disabling actions until a fresh probe
    // succeeds; a successful not_installed result below still removes it.
    if (current?.status === "unavailable")
      return {
        ...info,
        runtimeStatus: "unavailable" as const,
        runtimeError:
          current.error ??
          current.runtimeError ??
          "运行状态暂不可用，请重新检测",
        launchSupported: false,
        closeSupported: false,
        restartSupported: false,
      };
    return current ? { ...info, ...current } : info;
  });
}

// Local metadata only: no update checks, account reads or credential access.
export function useLocalTools(enabled: boolean) {
  const queryClient = useQueryClient();
  const installations = useQuery({
    queryKey: LOCAL_TOOLS_QUERY,
    queryFn: async () => {
      const detected =
        await invoke<LocalToolInstallation[]>("osir_detect_tools");
      const previous =
        queryClient.getQueryData<LocalToolInstallation[]>(LOCAL_TOOLS_QUERY);
      // A full refresh can fail per tool just like the runtime heartbeat.
      // Preserve known installations only for unknown results; confirmed
      // removals and fresh successes must still replace previous evidence.
      return detected.map((current) => {
        const known = previous?.find(
          (item) => item.id === current.id && item.installed,
        );
        if (current.status !== "unavailable" || !known) return current;
        return {
          ...current,
          installed: true,
          version: known.version,
          path: known.path,
          runtimeStatus: "unavailable" as const,
          launchSupported: false,
          closeSupported: false,
          restartSupported: false,
        };
      });
    },
    enabled,
    retry: false,
    // Local filesystem/registry IPC must run even when WebView reports offline.
    networkMode: "always",
    staleTime: 30_000,
    refetchOnWindowFocus: "always",
  });
  const runtime = useQuery({
    queryKey: DESKTOP_RUNTIME_QUERY,
    queryFn: detectDesktopRuntime,
    enabled: enabled && installations.isSuccess,
    retry: false,
    networkMode: "always",
    staleTime: 1_000,
    refetchOnWindowFocus: "always",
  });
  const data = useMemo(
    () =>
      mergeDesktopRuntime(
        installations.data,
        runtime.dataUpdatedAt >= installations.dataUpdatedAt
          ? runtime.data
          : undefined,
        runtime.isError &&
          runtime.errorUpdatedAt >= installations.dataUpdatedAt,
      ),
    [
      installations.data,
      installations.dataUpdatedAt,
      runtime.data,
      runtime.dataUpdatedAt,
      runtime.isError,
      runtime.errorUpdatedAt,
    ],
  );
  return { ...installations, data };
}
