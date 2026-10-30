import { useCallback, useEffect, useRef, useState } from "react";
import { useIsMutating, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  useToolManagement,
  toolDisplayName,
  TOOL_NAMES as TOOL_MANAGEMENT_NAMES,
  type ToolName,
} from "@/components/apps/useToolManagement";
import { ToolUpgradeConfirmDialog } from "@/components/settings/ToolUpgradeConfirmDialog";
import { ToolInstallRow } from "@/components/settings/ToolInstallRow";
import { toolLifecycleFailureReason } from "@/components/apps/toolLifecycleResults";
import { ToolLifecycleResults } from "./ToolLifecycleResults";
import { Button } from "@/components/ui/button";
import { isUpdateAvailable } from "@/lib/version";
import { SessionManagerPage } from "@/components/sessions/SessionManagerPage";
import { useSettings } from "@/hooks/useSettings";
import {
  ArrowUpCircle,
  CheckCircle2,
  Download,
  Loader2,
  RefreshCw,
  Terminal,
  TriangleAlert,
} from "lucide-react";
import { backupsApi, type AppId } from "@/lib/api";
import { CodexDesktopPanel } from "./CodexDesktopPanel";
import { OsirExtensionsWorkspace } from "./OsirExtensionsWorkspace";
import { OsirHistoryInspection } from "./OsirHistoryInspection";
import { OsirTabs } from "./OsirTabs";
import { connectionError } from "./errors";
import { SupportedToolIcon } from "./SupportedToolIcon";
import { BackupScope } from "./BackupRecoveryDialog";
import { OsirLocalBackupList } from "./OsirLocalBackupList";
import { OsirBackupTransfer } from "./OsirBackupTransfer";
import { ToolConfigBackups } from "./ToolConfigBackups";
import { ToolLaunchButton } from "./ToolLaunchButton";
import type { LocalToolInstallation } from "./toolsApi";
import { BackupStorageSection } from "@/components/settings/BackupStorageSection";
import "./management.css";

export type OsirManagementSection =
  | "versions"
  | "extensions"
  | "backup"
  | "history";
export interface OsirToolManagementProps {
  section: OsirManagementSection;
  desktop: boolean;
  appId?: AppId;
  /** Tool pages provide their own title and primary navigation. */
  embedded?: boolean;
  onOpenTerminalSettings?: () => void;
  /** Host must honor this when navigating away from an active editor/import. */
  onNavigationBlockedChange?: (blocked: boolean) => void;
}

const headings: Record<OsirManagementSection, [string, string]> = {
  versions: ["安装与版本", "管理本机工具，检查更新与恢复记录。"],
  extensions: ["MCP 与技能", "管理工具服务与技能，按需启用。"],
  backup: ["备份与恢复", "保存本地配置，在需要时选择恢复。"],
  history: ["历史会话", "按工具浏览本机保存的对话与项目记录。"],
};

export function OsirToolManagement(props: OsirToolManagementProps) {
  return <ManagementPage key={props.section} {...props} />;
}

