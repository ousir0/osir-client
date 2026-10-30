import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ArrowUpCircle, Loader2, RefreshCw } from "lucide-react";
import {
  TOOL_NAMES,
  toolDisplayName,
  useToolManagement,
  type ToolName,
  type ToolVersion,
} from "@/components/apps/useToolManagement";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ToolUpgradeConfirmDialog } from "@/components/settings/ToolUpgradeConfirmDialog";
import { isUpdateAvailable } from "@/lib/version";
import {
  LOCAL_TOOLS_QUERY,
  type LocalToolInstallation,
  type SupportedToolId,
} from "./toolsApi";
import {
  codexDesktopReleaseKey,
  useCodexInstallSource,
  type CodexInstallSource,
} from "./codexDesktopApi";
import { ToolLifecycleResults } from "./ToolLifecycleResults";
import "./tool-runtime-actions.css";

export const TOOL_UPDATES_QUERY = ["osir-tool-update-check"] as const;
export const CODEX_DESKTOP_UPDATE_QUERY = ["osir-codex-update-check"] as const;

type DesktopReleaseCheck = {
  source: CodexInstallSource;
  installedBuild: number | null;
  highestDetectedBuild: number | null;
  comparisonComplete: boolean;
  release: { build: number; version: string; updateAvailable: boolean };
};
type Update = { current: string; latest: string; desktop: boolean };
type CheckResult = { checked: boolean; unavailable: boolean };
type ToolUpdatesContextValue = {
  checking: boolean;
  blocked: boolean;
  result: CheckResult | undefined;
  check: () => void;
  updateFor: (
    tool: SupportedToolId,
    installation?: LocalToolInstallation,
  ) => Update | null;
  busy: (tool: SupportedToolId) => boolean;
  requestUpdate: (tool: SupportedToolId) => void;
  retryAction: (tool: ToolName, action: "install" | "update") => void;
};
const ToolUpdatesContext = createContext<ToolUpdatesContextValue | null>(null);

function cliName(installation: LocalToolInstallation): ToolName | null {
  if (
    !installation.installed ||
    installation.error ||
    installation.status !== "installed" ||
    installation.kind === "desktop" ||
    (installation.id === "codex" && installation.kind !== "cli") ||
    !TOOL_NAMES.includes(installation.id as ToolName)
  )
    return null;
  return installation.id as ToolName;
}

function cliUpdate(
  info?: ToolVersion,
  detectedVersion?: string | null,
): Update | null {
  if (
    !info ||
    info.error ||
    info.installed_but_broken ||
    info.update_blocked_by_node ||
    !isUpdateAvailable(info.version, info.latest_version) ||
    !isUpdateAvailable(detectedVersion ?? info.version, info.latest_version)
  )
    return null;
  return {
    current: info.version!,
    latest: info.latest_version!,
    desktop: false,
  };
}

