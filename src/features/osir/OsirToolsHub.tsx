import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Download,
  ExternalLink,
  Loader2,
  Monitor,
  RefreshCw,
  Terminal,
  TriangleAlert,
} from "lucide-react";
import { toast } from "@/lib/toast";
import { SupportedToolIcon } from "./SupportedToolIcon";
import { CodexDesktopPanel } from "./CodexDesktopPanel";
import { WorkBuddyModelSetup } from "./WorkBuddyModelSetup";
import { type ToolId } from "./providerConfig";
import { connectionError } from "./errors";
import {
  LOCAL_TOOLS_QUERY,
  LOCAL_TOOL_CATALOG,
  LOCAL_TOOL_IDS,
  isProviderTool,
  installedToolIds,
  useLocalTools,
  toolRuntimeLabel,
  type SupportedToolId,
} from "./toolsApi";
import { ToolLaunchButton } from "./ToolLaunchButton";
import { ToolCloseButton } from "./ToolCloseButton";
import {
  ToolUpdatesProvider,
  ToolUpdatesCheckButton,
  ToolUpdateResults,
  ToolUpdateButton,
} from "./ToolUpdateButton";
import "./tools-hub.css";

const CliManagement = lazy(() =>
  import("./OsirToolManagement").then((module) => ({
    default: module.CliManagement,
  })),
);
const CATALOG = LOCAL_TOOL_IDS.map((id) => ({ id, ...LOCAL_TOOL_CATALOG[id] }));
export { SupportedToolIcon } from "./SupportedToolIcon";
export const supportedToolName = (id: SupportedToolId) =>
  LOCAL_TOOL_CATALOG[id].name;