function ManagementPage({
  section,
  desktop,
  appId = "codex",
  embedded = false,
  onOpenTerminalSettings,
  onNavigationBlockedChange,
}: OsirToolManagementProps) {
  const [tab, setTab] = useState(
    section === "extensions"
      ? "mcp"
      : embedded && appId !== "codex"
        ? "cli"
        : "desktop",
  );
  const [interactionBlocked, setInteractionBlocked] = useState(false);
  const blocked = interactionBlocked;
  useEffect(() => {
    onNavigationBlockedChange?.(blocked);
    return () => onNavigationBlockedChange?.(false);
  }, [blocked, onNavigationBlockedChange]);
  const [title, description] = headings[section];
  const tabs =
    section === "versions"
      ? embedded && appId !== "codex"
        ? [{ value: "cli", label: "安装与更新" }]
        : [
            { value: "desktop", label: "Desktop" },
            { value: "cli", label: "CLI" },
          ]
      : [
          { value: "mcp", label: "MCP 服务" },
          { value: "skills", label: "Skills 技能" },
        ];
  return (
    <section
      className={
        "osir-management osir-management-" +
        section +
        (embedded ? " osir-management-embedded" : "")
      }
      aria-label={title}
    >
      {!embedded && (
        <header className="osir-management-heading">
          <h1>{title}</h1>
          <p>{description}</p>
        </header>
      )}
      {section === "versions" && !embedded && (
        <fieldset className="osir-management-tabs" disabled={blocked}>
          <legend className="sr-only">{title}</legend>
          <OsirTabs
            label={title}
            value={tab}
            items={tabs}
            onChange={(value) => {
              if (!blocked) {
                setTab(value);
              }
            }}
          />
        </fieldset>
      )}
      {blocked && (
        <p className="osir-management-notice" role="status">
          请先完成或取消当前编辑与操作，再离开此页。
        </p>
      )}
      {!desktop ? (
        <div className="osir-management-card">
          <p role="status">
            请在 OSIR 桌面客户端管理本机工具与数据。浏览器预览不读取本机配置。
          </p>
        </div>
      ) : (
        <div className="osir-management-content">
          {section === "versions" &&
            (tab === "desktop" ? (
              <CodexDesktopPanel
                desktop={desktop}
                onNavigationBlockedChange={setInteractionBlocked}
              />
            ) : (
              <CliManagement
                onBlockedChange={setInteractionBlocked}
                toolIds={
                  embedded &&
                  ["codex", "claude", "opencode", "gemini"].includes(appId)
                    ? [appId as "codex" | "claude" | "opencode" | "gemini"]
                    : undefined
                }
              />
            ))}
          {section === "extensions" && (
            <OsirExtensionsWorkspace
              desktop={desktop}
              onNavigationBlockedChange={setInteractionBlocked}
            />
          )}
          {section === "backup" && (
            <BackupManagement onBlockedChange={setInteractionBlocked} />
          )}
          {section === "history" && (
            <SessionManagerPage
              key={appId}
              appId={appId}
              fromApp={appId}
              osirVariant
              scopedToApp
              onOpenTerminalSettings={onOpenTerminalSettings}
              inspectionPlacement={appId === "codex" ? "side" : "above"}
              renderSessionInspection={
                appId === "codex"
                  ? (session) => (
                      <OsirHistoryInspection
                        session={session}
                        desktop={desktop}
                      />
                    )
                  : undefined
              }
            />
          )}
        </div>
      )}
    </section>
  );
}

function cliDetectionMessage(error: unknown, needsRepair: boolean) {
  switch (toolLifecycleFailureReason(error)) {
    case "incompleteInstall":
      return "安装不完整，请修复安装。";
    case "node":
      return "当前 Node 版本不兼容，请更新 Node 后重试。";
    case "permission":
      return "无权读取工具版本，请检查安装权限后重新检测。";
    case "timeout":
      return "版本检测超时，请重新检测。";
    case "network":
    case "dns":
      return "无法完成版本检测，请检查网络后重试。";
    default:
      return needsRepair
        ? "工具无法运行，请修复安装。"
        : "无法读取版本，请重新检测。";
  }
}