/** One controller per overview; a single stale-gated check covers installed CLIs. */
export function ToolUpdatesProvider({
  desktop,
  installations,
  children,
  onManageDesktop,
  onBusyChange,
  onNavigationBlockedChange,
}: {
  desktop: boolean;
  installations: LocalToolInstallation[];
  children: ReactNode;
  /** Open the existing Codex safe installer; never use CLI updates for Desktop. */
  onManageDesktop?: () => void;
  onBusyChange?: (busy: boolean) => void;
  onNavigationBlockedChange?: (blocked: boolean) => void;
}) {
  const tools = useToolManagement({ autoLoad: false });
  const queryClient = useQueryClient();
  const { source } = useCodexInstallSource();
  const [confirmation, setConfirmation] = useState<ToolName | null>(null);
  const confirmationRef = useRef<ToolName | null>(null);
  const retryConfirmationRef = useRef(false);
  const installed = useMemo(
    () => new Map(installations.map((info) => [info.id, info])),
    [installations],
  );
  const cliTools = TOOL_NAMES.filter((name) => {
    const info = installed.get(name);
    return info && cliName(info) === name;
  });
  const codex = installed.get("codex");
  const checkDesktop =
    !!codex?.installed &&
    !codex.error &&
    codex.status === "installed" &&
    codex.kind !== "cli";
  const desktopRelease = useQuery({
    queryKey: codexDesktopReleaseKey(source, codex?.path ?? undefined),
    queryFn: () =>
      invoke<DesktopReleaseCheck>("osir_codex_install_check", {
        source,
        ...(codex?.path ? { targetPath: codex.path } : {}),
      }),
    enabled: desktop && checkDesktop,
    retry: false,
    refetchOnWindowFocus: false,
    staleTime: 10 * 60_000,
  });
  const sharedVersionsFresh =
    cliTools.length > 0 &&
    tools.lastCheckedAt !== null &&
    Date.now() - tools.lastCheckedAt < 10 * 60_000 &&
    cliTools.every((name) => tools.toolVersionByName.has(name));
  const checks = useQuery({
    queryKey: [...TOOL_UPDATES_QUERY, ...cliTools],
    enabled: desktop && cliTools.length > 0,
    retry: false,
    refetchOnWindowFocus: false,
    staleTime: 10 * 60_000,
    initialData: sharedVersionsFresh
      ? {
          checked: true,
          unavailable: cliTools.some((name) => {
            const info = tools.toolVersionByName.get(name);
            return !!info?.error || !info?.latest_version;
          }),
        }
      : undefined,
    initialDataUpdatedAt: tools.lastCheckedAt ?? undefined,
    queryFn: async (): Promise<CheckResult> => {
      const versions = cliTools.length
        ? await tools.refreshToolVersions(cliTools)
        : [];
      return {
        checked: cliTools.length > 0,
        unavailable:
          versions.length !== cliTools.length ||
          versions.some((info) => !!info.error || !info.latest_version),
      };
    },
  });
  const checking = checks.isFetching || desktopRelease.isFetching;
  const blocked =
    !desktop || tools.isAnyBusy || !!tools.pendingUpgrade || !!confirmation;
  useEffect(() => {
    const working = tools.isAnyBusy || !!tools.pendingUpgrade || !!confirmation;
    onBusyChange?.(working);
    return () => onBusyChange?.(false);
  }, [tools.isAnyBusy, tools.pendingUpgrade, confirmation, onBusyChange]);
  useEffect(() => {
    // Installation is owned by the shared controller after confirmation.
    onNavigationBlockedChange?.(!!tools.pendingUpgrade || !!confirmation);
    return () => onNavigationBlockedChange?.(false);
  }, [tools.pendingUpgrade, confirmation, onNavigationBlockedChange]);
  function refreshInventory() {
    // The existing updater refreshes the shared version store itself. Run this
    // on settlement even after unmount: failed commands can also change files.
    void queryClient.invalidateQueries({ queryKey: LOCAL_TOOLS_QUERY });
    void queryClient.invalidateQueries({
      queryKey: TOOL_UPDATES_QUERY,
      refetchType: "none",
    });
  }

  function updateFor(
    tool: SupportedToolId,
    installation = installed.get(tool),
  ): Update | null {
    if (!desktop || !installation?.installed || installation.error) return null;
    const cli = cliName(installation);
    if (cli)
      return cliUpdate(tools.toolVersionByName.get(cli), installation.version);
    const release = desktopRelease.data;
    if (
      tool !== "codex" ||
      !checkDesktop ||
      !onManageDesktop ||
      !release ||
      release.source !== source ||
      release.comparisonComplete !== true ||
      !Number.isSafeInteger(release.highestDetectedBuild) ||
      !release.highestDetectedBuild ||
      release.highestDetectedBuild <= 0 ||
      !Number.isSafeInteger(release.release.build) ||
      release.release.build <= release.highestDetectedBuild ||
      release.release.updateAvailable !== true ||
      desktopRelease.isError
    )
      return null;
    return {
      current: `构建 ${release.highestDetectedBuild}`,
      latest: release.release.version,
      desktop: true,
    };
  }
  function requestUpdate(tool: SupportedToolId) {
    if (blocked || checking) return;
    const update = updateFor(tool);
    if (!update) return;
    if (update.desktop) {
      onManageDesktop?.();
      return;
    }
    confirmationRef.current = tool as ToolName;
    retryConfirmationRef.current = false;
    setConfirmation(tool as ToolName);
  }
  function cancel() {
    confirmationRef.current = null;
    retryConfirmationRef.current = false;
    setConfirmation(null);
  }
  function confirm() {
    const name = confirmationRef.current;
    const retry = retryConfirmationRef.current;
    if (!name) return;
    cancel();
    if ((!retry && !updateFor(name)) || tools.busyTools.has(name)) return;
    void tools.handleRunToolAction([name], "update").finally(refreshInventory);
  }
  const selected = confirmation
    ? tools.toolVersionByName.get(confirmation)
    : undefined;
  return (
    <ToolUpdatesContext.Provider
      value={{
        checking,
        blocked,
        result:
          checks.data || desktopRelease.data || desktopRelease.isError
            ? {
                checked:
                  !!checks.data?.checked ||
                  !!desktopRelease.data ||
                  desktopRelease.isError,
                unavailable:
                  !!checks.data?.unavailable || desktopRelease.isError,
              }
            : undefined,
        check: () => {
          if (
            desktop &&
            !blocked &&
            queryClient.isFetching({ queryKey: TOOL_UPDATES_QUERY }) === 0 &&
            queryClient.isFetching({ queryKey: CODEX_DESKTOP_UPDATE_QUERY }) ===
              0
          )
            void Promise.all([
              checks.refetch(),
              checkDesktop ? desktopRelease.refetch() : null,
            ]);
        },
        updateFor,
        busy: (tool) => tools.busyTools.has(tool as ToolName),
        requestUpdate,
        retryAction: (tool, action) => {
          if (blocked || checking) return;
          if (action === "update") {
            confirmationRef.current = tool;
            retryConfirmationRef.current = true;
            setConfirmation(tool);
          } else {
            void tools
              .handleRunToolAction([tool], action)
              .finally(refreshInventory);
          }
        },
      }}
    >
      {children}
      <ConfirmDialog
        className="osir-tool-update-dialog"
        isOpen={confirmation !== null}
        title={`更新 ${confirmation ? toolDisplayName(confirmation) : "工具"}`}
        message={
          retryConfirmationRef.current
            ? "将重新执行更新并检测本机版本。请先保存该工具中未完成的工作。"
            : `将当前版本 ${selected?.version ?? ""} 更新到 ${selected?.latest_version ?? ""}。请先保存该工具中未完成的工作。`
        }
        confirmText="确认更新"
        cancelText="取消"
        variant="info"
        onConfirm={confirm}
        onCancel={cancel}
      />
      <ToolUpgradeConfirmDialog
        className="osir-tool-update-dialog"
        isOpen={tools.pendingUpgrade !== null}
        plans={tools.pendingUpgrade?.plans ?? []}
        displayName={toolDisplayName}
        onConfirm={() => {
          const task = tools.handleConfirmUpgrade();
          if (task) void task.finally(refreshInventory);
        }}
        onCancel={tools.handleCancelUpgrade}
      />
    </ToolUpdatesContext.Provider>
  );
}

