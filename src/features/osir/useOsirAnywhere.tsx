import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "@/lib/toast";

export const ANYWHERE_LOCAL_QUERY = ["osir-anywhere-local"] as const;
export const ANYWHERE_INSTALL_QUERY = ["osir-anywhere-install"] as const;
const RELEASE_QUERY = ["osir-anywhere-release"] as const;
export type AnywhereState = {
  status: "installed" | "not_installed" | "error" | "unsupported";
  installed: boolean;
  version: string | null;
  path: string | null;
  error: string | null;
  runtimeStatus: "running" | "stopped" | "unavailable";
};
export type AnywhereRelease = {
  status: "ready" | "not_configured" | "unsupported" | "error";
  version: string | null;
  platform: string | null;
  arch: string | null;
  size: number | null;
  error: string | null;
  systemSignature?: "unsigned" | "developer-id-notarized" | "authenticode";
};
export type AnywhereInstall = {
  phase:
    | "idle"
    | "downloading"
    | "ready"
    | "opening"
    | "waiting_installation"
    | "cancelled"
    | "error";
  downloadedBytes: number;
  totalBytes: number | null;
  error: string | null;
};

function useAnywhereController(desktop: boolean) {
  const client = useQueryClient();
  const [action, setAction] = useState<
    "launch" | "install" | "cancel" | "detect" | null
  >(null);
  const actionLock = useRef(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const previousInstallation = useRef<AnywhereState | null>(null);
  const local = useQuery({
    queryKey: ANYWHERE_LOCAL_QUERY,
    queryFn: () => invoke<AnywhereState>("osir_anywhere_detect"),
    enabled: desktop,
    retry: false,
    staleTime: 15_000,
    refetchOnWindowFocus: "always",
  });
  const installed =
    desktop &&
    !local.isError &&
    local.data?.status === "installed" &&
    local.data.installed;
  useEffect(() => {
    if (installed) previousInstallation.current = local.data!;
    else if (
      local.data?.status === "not_installed" ||
      local.data?.status === "unsupported"
    )
      previousInstallation.current = null;
  }, [installed, local.data]);
  const release = useQuery({
    queryKey: RELEASE_QUERY,
    queryFn: () => invoke<AnywhereRelease>("osir_anywhere_release"),
    enabled:
      desktop && local.data?.status === "not_installed" && !local.isError,
    retry: false,
    staleTime: 60_000,
  });
  const installation = useQuery({
    queryKey: ANYWHERE_INSTALL_QUERY,
    queryFn: () => invoke<AnywhereInstall>("osir_anywhere_install_status"),
    enabled: desktop,
    retry: false,
    staleTime: 0,
    refetchOnWindowFocus: false,
    // No idle, installer-waiting or error polling. A failed status read needs
    // an explicit retry before another installation can be requested.
    refetchInterval: (query) =>
      !action && !query.state.error && query.state.data?.phase === "downloading"
        ? 750
        : false,
  });
  const phase = installation.data?.phase;
  const previousPhase = useRef(phase);
  useEffect(() => {
    if (
      phase !== previousPhase.current &&
      (phase === "waiting_installation" || phase === "ready")
    ) {
      void client.invalidateQueries({ queryKey: ANYWHERE_LOCAL_QUERY });
    }
    previousPhase.current = phase;
  }, [phase, client]);

  async function redetect() {
    if (!desktop || actionLock.current) return;
    actionLock.current = true;
    setAction("detect");
    setActionError(null);
    try {
      await Promise.all([
        local.refetch(),
        release.refetch(),
        installation.refetch(),
        client.invalidateQueries({ queryKey: ["osir-anywhere-codex-status"] }),
      ]);
    } finally {
      actionLock.current = false;
      setAction(null);
    }
  }
  async function launch() {
    if (!desktop || actionLock.current || !installed || local.isFetching)
      return;
    actionLock.current = true;
    setAction("launch");
    setActionError(null);
    try {
      const result = await invoke<{ foreground: boolean }>(
        "osir_anywhere_launch",
      );
      if (!result?.foreground) throw new Error("unconfirmed");
      toast.success("OSIR Anywhere 已打开。");
    } catch {
      setActionError("暂时无法确认应用已打开，请查看应用或重新检测后重试。");
    } finally {
      await client.invalidateQueries({ queryKey: ANYWHERE_LOCAL_QUERY });
      actionLock.current = false;
      setAction(null);
    }
  }
  const canInstall =
    desktop &&
    !installed &&
    !local.isError &&
    local.data?.status === "not_installed" &&
    release.data?.status === "ready" &&
    !release.isError &&
    !release.isFetching &&
    !installation.isPending &&
    !installation.isError &&
    !local.isFetching &&
    !["downloading", "opening", "waiting_installation"].includes(phase ?? "");
  async function install() {
    if (!canInstall || actionLock.current) return;
    actionLock.current = true;
    setAction("install");
    setActionError(null);
    try {
      await client.cancelQueries({ queryKey: ANYWHERE_INSTALL_QUERY });
      const result = await invoke<AnywhereInstall>("osir_anywhere_install");
      client.setQueryData(ANYWHERE_INSTALL_QUERY, result);
      if (result.phase !== "downloading")
        await client.invalidateQueries({ queryKey: ANYWHERE_LOCAL_QUERY });
    } catch {
      setActionError("安装未完成，请重新检测下载状态后重试。");
      await installation.refetch();
    } finally {
      actionLock.current = false;
      setAction(null);
    }
  }
  async function cancel() {
    if (!desktop || actionLock.current || phase !== "downloading") return;
    actionLock.current = true;
    setAction("cancel");
    setActionError(null);
    try {
      await client.cancelQueries({ queryKey: ANYWHERE_INSTALL_QUERY });
      client.setQueryData(
        ANYWHERE_INSTALL_QUERY,
        await invoke<AnywhereInstall>("osir_anywhere_cancel_install"),
      );
    } catch {
      setActionError("取消未确认，请重新检测下载状态。");
      await installation.refetch();
    } finally {
      actionLock.current = false;
      setAction(null);
    }
  }
  const detectionFailed = local.isError || local.data?.status === "error";
  let label = "检测中";
  let description = "正在检查本机安装状态…";
  if (!desktop) {
    label = "桌面预览";
    description = "请在桌面客户端检测、安装和打开 OSIR Anywhere。";
  } else if (detectionFailed) {
    label = "检测异常";
    description = "本机检测失败，请重新检测。";
  } else if (installed) {
    label = local.data?.runtimeStatus === "running" ? "运行中" : "已安装";
    description = "安装已完成，打开应用后开始连接。";
  } else if (local.data?.status === "unsupported") {
    label = "平台暂不支持";
    description = "当前系统暂不支持安装 OSIR Anywhere。";
  } else if (installation.isError || phase === "error") {
    label = "安装异常";
    description = installation.isError
      ? "下载状态读取失败，请重新检测后重试。"
      : "下载或安装未完成，请检查网络、磁盘空间与系统权限后重试。";
  } else if (phase === "downloading") {
    label = "下载中";
    description = "正在下载安装包，完成后将打开安装器。";
  } else if (phase === "opening") {
    label = "正在打开安装器";
    description = "请完成系统确认；安装后点击重新检测。";
  } else if (phase === "waiting_installation") {
    label = "等待安装完成";
    description =
      "请在安装器中完成安装；macOS 请将应用拖入 Applications，然后重新检测。";
  } else if (phase === "ready") {
    label = "安装包已就绪";
    description = "安装包已下载并校验，点击继续安装。";
  } else if (
    local.data?.status === "not_installed" &&
    !release.isPending &&
    !installation.isPending
  ) {
    if (release.isError || release.data?.status === "error") {
      label = "发行信息异常";
      description = "暂时无法获取安装包信息，请重试。";
    } else if (release.data?.status === "unsupported") {
      label = "平台暂不支持";
      description = "当前系统或处理器暂没有可用安装包。";
    } else if (release.data?.status === "not_configured") {
      label = "即将开放";
      description = "正式安装包尚未开放下载，请稍后再试。";
    } else if (release.data?.status === "ready") {
      label = phase === "cancelled" ? "已取消" : "未安装";
      description =
        phase === "cancelled"
          ? "下载已取消，可重新开始安装。"
          : "安装桌面应用，跨设备继续 AI 任务。";
    }
  }
  return {
    desktop,
    local,
    release,
    installation,
    installed,
    label,
    description,
    displayPath: detectionFailed
      ? previousInstallation.current?.path
      : local.data?.path,
    detectionFailed,
    action,
    actionError,
    canInstall,
    redetect,
    launch,
    install,
    cancel,
  };
}

type AnywhereController = ReturnType<typeof useAnywhereController>;
const AnywhereContext = createContext<AnywhereController | null>(null);
export function OsirAnywhereProvider({
  desktop,
  children,
}: {
  desktop: boolean;
  children: ReactNode;
}) {
  const value = useAnywhereController(desktop);
  return (
    <AnywhereContext.Provider value={value}>
      {children}
    </AnywhereContext.Provider>
  );
}
export function useOsirAnywhere() {
  const value = useContext(AnywhereContext);
  if (!value) throw new Error("OSIR Anywhere provider missing");
  return value;
}