export function CliManagement({
  onBlockedChange,
  toolIds = TOOL_MANAGEMENT_NAMES,
  showLifecycleResults = true,
  showUpgradeConfirmation = true,
}: {
  onBlockedChange: (value: boolean) => void;
  toolIds?: readonly ToolName[];
  showLifecycleResults?: boolean;
  /** Disable when the surrounding ToolUpdatesProvider owns this dialog. */
  showUpgradeConfirmation?: boolean;
}) {
  const tools = useToolManagement();
  const [launchBusy, setLaunchBusy] = useState(false);
  useEffect(() => {
    onBlockedChange(!!tools.pendingUpgrade || launchBusy);
    return () => onBlockedChange(false);
  }, [tools.pendingUpgrade, launchBusy, onBlockedChange]);
  return (
    <div className="osir-management-cli">
      <div className="osir-management-tool-heading">
        <div>
          <h2>
            <Terminal size={18} aria-hidden="true" />
            开发工具 CLI
          </h2>
          <p className="osir-cli-caption">
            检测本机安装与可用版本，按工具安装或更新。
          </p>
        </div>
        <Button
          variant="neutral"
          disabled={tools.isLoadingTools || tools.isAnyBusy || launchBusy}
          onClick={() => void tools.checkForUpdates()}
        >
          <RefreshCw
            size={15}
            aria-hidden="true"
            className={tools.isLoadingTools ? "animate-spin" : undefined}
          />
          {tools.isLoadingTools ? "检测中…" : "检查更新"}
        </Button>
      </div>
      {showLifecycleResults && (
        <ToolLifecycleResults
          toolIds={toolIds}
          disabled={tools.isAnyBusy || !!tools.pendingUpgrade || launchBusy}
          onRetry={(result) =>
            void tools.handleRunToolAction([result.tool], result.action)
          }
        />
      )}
      {toolIds.map((tool) => {
        const info = tools.toolVersionByName.get(tool);
        const busy = tools.busyTools.has(tool);
        const loading =
          !!tools.loadingTools[tool] || (tools.isLoadingTools && !info);
        const outdated = isUpdateAvailable(info?.version, info?.latest_version);
        const report = tools.installReports?.[tool];
        const needsRepair = !!info?.installed_but_broken;
        const blocked = !!info?.update_blocked_by_node;
        // This is CLI version-probe evidence, independent of Desktop inventory.
        const cliInstallation: LocalToolInstallation | undefined =
          tool === "codex" &&
          info?.name === "codex" &&
          typeof info.version === "string" &&
          !!info.version.trim() &&
          info.installed_but_broken === false &&
          info.error === null
            ? {
                id: "codex",
                kind: "cli",
                installed: true,
                version: info.version,
                path: null,
                status: "installed",
                error: null,
                runtimeStatus: "unavailable",
                runtimeError: null,
                launchSupported: true,
              }
            : undefined;
        const action = needsRepair || info?.version ? "update" : "install";
        const state = busy
          ? "busy"
          : loading
            ? "loading"
            : needsRepair || blocked || info?.error
              ? "warning"
              : info?.version
                ? outdated
                  ? "update"
                  : "installed"
                : "idle";
        const status = busy
          ? "处理中"
          : loading
            ? "检测中"
            : info?.update_blocked_by_node
              ? "需要更新环境"
              : needsRepair
                ? "需要修复"
                : info?.error
                  ? "检测异常"
                  : info?.version
                    ? outdated
                      ? "可更新"
                      : "已安装"
                    : info
                      ? "未安装"
                      : "未检测";
        const StatusIcon =
          busy || loading
            ? Loader2
            : state === "warning"
              ? TriangleAlert
              : outdated
                ? ArrowUpCircle
                : info?.version
                  ? CheckCircle2
                  : Download;
        const environment =
          info?.env_type === "wsl"
            ? ["WSL", info.wsl_distro].filter(Boolean).join(" · ")
            : (
                {
                  macos: "macOS",
                  windows: "Windows",
                  linux: "Linux",
                } as Record<string, string>
              )[info?.env_type ?? ""];
        return (
          <section
            className="osir-management-card osir-cli-card"
            key={tool}
            aria-label={toolDisplayName(tool)}
          >
            <div className="osir-management-tool-heading">
              <div className="osir-cli-identity">
                <h2>
                  <SupportedToolIcon id={tool} size={22} decorative />
                  {toolDisplayName(tool)}
                </h2>
                <span
                  className="osir-cli-status"
                  data-state={state}
                  role="status"
                >
                  <StatusIcon
                    size={13}
                    aria-hidden="true"
                    className={busy || loading ? "animate-spin" : undefined}
                  />
                  {status}
                </span>
              </div>
              <div className="flex flex-wrap items-center gap-2">
                {tool === "codex" && (
                  <ToolLaunchButton
                    tool="codex"
                    target="cli"
                    installation={cliInstallation}
                    desktop
                    disabled={
                      loading ||
                      tools.isLoadingTools ||
                      tools.isAnyBusy ||
                      !!tools.pendingUpgrade
                    }
                    onBusyChange={setLaunchBusy}
                  />
                )}
                <Button
                  variant="neutral"
                  disabled={
                    loading ||
                    busy ||
                    launchBusy ||
                    !info ||
                    (!!info.error && !info.version && !needsRepair) ||
                    blocked ||
                    (!!info.version && !outdated && !needsRepair)
                  }
                  aria-busy={busy}
                  onClick={() => void tools.handleRunToolAction([tool], action)}
                >
                  {busy ? (
                    <Loader2
                      size={15}
                      className="animate-spin"
                      aria-hidden="true"
                    />
                  ) : info?.version || needsRepair ? (
                    <ArrowUpCircle size={15} aria-hidden="true" />
                  ) : (
                    <Download size={15} aria-hidden="true" />
                  )}
                  {busy
                    ? "处理中…"
                    : needsRepair
                      ? "修复安装"
                      : info?.version
                        ? outdated
                          ? "更新版本"
                          : info.latest_version
                            ? "已是最新"
                            : "已安装"
                        : "安装"}
                </Button>
              </div>
            </div>
            <dl className="osir-cli-versions">
              <div>
                <dt>当前版本</dt>
                <dd>
                  {loading
                    ? "检测中…"
                    : info?.version ||
                      (needsRepair || info?.error ? "无法读取" : "—")}
                </dd>
              </div>
              <div>
                <dt>可用版本</dt>
                <dd>
                  {loading ? "检测中…" : info?.latest_version || "暂不可用"}
                </dd>
              </div>
              {environment && (
                <div>
                  <dt>安装环境</dt>
                  <dd>{environment}</dd>
                </div>
              )}
            </dl>
            {(info?.error || needsRepair) && !info?.update_blocked_by_node && (
              <p role="alert" className="osir-management-feedback">
                {cliDetectionMessage(info?.error, needsRepair)}
              </p>
            )}
            {info?.update_blocked_by_node && (
              <p className="osir-management-notice">
                当前 Node 环境不满足新版本要求，请更新 Node 后重试。
              </p>
            )}
            {!!report?.installs.length && (
              <details className="osir-cli-installations">
                <summary>
                  安装位置与详情 <span>{report.installs.length}</span>
                </summary>
                {report.installs.map((inst) => (
                  <ToolInstallRow key={inst.path} inst={inst} />
                ))}
              </details>
            )}
          </section>
        );
      })}
      {showUpgradeConfirmation && (
        <ToolUpgradeConfirmDialog
          className="osir-tool-update-dialog"
          isOpen={tools.pendingUpgrade !== null}
          plans={tools.pendingUpgrade?.plans ?? []}
          displayName={toolDisplayName}
          onConfirm={tools.handleConfirmUpgrade}
          onCancel={tools.handleCancelUpgrade}
        />
      )}
    </div>
  );
}

