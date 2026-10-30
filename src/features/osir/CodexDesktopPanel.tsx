import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Download, Check, Folder, RefreshCw, RotateCcw } from "lucide-react";
import { toast } from "@/lib/toast";
import { connectionError } from "./errors";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { SupportedToolIcon } from "./SupportedToolIcon";
import { LOCAL_TOOLS_QUERY, useLocalTools } from "./toolsApi";
import { ToolLaunchButton } from "./ToolLaunchButton";
import {
  CODEX_INSTALL_SOURCES,
  codexDesktopReleaseKey,
  useCodexInstallSource,
  type CodexInstallSource,
} from "./codexDesktopApi";
import "./management.css";
import "./codex-desktop.css";

type DesktopDetection = {
  status: "detected" | "not_found" | "inspection_failed" | "unsupported";
  comparisonComplete?: boolean;
  recoveryTargets?: string[];
  installations: {
    path: string;
    version: string | null;
    build: string | null;
  }[];
  message: string;
};

type ReleaseCheck = {
  source?: CodexInstallSource;
  actualSource?: CodexInstallSource;
  highestDetectedBuild?: number | null;
  comparisonComplete?: boolean;
  installedBuild: number | null;
  release: {
    build: number;
    version: string;
    fullSize: number;
    updateAvailable: boolean;
  };
  targetPath: string;
  restorePoints: {
    receiptId: string;
    targetBuild: number;
    previousBuild: number;
  }[];
};

type LocalReport = Pick<
  ReleaseCheck,
  "installedBuild" | "targetPath" | "restorePoints"
> & {
  recoveryRequired: boolean;
  updateBlockedReason?: string | null;
  installMethod?: "microsoft_store";
};
type InstallReport = {
  step: string;
  build: number | null;
  installed?: boolean;
};
type OperationNotice = string | { message: string; handedOff: true };
type InstallerStatus = {
  operationId: string | null;
  phase: string | null;
};
type PhaseHistory = { operationId: string; phases: string[]; ended: boolean };
type InstallOutcome = "completed" | "failed" | "cancelled" | null;
type DesktopOperationResult = {
  working: boolean;
  message: string;
  failed: boolean;
};
const DESKTOP_OPERATION_QUERY = ["osir-codex-operation-result"] as const;
const installSteps = ["下载", "校验", "安装", "完成"];
// Extraction prepares the candidate for application verification; only applying
// commits the installation. Checking/applying alone also occur in other ops.
const installStepForPhase = (phase: string | null): number => {
  switch (phase) {
    case "downloading":
      return 0;
    case "verifying_artifact":
    case "extracting":
    case "verifying_application":
      return 1;
    case "applying":
      return 2;
    default:
      return -1;
  }
};

// These are live coordinator phases, not the install journal's terminal steps.
// Status has neither percentages nor a final outcome; skipped polls prove nothing
// about intermediate phases, and a released operation may have failed.
const installerPhases: Record<string, { label: string; description: string }> =
  {
    checking: {
      label: "检查状态",
      description: "正在检查本地安装与操作条件…",
    },
    downloading: {
      label: "下载安装包",
      description: "正在下载安装包，请保持客户端打开…",
    },
    verifying_artifact: {
      label: "验证安装包",
      description: "正在验证安装包签名，大文件校验可能需要数分钟…",
    },
    extracting: {
      label: "解压安装包",
      description: "正在解压并检查安装包，请稍候…",
    },
    verifying_application: {
      label: "验证应用",
      description: "正在验证应用签名与发布者…",
    },
    applying: {
      label: "应用更改",
      description: "正在应用更改并保存恢复记录，此阶段不能取消…",
    },
    local_inspection: {
      label: "检查本地安装",
      description: "正在检查本地安装…",
    },
  };
const cancellablePhases = new Set([
  "checking",
  "downloading",
  "verifying_artifact",
  "extracting",
  "verifying_application",
]);
const phaseDetails = (phase: string) =>
  Object.prototype.hasOwnProperty.call(installerPhases, phase)
    ? installerPhases[phase]
    : {
        label: "其他处理阶段",
        description: "安装器报告了暂未识别的阶段，请等待当前操作结束。",
      };