export function OsirToolsHub({
  desktop,
  selectedTool,
  onOpenTool,
  onNavigationBlockedChange,
}: {
  desktop: boolean;
  selectedTool?: SupportedToolId;
  onOpenTool: (tool: ToolId) => void;
  onNavigationBlockedChange: (blocked: boolean) => void;
}) {
  const tools = useLocalTools(desktop);
  const query = useQueryClient();
  const [expanded, setExpanded] = useState<SupportedToolId | null>(
    selectedTool ?? null,
  );
  const [filter, setFilter] = useState("all");
  const [working, setWorking] = useState(false);
  const [draft, setDraft] = useState(false);
  const draftRef = useRef(false);
  const [error, setError] = useState("");
  const wasBusy = useRef(false);
  const lock = useRef(false);
  const onBusyChange = useCallback(
    (busy: boolean) => {
      setWorking(busy);
      if (wasBusy.current && !busy)
        void query.invalidateQueries({ queryKey: LOCAL_TOOLS_QUERY });
      wasBusy.current = busy;
    },
    [query],
  );
  const onInteractionBlockedChange = useCallback(
    (blocked: boolean) =>
      onNavigationBlockedChange(blocked || draftRef.current),
    [onNavigationBlockedChange],
  );
  const onConfigurationBusyChange = useCallback(
    (busy: boolean) => {
      onBusyChange(busy);
      onNavigationBlockedChange(busy || draftRef.current);
    },
    [onBusyChange, onNavigationBlockedChange],
  );
  const onLaunchBusyChange = useCallback(
    (busy: boolean) => {
      setWorking(busy);
      onNavigationBlockedChange(busy || draftRef.current);
    },
    [onNavigationBlockedChange],
  );
  const onDraftChange = useCallback(
    (dirty: boolean) => {
      draftRef.current = dirty;
      setDraft(dirty);
      onNavigationBlockedChange(dirty);
    },
    [onNavigationBlockedChange],
  );
  useEffect(
    () => () => onNavigationBlockedChange(false),
    [onNavigationBlockedChange],
  );
  async function downloadWorkBuddy() {
    if (lock.current) return;
    lock.current = true;
    setError("");
    onBusyChange(true);
    try {
      await invoke("osir_open_workbuddy_download");
      toast.success("已打开官方下载页，安装完成后点击重新检测。");
    } catch (e) {
      setError(connectionError(e));
    } finally {
      lock.current = false;
      onBusyChange(false);
    }
  }
  const catalog = selectedTool
    ? CATALOG.filter((item) => item.id === selectedTool)
    : CATALOG;
  const installedCount = installedToolIds(tools.data).length;
  return (
    <ToolUpdatesProvider
      desktop={desktop}
      installations={tools.data ?? []}
      onManageDesktop={() => setExpanded("codex")}
      onBusyChange={onBusyChange}
      onNavigationBlockedChange={onInteractionBlockedChange}
    >
      <section
        className="osir-tools-hub"
        aria-label={selectedTool ? supportedToolName(selectedTool) : "工具管理"}
      >
        <header className="osir-header">
          <div>
            <h1>
              {selectedTool ? supportedToolName(selectedTool) : "工具管理"}
            </h1>
            <p>
              {selectedTool === "workbuddy"
                ? "连接 OSIRAPI 模型。"
                : selectedTool
                  ? "管理本机安装与更新，供应商连接请在工具中配置。"
                  : "检测、安装与更新支持的工具，已安装工具显示在左侧。"}
            </p>
          </div>
          <div className="osir-tools-header-actions">
            <ToolUpdatesCheckButton disabled={working || draft} />
            <button
              disabled={!desktop || tools.isFetching || working}
              onClick={() => void tools.refetch()}
            >
              <RefreshCw
                size={15}
                className={tools.isFetching ? "animate-spin" : undefined}
              />
              {tools.isFetching ? "检测中…" : "重新检测"}
            </button>
          </div>
        </header>
        <ToolUpdateResults />
        {!selectedTool && (
          <div
            className="osir-tools-filter"
            role="group"
            aria-label="工具安装状态"
          >
            {[
              { id: "all", label: "全部工具" },
              {
                id: "installed",
                label: `已安装${tools.data ? ` ${installedCount}` : ""}`,
              },
              { id: "missing", label: "待安装" },
            ].map((item) => (
              <button
                key={item.id}
                aria-pressed={filter === item.id}
                disabled={working || draft}
                onClick={() => setFilter(item.id)}
              >
                {item.label}
              </button>
            ))}
          </div>
        )}
        {!desktop && (
          <p className="osir-tools-notice" role="status">
            请在桌面客户端检测与安装工具。
          </p>
        )}
        {tools.isError && (
          <div className="osir-tools-notice" role="alert">
            <TriangleAlert size={16} />
            <span>本机工具检测失败，请重新检测。已有配置不会受影响。</span>
            <button
              disabled={tools.isFetching}
              onClick={() => void tools.refetch()}
            >
              重试
            </button>
          </div>
        )}
        {error && (
          <div className="osir-tools-notice" role="alert">
            <TriangleAlert size={16} />
            <span>{error}</span>
          </div>
        )}
        <div className="osir-tools-list">
          {catalog
            .filter((item) => {
              const info = tools.data?.find((tool) => tool.id === item.id);
              return (
                filter === "all" ||
                (!!info &&
                  (filter === "installed"
                    ? info.installed
                    : info.status === "not_installed"))
              );
            })
            .map((item) => {
              const info = tools.data?.find((tool) => tool.id === item.id);
              const open = expanded === item.id;
              const known = desktop && !!info && !tools.isError;
              return (
                <article className="osir-supported-tool" key={item.id}>
                  <div className="osir-supported-tool-row">
                    <span className="osir-supported-tool-icon">
                      <SupportedToolIcon id={item.id} size={24} decorative />
                    </span>
                    <div className="osir-supported-tool-name">
                      <div className="osir-supported-tool-title">
                        <h2>{supportedToolName(item.id)}</h2>
                        {known && info.installed && (
                          <ToolUpdateButton
                            tool={item.id}
                            installation={info}
                            desktop={desktop}
                            disabled={working || draft}
                            onBusyChange={onBusyChange}
                          />
                        )}
                        <span className="osir-tools-kind">
                          {item.desktop ? (
                            <Monitor size={11} />
                          ) : (
                            <Terminal size={11} />
                          )}
                          {item.desktop ? "桌面应用" : "CLI"}
                        </span>
                      </div>
                      <p>{item.description}</p>
                      {known &&
                        info.installed &&
                        item.desktop &&
                        info.runtimeStatus && (
                          <p className="osir-tools-runtime">
                            {toolRuntimeLabel(info)}
                            {info.launchSupported === false &&
                            info.runtimeStatus !== "unavailable"
                              ? " · 暂不支持从客户端启动"
                              : ""}
                          </p>
                        )}
                    </div>
                    <span
                      className="osir-tools-state"
                      data-state={
                        known
                          ? info.error
                            ? "warning"
                            : info.installed
                              ? "installed"
                              : "missing"
                          : "unknown"
                      }
                    >
                      {tools.isPending && desktop ? (
                        <Loader2 size={13} className="animate-spin" />
                      ) : known && info.installed ? (
                        <CheckCircle2 size={13} />
                      ) : null}
                      {!known
                        ? desktop && tools.isPending
                          ? "检测中"
                          : "未检测"
                        : info.error
                          ? info.installed
                            ? "需要检查"
                            : "检测异常"
                          : info.installed
                            ? info.version
                              ? `v${info.version.replace(/^v/, "")}`
                              : "已安装"
                            : "未安装"}
                    </span>
                    <div className="osir-supported-tool-actions">
                      {known && info.installed && isProviderTool(item.id) && (
                        <button
                          disabled={working}
                          onClick={() => {
                            if (isProviderTool(item.id)) onOpenTool(item.id);
                          }}
                        >
                          配置
                        </button>
                      )}
                      {known &&
                        info.installed &&
                        !(item.id === "workbuddy" && open) && (
                          <ToolLaunchButton
                            tool={item.id}
                            installation={info}
                            desktop={desktop}
                            disabled={working || draft}
                            onBusyChange={onLaunchBusyChange}
                          />
                        )}
                      {!(
                        selectedTool === "workbuddy" &&
                        known &&
                        info.installed
                      ) && (
                        <button
                          disabled={
                            !desktop || working || draft || tools.isPending
                          }
                          aria-label={`${open ? "收起" : "管理"}${supportedToolName(item.id)}`}
                          aria-expanded={open}
                          aria-controls={`tool-management-${item.id}`}
                          onClick={() => {
                            setError("");
                            setExpanded(open ? null : item.id);
                          }}
                        >
                          {known && info.status === "not_installed" ? (
                            <>
                              <Download size={14} />
                              安装
                            </>
                          ) : info?.error ? (
                            "检查"
                          ) : (
                            "管理"
                          )}
                          {open ? (
                            <ChevronDown size={14} />
                          ) : (
                            <ChevronRight size={14} />
                          )}
                        </button>
                      )}
                      {known && info.installed && item.desktop && (
                        <ToolCloseButton
                          tool={item.id}
                          installation={info}
                          desktop={desktop}
                          disabled={working || draft}
                        />
                      )}
                    </div>
                  </div>
                  {open && (
                    <div
                      className="osir-supported-tool-detail"
                      id={`tool-management-${item.id}`}
                    >
                      {info?.error && (
                        <p role="alert" className="osir-tools-notice">
                          {connectionError(info.error)}
                        </p>
                      )}
                      {item.id === "workbuddy" ? (
                        <div className="osir-workbuddy-details">
                          {known && info.installed ? (
                            <WorkBuddyModelSetup
                              desktop={desktop}
                              onBusyChange={onConfigurationBusyChange}
                              onDirtyChange={onDraftChange}
                            />
                          ) : (
                            <>
                              <p>
                                下载安装 WorkBuddy 后，重新检测即可配置模型。
                              </p>
                              <button
                                disabled={working}
                                onClick={() => void downloadWorkBuddy()}
                              >
                                <ExternalLink size={14} />
                                官方下载
                              </button>
                            </>
                          )}
                          {known && info.installed && (
                            <details className="osir-workbuddy-install-details">
                              <summary>安装信息</summary>
                              {info.path && (
                                <p
                                  className="osir-tools-path"
                                  title={info.path}
                                >
                                  {info.path}
                                </p>
                              )}
                              <p>版本更新请在 WorkBuddy 中检查。</p>
                              <button
                                disabled={working}
                                onClick={() => void downloadWorkBuddy()}
                              >
                                <ExternalLink size={14} />
                                官方下载
                              </button>
                            </details>
                          )}
                        </div>
                      ) : item.id === "claude-desktop" ? (
                        <div className="osir-workbuddy-details">
                          <p>
                            Claude 桌面客户端与 Claude Code CLI
                            分别启动；桌面登录和模型选择在 Claude 应用中完成。
                          </p>
                          {!info?.installed && (
                            <p>
                              请通过 Claude
                              官方网站安装桌面客户端，完成后点击重新检测。
                            </p>
                          )}
                          {info?.path && (
                            <p className="osir-tools-path">{info.path}</p>
                          )}
                        </div>
                      ) : item.id === "codex" ? (
                        <CodexDesktopPanel
                          desktop={desktop}
                          onBusyChange={onBusyChange}
                          onNavigationBlockedChange={onInteractionBlockedChange}
                        />
                      ) : (
                        <Suspense
                          fallback={<p role="status">正在加载安装管理…</p>}
                        >
                          <CliManagement
                            toolIds={[item.id]}
                            showLifecycleResults={false}
                            showUpgradeConfirmation={false}
                            onBlockedChange={onInteractionBlockedChange}
                          />
                        </Suspense>
                      )}
                    </div>
                  )}
                </article>
              );
            })}
        </div>
        {filter !== "all" &&
          tools.data &&
          !catalog.some((item) => {
            const info = tools.data?.find((tool) => tool.id === item.id);
            return (
              info &&
              (filter === "installed"
                ? info.installed
                : info.status === "not_installed")
            );
          }) && (
            <p className="osir-tools-empty">
              {filter === "installed"
                ? "尚未检测到已安装工具，切换全部工具开始安装。"
                : catalog.every(
                      (item) =>
                        tools.data?.find((tool) => tool.id === item.id)
                          ?.status === "installed",
                    )
                  ? "支持的工具均已安装。"
                  : "暂无可确认待安装的工具，请重新检测异常项目。"}
            </p>
          )}
      </section>
    </ToolUpdatesProvider>
  );
}