function BackupManagement({
  onBlockedChange,
}: {
  onBlockedChange: (value: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const { settings, isLoading, isSaving, autoSaveSettings, updateSettings } =
    useSettings();
  const [error, setError] = useState("");
  const saveLock = useRef(false);
  const mutations = useIsMutating();
  const [selectionBlocked, setSelectionBlocked] = useState(false);
  const [transferBlocked, setTransferBlocked] = useState(false);
  const [toolBackupBlocked, setToolBackupBlocked] = useState(false);
  // Share the backup manager query; load failures must not look empty.
  const backups = useQuery({
    queryKey: ["db-backups"],
    queryFn: () => backupsApi.listDbBackups(),
    retry: false,
  });
  useEffect(() => {
    onBlockedChange(
      selectionBlocked ||
        transferBlocked ||
        toolBackupBlocked ||
        isSaving ||
        mutations > 0,
    );
    return () => onBlockedChange(false);
  }, [
    selectionBlocked,
    transferBlocked,
    toolBackupBlocked,
    isSaving,
    mutations,
    onBlockedChange,
  ]);
  const save = useCallback(
    async (updates: {
      backupIntervalHours?: number;
      backupRetainCount?: number;
    }) => {
      if (!settings || isSaving || saveLock.current) return;
      saveLock.current = true;
      const previous = {
        backupIntervalHours: settings.backupIntervalHours,
        backupRetainCount: settings.backupRetainCount,
      };
      setError("");
      updateSettings(updates);
      try {
        if (!(await autoSaveSettings(updates)))
          throw new Error("备份设置未保存，请重试。");
      } catch (e) {
        updateSettings(previous);
        setError(connectionError(e));
      } finally {
        saveLock.current = false;
      }
    },
    [settings, isSaving, autoSaveSettings, updateSettings],
  );
  return (
    <div className="osir-management-backups">
      <ToolConfigBackups
        disabled={
          selectionBlocked || transferBlocked || isSaving || mutations > 0
        }
        onBlockedChange={setToolBackupBlocked}
        onRestored={() => {
          void queryClient.invalidateQueries().catch(() => {
            setError(
              "工具配置已处理，但页面状态刷新失败，请重新打开相关工具页核对。",
            );
          });
        }}
      />
      <section
        className="osir-management-card osir-backup-range"
        aria-labelledby="osir-backup-range-title"
      >
        <header className="osir-backup-card-heading">
          <div>
            <h2 id="osir-backup-range-title">客户端数据备份范围</h2>
            <p>保存客户端本地数据库，不是完整工具配置或设备备份。</p>
          </div>
          <span className="osir-backup-badge">仅本机数据</span>
        </header>
        <BackupScope compact />
        <p className="osir-backup-note">
          云端账户、余额、订阅与密钥不会被恢复操作修改。数据库恢复可能重新同步工具配置；仅恢复指定配置文件请使用上方的工具配置快照。
        </p>
      </section>
      {error && (
        <p role="alert" className="osir-management-feedback">
          {error}
        </p>
      )}
      <div className="osir-management-card osir-backup-policy">
        <header className="osir-backup-card-heading">
          <div>
            <h2>客户端数据自动备份</h2>
            <p>客户端启动时及运行期间每天检查一次，达到所选间隔后自动备份。</p>
          </div>
        </header>
        {isLoading ? (
          <p role="status">正在读取备份设置…</p>
        ) : !settings ? (
          <p role="alert">备份设置暂不可用，请重新打开此页。</p>
        ) : (
          <fieldset
            disabled={
              isSaving ||
              mutations > 0 ||
              selectionBlocked ||
              transferBlocked ||
              toolBackupBlocked
            }
          >
            <legend className="sr-only">自动备份设置</legend>
            <label>
              备份间隔
              <select
                value={settings.backupIntervalHours ?? 24}
                onChange={(event) =>
                  void save({ backupIntervalHours: Number(event.target.value) })
                }
              >
                <option value={0}>关闭自动备份</option>
                <option value={6}>6 小时</option>
                <option value={12}>12 小时</option>
                <option value={24}>24 小时</option>
                <option value={48}>48 小时</option>
                <option value={168}>7 天</option>
              </select>
            </label>
            <label>
              保留数量
              <select
                value={settings.backupRetainCount ?? 10}
                onChange={(event) =>
                  void save({ backupRetainCount: Number(event.target.value) })
                }
              >
                {Array.from(
                  new Set([
                    settings.backupRetainCount ?? 10,
                    3,
                    5,
                    10,
                    15,
                    20,
                    30,
                    50,
                  ]),
                )
                  .sort((a, b) => a - b)
                  .map((count) => (
                    <option value={count} key={count}>
                      {count} 份
                    </option>
                  ))}
              </select>
            </label>
          </fieldset>
        )}
        <p className="osir-backup-note">
          手动备份与恢复前的安全备份也计入此数量；恢复时会保护来源和安全备份，必要时临时超出上限。关闭自动备份不影响手动备份和恢复前保护。
        </p>
        {isSaving && <p role="status">正在保存备份设置…</p>}
      </div>
      <div className="osir-management-card">
        {backups.isError && (
          <div role="alert">
            备份列表读取失败。
            <button
              type="button"
              disabled={backups.isFetching}
              onClick={() => void backups.refetch()}
            >
              重试
            </button>
          </div>
        )}
        {(!backups.isError || backups.data) && (
          <OsirLocalBackupList
            disabled={
              backups.isError ||
              isSaving ||
              transferBlocked ||
              toolBackupBlocked ||
              mutations > 0
            }
            onSelectionChange={setSelectionBlocked}
          />
        )}
      </div>
      <div className="osir-management-card">
        <OsirBackupTransfer
          disabled={
            isSaving || selectionBlocked || toolBackupBlocked || mutations > 0
          }
          onBlockedChange={setTransferBlocked}
        />
      </div>
      <div className="osir-management-card osir-backup-storage">
        <p className="osir-backup-note">
          下列是单独管理的各类备份目录；除数据库备份外，其他目录不会随上方的数据库恢复操作恢复。清理前请核对各项说明。
        </p>
        <fieldset
          disabled={
            isSaving ||
            selectionBlocked ||
            transferBlocked ||
            toolBackupBlocked ||
            mutations > 0
          }
        >
          <legend className="sr-only">其他备份存储管理</legend>
          <BackupStorageSection />
        </fieldset>
      </div>
    </div>
  );
}