export function CodexDesktopPanel({
  desktop,
  onBusyChange,
  onNavigationBlockedChange,
}: {
  desktop: boolean;
  onBusyChange?: (busy: boolean) => void;
  onNavigationBlockedChange?: (blocked: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const tools = useLocalTools(desktop);
  const currentCodex = tools.data?.find((item) => item.id === "codex");
  const detection = useQuery({
    queryKey: ["osir-codex-desktop"],
    queryFn: async () =>
      await invoke<DesktopDetection>("osir_codex_desktop_detection"),
    enabled: desktop,
    retry: false,
    throwOnError: false,
    staleTime: 60_000,
  });
  const [selectedTarget, setSelectedTarget] = useState<string | null>(null);
  const targetPath =
    selectedTarget === ""
      ? undefined
      : (selectedTarget ??
        currentCodex?.path ??
        detection.data?.installations[0]?.path ??
        detection.data?.recoveryTargets?.[0] ??
        undefined);
  const targetRef = useRef(targetPath);
  targetRef.current = targetPath;
  const { source, setSource } = useCodexInstallSource();
  const operationLock = useRef(false);
  const [installConfirmation, setInstallConfirmation] = useState<{
    source: CodexInstallSource;
    build: number;
    version: string;
    targetPath: string;
    installedBuild: number | null;
    actualSource: CodexInstallSource;
  } | null>(null);
  const [restorePoint, setRestorePoint] = useState<
    ReleaseCheck["restorePoints"][number] | null
  >(null);
  const nativeStatus = useRef<InstallerStatus | null>(null);
  const [status, setStatus] = useState<InstallerStatus | null>(null);
  const [history, setHistory] = useState<PhaseHistory | null>(null);
  const [operationKind, setOperationKind] = useState<
    "install" | "other" | null
  >(null);
  const [installOutcome, setInstallOutcome] = useState<InstallOutcome>(null);
  const [statusUnavailable, setStatusUnavailable] = useState(false);
  const [statusRetry, setStatusRetry] = useState(0);
  const [cancelRequested, setCancelRequested] = useState<string | null>(null);
  const externalWorking =
    !!status?.operationId && status.phase !== "local_inspection";
  const releaseQuery = useQuery<ReleaseCheck>({
    queryKey: codexDesktopReleaseKey(source, targetPath),
    queryFn: async () => {
      const result = await invoke<ReleaseCheck>("osir_codex_install_check", {
        source,
        ...(targetPath ? { targetPath } : {}),
      });
      if (result.source && result.source !== source)
        throw new Error("更新源已变化，请重新检查版本。");
      return result;
    },
    enabled: false,
    retry: false,
    refetchOnWindowFocus: false,
    staleTime: 60_000,
  });
  const release = releaseQuery.isError ? undefined : releaseQuery.data;
  const clearRelease = () => {
    void queryClient.resetQueries({ queryKey: ["osir-codex-update-check"] });
  };
  const operation = useQuery<DesktopOperationResult>({
    queryKey: DESKTOP_OPERATION_QUERY,
    queryFn: () => ({ working: false, message: "", failed: false }),
    initialData: { working: false, message: "", failed: false },
    enabled: false,
    gcTime: Infinity,
    staleTime: Infinity,
  });
  const { working, message, failed } = operation.data;
  const updateOperation = (patch: Partial<DesktopOperationResult>) =>
    queryClient.setQueryData<DesktopOperationResult>(
      DESKTOP_OPERATION_QUERY,
      (previous) => ({
        working: false,
        message: "",
        failed: false,
        ...previous,
        ...patch,
      }),
    );
  const setWorking = (value: boolean) => updateOperation({ working: value });
  const setMessage = (value: string) => updateOperation({ message: value });
  const setFailed = (value: boolean) => updateOperation({ failed: value });
  useEffect(() => {
    if (!desktop) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      const requestedDuringOperation = operationLock.current;
      try {
        const next = await invoke<InstallerStatus>("osir_codex_install_status");
        if (!stopped) {
          const previous = nativeStatus.current;
          nativeStatus.current = next;
          setStatus(next);
          setStatusUnavailable(false);
          if (
            next.operationId &&
            next.operationId !== previous?.operationId &&
            next.phase !== "local_inspection" &&
            !operationLock.current &&
            !requestedDuringOperation
          ) {
            setMessage("");
            setFailed(false);
            setCancelRequested(null);
            setOperationKind(null);
            setInstallOutcome(null);
          }
          setHistory((current) => {
            if (!next.operationId || next.phase === "local_inspection")
              return current && !current.ended
                ? { ...current, ended: true }
                : current;
            const phases =
              current?.operationId === next.operationId ? current.phases : [];
            return {
              operationId: next.operationId,
              phases:
                next.phase && !phases.includes(next.phase)
                  ? [...phases, next.phase]
                  : phases,
              ended: false,
            };
          });
          if (
            previous?.operationId &&
            previous.operationId !== next.operationId &&
            previous.phase !== "local_inspection"
          )
            void detection.refetch();
        }
      } catch {
        // A failed status read is not an installation failure or completion.
        if (!stopped) setStatusUnavailable(true);
      } finally {
        if (!stopped && (working || nativeStatus.current?.operationId))
          timer = setTimeout(() => void poll(), 1500);
      }
    }
    void poll();
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  }, [desktop, working, statusRetry]);
  async function run(
    action: () => Promise<OperationNotice>,
    kind: "install" | "other" = "other",
  ) {
    if (working || operationLock.current || nativeStatus.current?.operationId)
      return;
    operationLock.current = true;
    setHistory(null);
    setOperationKind(kind);
    setInstallOutcome(null);
    setCancelRequested(null);
    setWorking(true);
    setFailed(false);
    setMessage("");
    try {
      const result = await action();
      const notice = typeof result === "string" ? result : result.message;
      setMessage(notice);
      if (typeof result !== "string" && result.handedOff) {
        toast.info(notice);
        setOperationKind("other");
      } else {
        toast.success(notice);
        if (kind === "install") setInstallOutcome("completed");
      }
    } catch (error) {
      const detail = connectionError(error);
      // This exact message is emitted by native InstallError::Cancelled. A
      // successful cancel request or an idle status is not a terminal result.
      const cancelled = detail === "操作已取消";
      setFailed(!cancelled);
      setMessage(detail);
      if (cancelled) toast.info(detail);
      else toast.error(detail);
      if (kind === "install")
        setInstallOutcome(cancelled ? "cancelled" : "failed");
    } finally {
      operationLock.current = false;
      setWorking(false);
      // Keep the receipt and inventory fresh even when the user left this page.
      void queryClient.invalidateQueries({
        queryKey: ["osir-codex-install-local"],
      });
      void queryClient.invalidateQueries({ queryKey: ["osir-codex-desktop"] });
      void queryClient.invalidateQueries({ queryKey: LOCAL_TOOLS_QUERY });
    }
  }
  async function check() {
    const checkedTarget = targetPath;
    const result = await releaseQuery.refetch({ throwOnError: true });
    if (checkedTarget !== targetRef.current)
      throw new Error("安装位置已变化，请重新检查更新。");
    if (!result.data) throw new Error("未能读取版本，请重试。");
    return result.data.release.updateAvailable
      ? "已获取可安装版本。"
      : "已是最新版本。";
  }
  const local = useQuery({
    queryKey: ["osir-codex-install-local", targetPath ?? "managed"],
    queryFn: () =>
      invoke<LocalReport>("osir_codex_install_local", {
        action: "inspect",
        ...(targetPath ? { targetPath } : {}),
      }),
    enabled: desktop && !working && !externalWorking,
    refetchOnWindowFocus: false,
    // The overview's version check shares the native installer lease. Busy
    // means inspection is queued behind that read, not a broken installation.
    retry: (count, error) =>
      count < 8 &&
      (typeof error === "string" ? error : error.message) ===
        "安装器正忙，请等待当前操作完成",
    retryDelay: (attempt) => Math.min(250 * 2 ** attempt, 2000),
    staleTime: 60_000,
  });
  const active = working || !!status?.operationId;
  const systemInstall = local.data?.installMethod === "microsoft_store";
  useEffect(() => {
    if (systemInstall && source === "osir_mirror") setSource("official");
  }, [systemInstall, source, setSource]);
  const currentPhase =
    !statusUnavailable && status?.operationId ? status.phase : null;
  const canCancel = !!currentPhase && cancellablePhases.has(currentPhase);
  const showInstallSteps =
    operationKind === "install" ||
    (operationKind === null &&
      history?.phases.some((phase) =>
        [
          "downloading",
          "verifying_artifact",
          "extracting",
          "verifying_application",
        ].includes(phase),
      ));
  const currentStep = installStepForPhase(currentPhase);
  const reportedSteps = (history?.phases ?? []).map(installStepForPhase);
  const stepStates = installSteps.map((_, index) => {
    if (index === 3) {
      if (installOutcome === "completed") return "已完成";
      if (installOutcome === "failed") return "失败";
      if (installOutcome === "cancelled") return "已取消";
      return history?.ended ? "待确认" : "待处理";
    }
    const reported = reportedSteps.includes(index);
    // Only observed groups can be marked complete, and only when a native
    // successor (not the array position) or a completed receipt proves it.
    if (
      reported &&
      (installOutcome === "completed" ||
        reportedSteps.some((step) => step > index))
    )
      return "已完成";
    if (!installOutcome && index === currentStep)
      return cancelRequested === status?.operationId ? "取消中" : "当前阶段";
    if (reported) return history?.ended ? "待确认" : "已报告";
    return installOutcome ||
      history?.ended ||
      reportedSteps.some((step) => step > index)
      ? "未记录"
      : "待处理";
  });
  const phaseMessage = statusUnavailable
    ? "暂时无法读取当前阶段，最近记录已保留；请重试。"
    : status?.operationId
      ? currentPhase
        ? phaseDetails(currentPhase).description
        : "安装器尚未提供当前阶段，请稍候…"
      : working
        ? "正在等待安装器报告阶段…"
        : history?.ended
          ? "操作已结束，请查看本地安装状态确认结果。"
          : status
            ? "当前没有进行中的安装器操作。"
            : "正在读取安装器状态…";
  useEffect(() => {
    onBusyChange?.(active || !!installConfirmation || !!restorePoint);
    return () => onBusyChange?.(false);
  }, [active, installConfirmation, restorePoint, onBusyChange]);
  useEffect(() => {
    onNavigationBlockedChange?.(!!installConfirmation || !!restorePoint);
    return () => onNavigationBlockedChange?.(false);
  }, [installConfirmation, restorePoint, onNavigationBlockedChange]);
  const restorePoints = local.data?.restorePoints ?? [];
  const installations = Array.from(
    new Map(
      (detection.data?.installations ?? []).map((installation) => [
        installation.path.replace(/[/]+$/, ""),
        installation,
      ]),
    ).values(),
  );
  const recoveryTargets = (detection.data?.recoveryTargets ?? []).filter(
    (path) => !installations.some((installation) => installation.path === path),
  );
  const managedPath = local.data?.targetPath.replace(/[/]+$/, "");
  const isManaged = (path: string) =>
    !!managedPath && path.replace(/[/]+$/, "") === managedPath;
  const label = !desktop
    ? "请在桌面客户端检测"
    : detection.isPending
      ? "正在检测安装状态…"
      : detection.isError
        ? "检测失败，请重试"
        : {
            detected: "已检测到安装",
            not_found: "未在标准目录发现安装",
            inspection_failed: "暂不能确定安装状态",
            unsupported: "当前平台暂未接入检测",
          }[detection.data.status];
  async function recover() {
    const reports = await invoke<InstallReport[]>(
      "osir_codex_install_recover",
      { ...(targetPath ? { targetPath } : {}) },
    );
    if (
      reports.some(
        (r) =>
          ![
            "completed",
            "rolled_back",
            "retired_completed",
            "retired_rolled_back",
          ].includes(r.step),
      )
    )
      throw new Error("恢复尚未完成，请检查安装目录；不要启动此安装。");
    void detection.refetch();
    return "已检查未完成的安装并执行恢复。";
  }
  const installedBuild = local.data?.installedBuild ?? 0;
  const comparisonComplete =
    !!release &&
    release.comparisonComplete !== false &&
    !!local.data &&
    release.targetPath === local.data.targetPath &&
    release.installedBuild === local.data.installedBuild;
  const canInstall =
    !!release?.release.updateAvailable &&
    (!release.source || release.source === source) &&
    release.release.build > installedBuild &&
    comparisonComplete &&
    !local.isError &&
    !local.isFetching &&
    !!local.data &&
    !local.data.recoveryRequired &&
    !local.data.updateBlockedReason;
  const otherInstallations = installations.filter(
    (installation) => !isManaged(installation.path),
  );
  const selectedVersion = installations.find((installation) =>
    isManaged(installation.path),
  )?.version;
  async function install(expected: NonNullable<typeof installConfirmation>) {
    if (
      !release ||
      expected.source !== source ||
      expected.build !== release.release.build ||
      expected.targetPath !== local.data?.targetPath ||
      expected.installedBuild !== local.data?.installedBuild ||
      !canInstall
    )
      throw new Error("版本、更新源或安装状态已改变，请重新检查。");
    const result = await invoke<InstallReport>("osir_codex_install_start", {
      source: expected.source,
      expectedBuild: expected.build,
      targetPath: expected.targetPath,
      expectedInstalledBuild: expected.installedBuild,
      expectedSource: expected.actualSource,
    });
    void detection.refetch();
    clearRelease();
    if (
      systemInstall &&
      result.step === "system_install_requested" &&
      result.installed === false &&
      result.build === null
    ) {
      setInstallConfirmation(null);
      return {
        message: "已打开 Microsoft Store，请完成安装或更新后重新检测。",
        handedOff: true as const,
      };
    }
    if (result.step !== "completed")
      throw new Error("安装未完成，请检查本地状态并恢复未完成的安装。");
    setInstallConfirmation(null);
    return "Codex 安装完成，会话与配置已保留。";
  }
  async function cancelOperation() {
    const operationId = status?.operationId;
    if (!operationId || !canCancel || cancelRequested === operationId) return;
    try {
      const latest = await invoke<InstallerStatus>("osir_codex_install_status");
      if (
        latest.operationId !== operationId ||
        !latest.phase ||
        !cancellablePhases.has(latest.phase)
      ) {
        toast.info("当前操作或阶段已变化，正在重新读取进度。");
        setStatusRetry((value) => value + 1);
        return;
      }
      const cancelled = await invoke<boolean>("osir_codex_install_cancel", {
        operationId,
      });
      if (cancelled) setCancelRequested(operationId);
      toast.info(
        cancelled
          ? "已请求取消，请等待清理。"
          : "取消未生效，请等待当前操作结束。",
      );
    } catch {
      toast.error("取消未确认，请等待当前操作结束。");
    }
  }
  const showProgress =
    active ||
    statusUnavailable ||
    !!installOutcome ||
    history?.phases.some((phase) =>
      [
        "downloading",
        "verifying_artifact",
        "extracting",
        "verifying_application",
      ].includes(phase),
    );
  const confirmationOpen = !!installConfirmation || !!restorePoint;
  return (
    <section
      className="osir-desktop-panel osir-managed-desktop"
      aria-label="Codex Desktop 安装状态"
    >
      <header className="osir-desktop-heading">
        <SupportedToolIcon id="codex" size={28} decorative />
        <div>
          <h2>Codex Desktop</h2>
          <p className="osir-desktop-note">
            {systemInstall
              ? "检测本机版本，通过 Microsoft Store 安装或更新 Codex。"
              : "检测本机版本，下载或更新 Codex。"}
          </p>
        </div>
        <button
          type="button"
          onClick={() => {
            void detection.refetch();
            void local.refetch();
          }}
          disabled={
            !desktop ||
            detection.isFetching ||
            local.isFetching ||
            active ||
            confirmationOpen
          }
          aria-label="刷新 Desktop 安装状态"
        >
          <RefreshCw size={14} aria-hidden="true" />
          重新检测
        </button>
      </header>
      <div className="osir-desktop-overview">
        <div className="osir-desktop-current" aria-label="当前版本">
          <span className="osir-desktop-note">当前安装版本</span>
          <strong>
            {local.isError
              ? "暂不可用"
              : local.data?.installedBuild
                ? selectedVersion
                  ? `${selectedVersion} · 构建 ${local.data.installedBuild}`
                  : `构建 ${local.data.installedBuild}`
                : local.isPending && desktop
                  ? "正在读取…"
                  : "尚未安装"}
          </strong>
          <p role="status" className="osir-desktop-note">
            {label}
          </p>
        </div>
        <label className="osir-desktop-source">
          更新源
          <select
            aria-label="Desktop 更新源"
            value={source}
            disabled={!desktop || active || confirmationOpen}
            onChange={(event) => {
              setSource(event.target.value as CodexInstallSource);
              setMessage("");
              setFailed(false);
            }}
          >
            <option value="auto">自动选择</option>
            {!systemInstall && <option value="osir_mirror">OSIR 下载源</option>}
            <option value="official">官方更新源</option>
          </select>
        </label>
      </div>
      {desktop && !systemInstall && (
        <label className="osir-desktop-target">
          安装位置
          <select
            aria-label="Codex 安装位置"
            value={targetPath ?? ""}
            disabled={active || confirmationOpen || tools.isPending}
            onChange={(event) => {
              setSelectedTarget(event.target.value);
              setMessage("");
              setFailed(false);
            }}
          >
            <option value="">安装到用户目录</option>
            {installations.map((installation) => (
              <option key={installation.path} value={installation.path}>
                {installation.path} · {installation.version ?? "版本未知"}
              </option>
            ))}
            {recoveryTargets.map((path) => (
              <option key={path} value={path}>
                {path} · 已保存安装记录
              </option>
            ))}
          </select>
        </label>
      )}
      {!desktop && (
        <p className="osir-desktop-note">浏览器预览不读取本机安装。</p>
      )}
      {desktop && (
        <>
          {release && (
            <div className="osir-desktop-release" aria-label="更新检查结果">
              <strong>
                {release.release.version} · 构建 {release.release.build}
              </strong>
              <span>
                {CODEX_INSTALL_SOURCES[release.actualSource ?? source]} · 约{" "}
                {Math.ceil(release.release.fullSize / 1024 / 1024)} MB
              </span>
              {!canInstall && !active && (
                <p className="osir-desktop-note">
                  {!comparisonComplete
                    ? "安装状态已变化，请重新检查更新。"
                    : local.data?.updateBlockedReason
                      ? local.data.updateBlockedReason
                      : release.release.build <= installedBuild
                        ? "已是最新版本。"
                        : "请先核对本地安装状态。"}
                </p>
              )}
            </div>
          )}
          <div className="osir-desktop-actions">
            <button
              type="button"
              aria-label="检查可安装版本"
              disabled={active || confirmationOpen}
              onClick={() => void run(check)}
            >
              <RefreshCw size={14} aria-hidden="true" />
              检查更新
            </button>
            {release && (
              <button
                type="button"
                className="primary"
                disabled={active || confirmationOpen || !canInstall}
                onClick={() => {
                  if (!canInstall || !local.data) return;
                  setFailed(false);
                  setMessage("");
                  setInstallConfirmation({
                    source,
                    build: release.release.build,
                    version: release.release.version,
                    targetPath: local.data.targetPath,
                    installedBuild: local.data.installedBuild,
                    actualSource: release.actualSource ?? source,
                  });
                }}
              >
                <Download size={14} aria-hidden="true" />
                {systemInstall
                  ? "打开 Microsoft Store"
                  : local.data?.installedBuild
                    ? "更新 Codex"
                    : "下载并安装"}
              </button>
            )}
            <ToolLaunchButton
              tool="codex"
              installation={currentCodex}
              desktop={desktop}
              disabled={
                active ||
                confirmationOpen ||
                tools.isError ||
                tools.isPending ||
                local.data?.recoveryRequired === true
              }
            />
          </div>
          {local.isPending && (
            <p role="status" className="osir-desktop-note">
              正在读取本地安装状态…
            </p>
          )}
          {local.isError && !local.isFetching && !active && (
            <p role="alert">本地安装状态不可用，请刷新或恢复未完成的安装。</p>
          )}
          {local.data?.recoveryRequired && (
            <div className="osir-desktop-recovery" role="alert">
              <p>存在未完成的安装，请先恢复后再更新或打开。</p>
              <button
                type="button"
                disabled={active || confirmationOpen}
                onClick={() => void run(recover)}
              >
                <RotateCcw size={14} aria-hidden="true" />
                恢复未完成的安装
              </button>
            </div>
          )}
          {message && (
            <p
              role={failed ? "alert" : "status"}
              className="osir-desktop-feedback"
            >
              {message}
            </p>
          )}
          {showProgress && (
            <section className="osir-desktop-operation" aria-label="安装阶段">
              <p role="status" className="osir-desktop-note">
                {phaseMessage}
              </p>
              {showInstallSteps ? (
                <ol
                  className="osir-installer-phases osir-installer-steps"
                  aria-label="安装流程"
                >
                  {installSteps.map((step, index) => {
                    const state = stepStates[index];
                    const current = state === "当前阶段" || state === "取消中";
                    return (
                      <li
                        key={step}
                        aria-current={current ? "step" : undefined}
                        data-state={state}
                      >
                        <span
                          className="osir-installer-phase-marker"
                          aria-hidden="true"
                        >
                          {state === "已完成" ? <Check size={13} /> : index + 1}
                        </span>
                        <span>{step}</span>
                        <span className="osir-installer-phase-state">
                          {state}
                        </span>
                      </li>
                    );
                  })}
                </ol>
              ) : (
                history && (
                  <ol
                    className="osir-installer-phases"
                    aria-label="已报告的处理阶段"
                  >
                    {history.phases.map((phase) => (
                      <li
                        key={phase}
                        aria-current={
                          !history.ended && phase === currentPhase
                            ? "step"
                            : undefined
                        }
                      >
                        <span>{phaseDetails(phase).label}</span>
                        <span>
                          {!history.ended && phase === currentPhase
                            ? "当前阶段"
                            : "已报告"}
                        </span>
                      </li>
                    ))}
                  </ol>
                )
              )}
              <div className="osir-desktop-actions">
                {statusUnavailable && (
                  <button
                    type="button"
                    onClick={() => setStatusRetry((value) => value + 1)}
                  >
                    重试读取进度
                  </button>
                )}
                {active && (
                  <button
                    type="button"
                    disabled={
                      !canCancel || cancelRequested === status?.operationId
                    }
                    onClick={() => void cancelOperation()}
                  >
                    {cancelRequested === status?.operationId && cancelRequested
                      ? "正在取消…"
                      : currentPhase === "downloading"
                        ? "取消下载"
                        : "取消操作"}
                  </button>
                )}
              </div>
              {active && !canCancel && (
                <p className="osir-desktop-note">
                  {currentPhase === "applying"
                    ? "应用更改期间不能取消，请保持客户端打开。"
                    : "当前阶段暂不可取消，请等待进度更新。"}
                </p>
              )}
            </section>
          )}
          <details className="osir-desktop-advanced">
            <summary>高级与恢复</summary>
            <section className="osir-desktop-information" aria-label="安装信息">
              <div className="osir-section-title">
                <h3>安装信息</h3>
                <button
                  type="button"
                  disabled={active || confirmationOpen || local.isFetching}
                  onClick={() => void local.refetch()}
                >
                  刷新本地状态
                </button>
              </div>
              <dl className="osir-desktop-info-grid">
                <div>
                  <dt>所选安装位置</dt>
                  <dd>
                    <code>{local.data?.targetPath || "暂不可用"}</code>
                  </dd>
                </div>
                <div>
                  <dt>下载来源</dt>
                  <dd>{CODEX_INSTALL_SOURCES[source]}</dd>
                </div>
                <div>
                  <dt>校验状态</dt>
                  <dd>
                    {local.isFetching
                      ? "正在校验…"
                      : local.isError
                        ? "暂不可用，请刷新本地状态"
                        : local.data?.recoveryRequired
                          ? "存在未完成安装，请先恢复"
                          : local.data?.installedBuild
                            ? systemInstall
                              ? `已检测到系统安装 · 版本 ${selectedVersion || "未知"}`
                              : `上次校验通过 · 构建 ${local.data.installedBuild}`
                            : "所选目录尚未安装"}
                  </dd>
                </div>
                <div>
                  <dt>版本来源</dt>
                  <dd>本机应用</dd>
                </div>
              </dl>
              {installations.map((installation) => (
                <div className="osir-desktop-version" key={installation.path}>
                  <span>
                    {isManaged(installation.path) ? "当前选择" : "其他安装"}
                  </span>
                  <strong>版本 {installation.version || "未知"}</strong>
                  <span>构建 {installation.build || "未知"}</span>
                  <code>{installation.path}</code>
                </div>
              ))}
              {otherInstallations.length > 0 && (
                <p className="osir-desktop-note">
                  仅更新所选安装位置，其他目录中的应用保持不变。
                </p>
              )}
              <p className="osir-desktop-note">
                外部安装或更新后，请重新检测。
              </p>
              <div className="osir-desktop-actions">
                {!local.data?.recoveryRequired && (
                  <button
                    type="button"
                    disabled={active || confirmationOpen}
                    onClick={() => void run(recover)}
                  >
                    <RotateCcw size={14} aria-hidden="true" />
                    恢复未完成的安装
                  </button>
                )}
                <button
                  type="button"
                  disabled={active || confirmationOpen || !local.data}
                  onClick={() =>
                    void run(async () => {
                      await invoke("osir_codex_install_local", {
                        action: "reveal",
                        ...(targetPath ? { targetPath } : {}),
                      });
                      return "已打开安装目录。";
                    })
                  }
                >
                  <Folder size={14} aria-hidden="true" />
                  打开安装目录
                </button>
              </div>
            </section>
            {!systemInstall && (
              <section
                className="osir-desktop-restore"
                aria-label="历史版本与恢复点"
              >
                <h3>历史版本与恢复点</h3>
                <p className="osir-desktop-note">
                  仅恢复应用版本，不包含会话历史或配置。
                </p>
                <div className="osir-desktop-restore-table">
                  <table>
                    <thead>
                      <tr>
                        <th scope="col">可恢复版本</th>
                        <th scope="col">来源</th>
                        <th scope="col">保存时间</th>
                        <th scope="col">操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {restorePoints.map((point) => (
                        <tr key={point.receiptId}>
                          <td>
                            构建 {point.previousBuild}
                            <span className="osir-desktop-restore-context">
                              安装构建 {point.targetBuild}
                            </span>
                          </td>
                          <td>更新前应用备份</td>
                          <td>未提供</td>
                          <td>
                            <button
                              type="button"
                              disabled={
                                active ||
                                confirmationOpen ||
                                local.isFetching ||
                                local.isError ||
                                local.data?.recoveryRequired
                              }
                              onClick={() => {
                                setFailed(false);
                                setRestorePoint(point);
                              }}
                            >
                              恢复构建 {point.previousBuild}
                            </button>
                          </td>
                        </tr>
                      ))}
                      {!restorePoints.length && (
                        <tr>
                          <td colSpan={4}>
                            {local.isError
                              ? "恢复记录暂不可用，请刷新本地状态。"
                              : local.isFetching
                                ? "正在读取恢复记录…"
                                : local.data?.recoveryRequired
                                  ? "请先恢复未完成的安装，再查看恢复点。"
                                  : "暂无已保存且可恢复的版本。"}
                          </td>
                        </tr>
                      )}
                    </tbody>
                  </table>
                </div>
              </section>
            )}
          </details>
          <ConfirmDialog
            className="osir-tool-update-dialog"
            isOpen={installConfirmation !== null}
            title={
              systemInstall
                ? "打开 Microsoft Store"
                : local.data?.installedBuild
                  ? "更新 Codex"
                  : "安装 Codex"
            }
            message={
              systemInstall
                ? "安装和更新由 Microsoft Store 完成，完成后请返回重新检测。"
                : `将从${installConfirmation ? CODEX_INSTALL_SOURCES[installConfirmation.actualSource] : ""}下载 ${installConfirmation?.version ?? ""}（构建 ${installConfirmation?.build ?? ""}），安装至 ${installConfirmation?.targetPath ?? ""}。请先退出此 Codex；更新前会备份现有版本，并保留会话与配置。${failed && message ? "\n" + message : ""}`
            }
            confirmText={systemInstall ? "打开商店" : "确认安装"}
            cancelText="取消"
            variant="info"
            pending={active}
            onCancel={() => setInstallConfirmation(null)}
            onConfirm={() => {
              if (!installConfirmation || active) return;
              const expected = installConfirmation;
              setInstallConfirmation(null);
              void run(() => install(expected), "install");
            }}
          />
          <ConfirmDialog
            className="osir-tool-update-dialog"
            isOpen={restorePoint !== null}
            title="恢复 Desktop 版本"
            message={`将恢复到构建 ${restorePoint?.previousBuild ?? ""}。请先退出此 Desktop。仅替换所示安装目录中的应用，不恢复会话历史或配置。${failed ? "\n" + message : ""}`}
            confirmText="确认恢复"
            cancelText="取消"
            pending={active}
            onCancel={() => setRestorePoint(null)}
            onConfirm={() => {
              if (!restorePoint || active) return;
              const expected = restorePoint;
              setRestorePoint(null);
              void run(async () => {
                const result = await invoke<InstallReport>(
                  "osir_codex_install_restore",
                  { expected, ...(targetPath ? { targetPath } : {}) },
                );
                void detection.refetch();
                clearRelease();
                if (result.step !== "rolled_back")
                  throw new Error("恢复未完成，请检查本地状态。");
                setRestorePoint(null);
                return "已恢复上一版本，请重新检测。";
              });
            }}
          />
        </>
      )}
    </section>
  );
}