export function ToolUpdateResults() {
  const updates = useContext(ToolUpdatesContext);
  if (!updates) return null;
  return (
    <ToolLifecycleResults
      disabled={updates.blocked || updates.checking}
      onRetry={(result) => {
        if (result.action !== "uninstall")
          updates.retryAction(result.tool, result.action);
      }}
    />
  );
}

/** Force a fresh check; installed CLI metadata otherwise shares a ten-minute cache. */
export function ToolUpdatesCheckButton({
  className,
  disabled = false,
  iconOnly = false,
}: {
  className?: string;
  disabled?: boolean;
  iconOnly?: boolean;
}) {
  const updates = useContext(ToolUpdatesContext);
  if (!updates) return null;
  return (
    <>
      <button
        type="button"
        className={className}
        disabled={disabled || updates.blocked || updates.checking}
        aria-busy={updates.checking || undefined}
        aria-label={iconOnly ? "检查更新" : undefined}
        title={
          iconOnly
            ? updates.result?.unavailable
              ? "部分工具版本暂不可用，点击重试"
              : "检查工具版本"
            : undefined
        }
        onClick={updates.check}
      >
        <RefreshCw
          size={15}
          aria-hidden="true"
          className={updates.checking ? "animate-spin" : undefined}
        />
        {!iconOnly && (updates.checking ? "正在检查…" : "检查更新")}
      </button>
      {updates.result && !updates.checking && (
        <span
          role="status"
          className={
            iconOnly || (updates.result.checked && !updates.result.unavailable)
              ? "sr-only"
              : "osir-tool-update-status"
          }
        >
          {!updates.result.checked
            ? "已安装工具暂不支持此更新检查。"
            : updates.result.unavailable
              ? "部分工具的版本暂不可用，请稍后重试。"
              : "版本检查完成。"}
        </span>
      )}
    </>
  );
}

/** Cached evidence only: unsupported and unknown updates render no icon. */
export function ToolUpdateButton({
  tool,
  installation,
  desktop,
  disabled = false,
  className,
  onBusyChange,
}: {
  tool: SupportedToolId;
  installation?: LocalToolInstallation;
  desktop: boolean;
  disabled?: boolean;
  className?: string;
  onBusyChange?: (busy: boolean) => void;
}) {
  const updates = useContext(ToolUpdatesContext);
  const busy = updates?.busy(tool) ?? false;
  useEffect(() => {
    if (!busy) return;
    onBusyChange?.(true);
    return () => onBusyChange?.(false);
  }, [busy, onBusyChange]);
  const update = desktop ? updates?.updateFor(tool, installation) : null;
  if (!updates || !update) return null;
  const name = update.desktop ? "Codex Desktop" : toolDisplayName(tool);
  return (
    <button
      type="button"
      className={["osir-tool-update", className].filter(Boolean).join(" ")}
      disabled={disabled || busy || updates.blocked || updates.checking}
      aria-label={busy ? `${name} 正在更新` : `更新 ${name}`}
      aria-busy={busy || undefined}
      title={
        update.desktop
          ? `发现 ${update.latest}，打开 Codex Desktop 安装与更新管理`
          : `从 ${update.current} 更新到 ${update.latest}`
      }
      onClick={() => updates.requestUpdate(tool)}
    >
      {busy ? (
        <Loader2 size={15} className="animate-spin" aria-hidden="true" />
      ) : (
        <ArrowUpCircle size={15} aria-hidden="true" />
      )}
    </button>
  );
}
