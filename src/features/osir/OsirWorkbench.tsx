import {
  lazy,
  Suspense,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { getName, getVersion } from "@tauri-apps/api/app";
import {
  Terminal,
  LayoutDashboard,
  KeyRound,
  CreditCard,
  History,
  Settings,
  Puzzle,
  Plus,
  RefreshCw,
  Cloud,
  ChevronRight,
  Download,
  UserRound,
  Layers,
  ChevronDown,
  Check,
  ReceiptText,
} from "lucide-react";
import { providersApi, proxyApi } from "@/lib/api";
import { toast } from "@/lib/toast";
import osirClientIcon from "@/assets/brand/osir-client.png";
import type { Provider } from "@/types";
const SettingsPage = lazy(() =>
  import("@/components/settings/SettingsPage").then((module) => ({
    default: module.SettingsPage,
  })),
);
import { Switch } from "@/components/ui/switch";
import { SupportedToolIcon } from "./SupportedToolIcon";
import {
  aggregationIntent,
  saveAggregationIntent,
  applyAggregation,
  syncSavedAggregation,
  disableAggregation,
} from "./aggregation";
import { ConnectionDrawer } from "./ConnectionDrawer";
import { ModelSelection } from "./ModelSelection";
import { ClaudeModelRoles } from "./ClaudeModelRoles";
import { CodexCatalogStatus } from "./CodexCatalogStatus";
import {
  ToolBrandIcon,
  ConnectionBrandIcon,
  connectionBrandName,
} from "./BrandMark";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ResourcePage } from "./ResourcePage";
import { legacyIdentity, useLegacySnapshot } from "./legacySnapshot";
import { OsirAccountPanel } from "./OsirAccountPanel";
import { AccountAvatar } from "./AccountAvatar";
import { OsirAnnouncements } from "./OsirAnnouncements";
import {
  useLocalTools,
  useDesktopRuntimeHeartbeat,
  LOCAL_TOOLS_QUERY,
  LOCAL_TOOL_CATALOG,
  installedToolIds,
  isManagedTool,
  toolRuntimeLabel,
  type SupportedToolId,
} from "./toolsApi";
import {
  useAccountSession,
  useAccountResource,
  type AccountProfile,
} from "./accountApi";
import { AccountOverview, AccountOverviewTools } from "./AccountOverview";
import { ToolLaunchButton } from "./ToolLaunchButton";
import { ToolCloseButton } from "./ToolCloseButton";
import {
  ToolUpdateButton,
  ToolUpdatesProvider,
  ToolUpdateResults,
} from "./ToolUpdateButton";
const OsirToolManagement = lazy(() =>
  import("./OsirToolManagement").then((module) => ({
    default: module.OsirToolManagement,
  })),
);
const OsirToolsHub = lazy(() =>
  import("./OsirToolsHub").then((module) => ({ default: module.OsirToolsHub })),
);
const OsirClaudeDesktop = lazy(() =>
  import("./OsirClaudeDesktop").then((module) => ({
    default: module.OsirClaudeDesktop,
  })),
);
const OsirInvoices = lazy(() =>
  import("./OsirInvoices").then((module) => ({ default: module.OsirInvoices })),
);
import { OsirImageBindingPanel } from "./OsirImageBindingPanel";
import { connectionError } from "./errors";
import {
  TOOL_IDS,
  TOOL_NAMES,
  modelNames,
  simpleEditable,
  type ToolId,
} from "./providerConfig";
import "./workbench.css";
import { OsirTabs } from "./OsirTabs";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";

type Page =
  | SupportedToolId
  | "overview"
  | "keys"
  | "subscriptions"
  | "usage"
  | "account"
  | "invoices"
  | "apps"
  | "extensions"
  | "settings";
type AccountAuthCapability = {
  status: "available" | "unavailable";
  reason: string;
};
type ConnectionModeView = {
  mode: "native" | "osir_api" | "unavailable";
  canRestoreNative: boolean;
  reason?: string;
};
const isTool = (s: Page): s is ToolId => TOOL_IDS.includes(s as ToolId);
export const isDesktop = () => "__TAURI_INTERNALS__" in window;
export default function OsirWorkbench() {
  const [page, setPage] = useState<Page>("overview");
  const [usageGroup, setUsageGroup] = useState<number>();
  const [tab, setTab] = useState("models");
  const contentRef = useRef<HTMLElement>(null);
  const [managementBlocked, setManagementBlocked] = useState(false);
  const [invoiceAccountRequested, setInvoiceAccountRequested] = useState(false);
  const dirtyModels = useRef(new Set<string>());
  const [leaveAction, setLeaveAction] = useState<(() => void) | null>(null);
  const requestLeave = (next: () => void) => {
    if (dirtyModels.current.size) setLeaveAction(() => next);
    else next();
  };
  const changeTab = (next: string) => {
    if (next !== tab && !busy && !managementBlocked)
      requestLeave(() => setTab(next));
  };
  const [drawer, setDrawer] = useState<{
    tool: ToolId;
    provider?: Provider;
    source?: "manual" | "account";
    reconnectName?: string;
  } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<{
    provider: Provider;
    tool: ToolId;
  } | null>(null);
  const [deleteError, setDeleteError] = useState("");
  const [actionBusy, setBusy] = useState(false);
  const modelOperations = useRef(new Set<string>());
  const [modelOperationCount, setModelOperationCount] = useState(0);
  const busy = actionBusy || modelOperationCount > 0;
  // Invoice drawers release their navigation lock only after draft confirmation.
  useEffect(() => {
    if (!invoiceAccountRequested) return;
    if (page !== "invoices") {
      setInvoiceAccountRequested(false);
    } else if (!managementBlocked && !busy && !drawer) {
      setInvoiceAccountRequested(false);
      requestLeave(() => {
        setPage("account");
        setUsageGroup(undefined);
        setTab("models");
      });
    }
  }, [invoiceAccountRequested, page, managementBlocked, busy, drawer]);
  const [previewProviders, setPreviewProviders] = useState<
    Record<string, Record<string, Provider>>
  >({});
  const [previewCurrent, setPreviewCurrent] = useState<Record<string, string>>(
    {},
  );
  const [imageOpenRequest, setImageOpenRequest] = useState(0);
  const [imagePanelVisible, setImagePanelVisible] = useState(false);
  const desktop = isDesktop();
  const clientBuild = useQuery({
    queryKey: ["osir-client-build"],
    queryFn: async () => {
      const [name, version] = await Promise.all([getName(), getVersion()]);
      return `${name} · ${version}`;
    },
    enabled: desktop,
    staleTime: Infinity,
    retry: false,
  });
  useDesktopRuntimeHeartbeat(desktop);
  const installedTools = useLocalTools(desktop);
  const accountSession = useAccountSession();
  // Observe shared profile cache; reads occur only on account/overview pages.
  const sidebarProfile = useAccountResource<AccountProfile>(
    "profile",
    {},
    false,
  );
  const connected = !!accountSession.data?.connected && !accountSession.isError;
  const providerGroups = useAccountResource<
    { id: number; name?: string; subscription_type?: string }[]
  >("groups", {}, connected && isTool(page));
  const sidebarAccount = connected
    ? sidebarProfile.data &&
      sidebarProfile.data.generation === accountSession.data?.generation
      ? sidebarProfile.data.data
      : accountSession.data?.profile
    : undefined;
  const legacySnapshot = useLegacySnapshot(desktop);
  const authCapability = useQuery({
    queryKey: ["osir-auth-capability"],
    queryFn: () =>
      invoke<AccountAuthCapability>("osir_desktop_auth_capability"),
    enabled: desktop,
    retry: false,
    staleTime: 60_000,
    refetchOnWindowFocus: false,
  });
  const [aggregationEnabled, setAggregationEnabled] =
    useState(aggregationIntent);
  const mutationLock = useRef(false);
  const query = useQueryClient();
  useLayoutEffect(() => {
    contentRef.current?.scrollTo({ top: 0, left: 0 });
  }, [page, tab]);
  const tool: ToolId = isTool(page) ? page : "codex";
  const [previewSource, setPreviewSource] = useState<
    Record<string, "native" | "osir_api">
  >({});
  const connectionMode = useQuery({
    queryKey: ["osir-connection-mode", tool],
    queryFn: () =>
      invoke<ConnectionModeView>("osir_get_connection_mode", { tool }),
    enabled: desktop && isTool(page),
    retry: false,
    refetchOnWindowFocus: false,
  });
  const connectionSource = desktop
    ? !connectionMode.isPending &&
      !connectionMode.isError &&
      (connectionMode.data?.mode === "native" ||
        connectionMode.data?.mode === "osir_api")
      ? connectionMode.data.mode
      : undefined
    : previewSource[tool] || "osir_api";
  const [expandedProviders, setExpandedProviders] = useState<
    Record<string, boolean>
  >({});
  const providers = useQuery({
    queryKey: ["osir-providers", tool],
    queryFn: () => providersApi.getAll(tool),
    enabled: desktop && isTool(page),
    retry: false,
  });
  const current = useQuery({
    queryKey: ["osir-current", tool],
    queryFn: () => providersApi.getCurrent(tool),
    enabled: desktop && isTool(page),
    retry: false,
  });
  const mode = useQuery({
    queryKey: ["osir-mode", "codex"],
    queryFn: () => proxyApi.getAppMode("codex"),
    enabled: desktop && page === "codex",
    retry: false,
  });
  const stack = useQuery({
    queryKey: ["osir-stack", "codex"],
    queryFn: () => proxyApi.getProxyStack("codex"),
    enabled: desktop && page === "codex",
    retry: false,
  });
  const overviewConnectionQueries = useQueries({
    queries: TOOL_IDS.map((target) => ({
      queryKey: ["osir-connection-mode", target],
      queryFn: () =>
        invoke<ConnectionModeView>("osir_get_connection_mode", {
          tool: target,
        }),
      // Observe only modes that a tool page has already read. The command
      // settles pending writes, so the overview must never invoke it itself.
      enabled: false,
      retry: false,
      staleTime: 60_000,
      refetchOnWindowFocus: false,
    })),
  });
  const overviewConnections = Object.fromEntries(
    TOOL_IDS.map((target, index) => {
      const status = overviewConnectionQueries[index];
      return [
        target,
        {
          status: !desktop
            ? "unavailable"
            : status.isLoading
              ? "loading"
              : status.isError
                ? "error"
                : status.data?.mode === "native" ||
                    status.data?.mode === "osir_api"
                  ? status.data.mode
                  : "unavailable",
          observedAt: status.dataUpdatedAt || undefined,
        },
      ];
    }),
  );
  const allRows = Object.values(
    desktop ? providers.data || {} : previewProviders[tool] || {},
  );
  const rows = allRows.filter((provider) =>
    connectionSource === "native"
      ? !provider.meta?.osirBinding
      : connectionSource === "osir_api"
        ? provider.category !== "official"
        : true,
  );
  const openCodeDefault = useQuery({
    queryKey: ["osir-opencode-default"],
    queryFn: () => providersApi.getOpenCodeDefaultModel(),
    enabled: desktop && tool === "opencode",
    retry: false,
  });
  const currentId = desktop
    ? tool === "opencode"
      ? openCodeDefault.data?.split("/")[0]
      : current.data
    : previewCurrent[tool];
  const version = installedTools.data?.find((x) => x.id === tool);
  const activeId =
    page === "codex" && mode.data?.mode !== "direct"
      ? (mode.data?.mode === "stack"
          ? stack.data?.members.find((member) => member.route)?.providerId
          : mode.data?.routeProviderId) || currentId
      : currentId;
  const aggregateActive =
    tool === "codex" && mode.data?.mode === "stack" && !!mode.data.attached;
  const providerInUse = (provider: Provider) =>
    !!connectionSource &&
    !current.isError &&
    !(tool === "codex" && (mode.isError || stack.isError)) &&
    !(tool === "opencode" && openCodeDefault.isError) &&
    (aggregateActive
      ? !!stack.data?.members.some(
          (member) => member.providerId === provider.id,
        )
      : provider.id === activeId);
  const go = (p: Page, groupId?: number) => {
    if (busy || drawer || managementBlocked || p === page) return;
    requestLeave(() => {
      setPage(p);
      setUsageGroup(p === "usage" ? groupId : undefined);
      setTab("models");
    });
  };
  const openToolPage = (target: ToolId, nextTab: string = "models") => {
    if (busy || drawer || managementBlocked) return;
    requestLeave(() => {
      setPage(target);
      setUsageGroup(undefined);
      setTab(nextTab);
    });
  };
  const trayNavigate = useRef<() => Promise<void>>(async () => {});
  const trayReady = useRef(false);
  const trayDraining = useRef(false);
  const trayBlocked = useRef(false);
  trayBlocked.current = busy || !!drawer || managementBlocked;
  const queuedTray = useRef<{
    app?: string;
    section?: string;
    intent?: string;
  } | null>(null);
  trayNavigate.current = async () => {
    if (trayBlocked.current || trayDraining.current) return;
    trayDraining.current = true;
    try {
      const navigation =
        queuedTray.current ??
        (await invoke<{
          app?: string;
          section?: string;
          intent?: string;
        } | null>("take_tray_navigation"));
      if (!navigation) return;
      if (trayBlocked.current) {
        queuedTray.current = navigation;
        return;
      }
      queuedTray.current = null;
      requestLeave(() => {
        const section = navigation.section?.replace(/^osir:/, "");
        if (navigation.app && TOOL_IDS.includes(navigation.app as ToolId)) {
          setPage(navigation.app as ToolId);
          setTab("models");
          if (navigation.intent === "add")
            setDrawer({ tool: navigation.app as ToolId });
          else if (navigation.intent === "needsRoute")
            toast.info("请在工具页面完成连接设置后切换。");
        } else if (section === "history") {
          setPage("codex");
          setTab("sessions");
        } else if (section === "updates") {
          setPage("settings");
          setTab("about");
        } else if (section === "account" || section === "extensions") {
          setPage(section);
          setTab("models");
        } else {
          setPage("settings");
          setTab("general");
        }
        setUsageGroup(undefined);
      });
    } finally {
      trayDraining.current = false;
    }
  };
  useEffect(() => {
    if (!desktop) return;
    let disposed = false;
    const subscription = listen("tray-navigate", () => {
      if (!disposed) void trayNavigate.current().catch(() => {});
    });
    void subscription
      .then((unlisten) => {
        if (disposed) {
          unlisten();
          return;
        }
        trayReady.current = true;
        void trayNavigate.current().catch(() => {});
      })
      .catch(() => {});
    return () => {
      disposed = true;
      trayReady.current = false;
      void subscription.then((unlisten) => unlisten());
    };
  }, [desktop]);
  useEffect(() => {
    if (desktop && trayReady.current && !busy && !drawer && !managementBlocked)
      void trayNavigate.current().catch(() => {});
  }, [desktop, busy, drawer, managementBlocked]);
  async function refresh() {
    await Promise.all([
      query.invalidateQueries({ queryKey: ["osir-versions"] }),
      query.invalidateQueries({ queryKey: ["osir-local-version"] }),
      query.invalidateQueries({ queryKey: LOCAL_TOOLS_QUERY }),
      query.invalidateQueries({ queryKey: ["osir-opencode-default"] }),
      query.invalidateQueries({ queryKey: ["osir-providers"] }),
      query.invalidateQueries({ queryKey: ["osir-current"] }),
      query.invalidateQueries({ queryKey: ["osir-stack"] }),
      query.invalidateQueries({ queryKey: ["osir-mode"] }),
      query.invalidateQueries({ queryKey: ["osir-connection-mode"] }),
      query.invalidateQueries({ queryKey: ["providers"] }),
    ]);
  }
  async function action(fn: () => Promise<unknown>, success: string) {
    if (mutationLock.current || modelOperations.current.size) return;
    mutationLock.current = true;
    setBusy(true);
    try {
      const outcome = await fn();
      void refresh().catch(() => {
        toast.warning(
          "操作已完成，但状态刷新失败，请点击刷新核对，勿重复提交。",
          { duration: 5000 },
        );
      });
      toast.success(
        success +
          (typeof outcome === "string" && outcome
            ? " · " + connectionError(outcome)
            : ""),
      );
      return true;
    } catch (e) {
      void refresh().catch(() => {});
      toast.error(connectionError(e), { duration: 4500 });
    } finally {
      mutationLock.current = false;
      setBusy(false);
    }
  }
  async function deleteConnection() {
    if (!deleteTarget || mutationLock.current) return;
    const target = deleteTarget;
    setDeleteError("");
    const ok = await action(async () => {
      try {
        if (!desktop) throw new Error("浏览器预览不能删除本机供应商。");
        // Native deletion owns reference checks and stack cleanup under its lock.
        // Never detach members or reroute before deletion in the frontend.
        const confirmed = await providersApi.delete(
          target.provider.id,
          target.tool,
        );
        if (!confirmed)
          throw new Error("删除未确认完成，请刷新列表核对后重试。");
      } catch (e) {
        setDeleteError(connectionError(e));
        throw e;
      }
    }, "本地供应商已删除，云端密钥保持不变。");
    if (ok) {
      if (target.tool === "codex") {
        void query.invalidateQueries({ queryKey: ["osir-global-image"] });
      }
      dirtyModels.current.delete(target.provider.id);
      setDeleteTarget(null);
      setDrawer(null);
    }
  }
  async function switchTo(p: Provider) {
    if (!desktop) {
      setPreviewCurrent((s) => ({ ...s, [tool]: p.id }));
      toast.info("仅切换浏览器预览状态，未修改本机配置。");
      return;
    }
    await action(async () => {
      if (p.meta?.osirBinding) {
        await invoke("osir_set_connection_mode", {
          tool,
          mode: "osir_api",
          providerId: p.id,
          aggregation: aggregationIntent(),
        });
        return;
      }
      if (tool === "opencode") {
        const model = modelNames(p, tool)[0];
        if (!model) throw new Error("请先配置模型。");
        await providersApi.addToLiveConfig(p.id, tool);
        await providersApi.setOpenCodeDefaultModel(p.id + "/" + model);
        return;
      }
      const result = await providersApi.switch(p.id, tool);
      return result.warnings.join("；");
    }, "供应商已切换，重新启动工具后使用新连接。");
  }
  const saved = (p: Provider, savedTool: ToolId) => {
    setExpandedProviders((state) => ({ ...state, [savedTool + p.id]: true }));
    if (!desktop) {
      setPreviewProviders((s) => ({
        ...s,
        [savedTool]: { ...s[savedTool], [p.id]: p },
      }));
      toast.info("已保存到本次浏览器预览，刷新页面后清除。");
      return;
    }
    void action(async () => {
      const existingAccountConnection =
        !!p.meta?.osirBinding && allRows.some((row) => row.id === p.id);
      if (
        p.meta?.osirBinding &&
        !existingAccountConnection &&
        (savedTool !== "codex" || !aggregationIntent())
      ) {
        await invoke("osir_set_connection_mode", {
          tool: savedTool,
          mode: "osir_api",
          providerId: p.id,
          aggregation: aggregationIntent(),
        });
      }
      if (savedTool === "codex") {
        void query.invalidateQueries({ queryKey: ["osir-global-image"] });
        try {
          await syncSavedAggregation(p);
        } catch (e) {
          throw new Error(
            "连接已保存，聚合未完成：" +
              connectionError(e) +
              "。请刷新核对后重试。",
          );
        }
      }
    }, "连接已保存。");
  };
  const openImageConfiguration = () => {
    if (busy || drawer || managementBlocked) return;
    requestLeave(() => {
      setPage("codex");
      setUsageGroup(undefined);
      setTab("models");
      setImagePanelVisible(true);
      setImageOpenRequest((request) => request + 1);
    });
  };
  const resourceNames: Record<string, string> = {
    keys: "密钥与分组",
    subscriptions: "我的订阅",
    usage: "使用记录",
    account: "个人中心",
    invoices: "开票管理",
  };
  return (
    <div className="osir-shell">
      <aside className="osir-sidebar">
        <div className="osir-brand" data-tauri-drag-region>
          <span className="osir-logo">
            <img src={osirClientIcon} alt="" width={40} height={40} />
          </span>
          <div>
            <strong>OSIR API</strong>
            <small>AI 工作台</small>
          </div>
        </div>
        <nav aria-label="主导航">
          <button
            aria-current={page === "overview" ? "page" : undefined}
            className={page === "overview" ? "selected" : ""}
            onClick={() => go("overview")}
          >
            <LayoutDashboard size={18} />
            总览
          </button>
          <p id="osir-nav-tools-label">工具</p>
          <div
            className="osir-nav-tools"
            aria-labelledby="osir-nav-tools-label"
          >
            {installedToolIds(installedTools.data).map((id) => {
              return (
                <button
                  key={id}
                  aria-current={page === id ? "page" : undefined}
                  className={page === id ? "selected" : ""}
                  onClick={() => go(id)}
                >
                  <SupportedToolIcon id={id} size={18} decorative />
                  {LOCAL_TOOL_CATALOG[id].name}
                </button>
              );
            })}
            {installedTools.isPending && desktop && (
              <small className="osir-nav-status" role="status">
                正在检测本机工具…
              </small>
            )}
            {installedTools.isError && (
              <button
                className="osir-nav-status"
                onClick={() => void installedTools.refetch()}
              >
                检测失败，重试
              </button>
            )}
            {installedTools.data &&
              !installedToolIds(installedTools.data).length && (
                <small className="osir-nav-status">暂无已安装工具</small>
              )}
          </div>
          <button
            aria-current={page === "apps" ? "page" : undefined}
            className={page === "apps" ? "selected" : ""}
            onClick={() => go("apps")}
          >
            <Download size={18} />
            工具管理
          </button>
          <p id="osir-nav-resources-label">账户与资源</p>
          <div
            className="osir-nav-resources"
            aria-labelledby="osir-nav-resources-label"
          >
            {(["keys", "subscriptions", "usage", "invoices"] as const).map(
              (id, i) => (
                <button
                  key={id}
                  aria-current={page === id ? "page" : undefined}
                  className={page === id ? "selected" : ""}
                  onClick={() => go(id)}
                >
                  {i === 0 ? (
                    <KeyRound size={18} />
                  ) : i === 1 ? (
                    <CreditCard size={18} />
                  ) : i === 2 ? (
                    <History size={18} />
                  ) : (
                    <ReceiptText size={18} />
                  )}{" "}
                  {resourceNames[id]}
                </button>
              ),
            )}
          </div>
        </nav>
        <div className="osir-sidebar-utilities" aria-label="工作台管理">
          <button
            aria-current={page === "extensions" ? "page" : undefined}
            className={page === "extensions" ? "selected" : ""}
            onClick={() => go("extensions")}
          >
            <Puzzle size={18} />
            MCP 与技能
          </button>
          <button
            aria-current={page === "settings" ? "page" : undefined}
            className={page === "settings" ? "selected" : ""}
            onClick={() => go("settings")}
          >
            <Settings size={18} />
            设置
          </button>
        </div>
        <div className="osir-account">
          <div className="osir-sidebar-profile">
            <AccountAvatar
              accountId={sidebarAccount?.id}
              size={36}
              label="更换账户头像"
            />
            <button className="osir-profile" onClick={() => go("account")}>
              <div>
                <strong>{sidebarAccount?.username || "本机工作台"}</strong>
                <small>
                  {accountSession.isError
                    ? "账户状态不可用"
                    : accountSession.isPending
                      ? "正在读取账户状态…"
                      : connected
                        ? "已连接 OSIR 账户"
                        : "尚未连接 OSIR 账户"}
                </small>
              </div>
            </button>
          </div>
          {legacySnapshot.data && !legacySnapshot.isError && (
            <button className="osir-profile" onClick={() => go("account")}>
              <div>
                <strong>{legacyIdentity(legacySnapshot.data)}</strong>
                <small>订阅安装授权 · 历史快照</small>
              </div>
            </button>
          )}
          <div className="osir-balance">
            <span>
              账户余额{" "}
              <b>
                {sidebarAccount?.balance != null &&
                Number.isFinite(Number(sidebarAccount.balance))
                  ? Number(sidebarAccount.balance).toLocaleString("zh-CN", {
                      minimumFractionDigits: 2,
                      maximumFractionDigits: 2,
                    })
                  : "—"}
              </b>
            </span>
            <button
              onClick={() => {
                go("account");
              }}
            >
              {accountSession.data?.connected ? "查看账户" : "连接账户"}{" "}
              <ChevronRight size={13} />
            </button>
          </div>
        </div>
      </aside>
      <div className="osir-workspace">
        <div className="osir-workspace-toolbar" aria-label="工作台通知">
          <OsirAnnouncements variant="bell" />
        </div>
        <main id="content-area" className="osir-main" ref={contentRef}>
          <Suspense fallback={<p role="status">正在加载页面…</p>}>
            {!desktop && (
              <div className="osir-preview">
                交互预览 · 供应商修改只保留在当前页面，不连接本机或云端服务
              </div>
            )}
            {isTool(page) ? (
              <>
                <ToolUpdatesProvider
                  desktop={desktop}
                  installations={version ? [version] : []}
                  onManageDesktop={() => changeTab("install")}
                  onNavigationBlockedChange={setManagementBlocked}
                >
                  <header className="osir-header">
                    <div className="osir-title">
                      <span className="osir-tool-icon">
                        <ToolBrandIcon tool={tool} size={28} />
                      </span>
                      <div>
                        <div className="osir-title-row">
                          <div className="osir-tool-name-line">
                            <h1>{TOOL_NAMES[tool]}</h1>
                            <ToolUpdateButton
                              tool={tool}
                              installation={version}
                              desktop={desktop}
                              disabled={
                                busy ||
                                managementBlocked ||
                                installedTools.isError
                              }
                            />
                          </div>
                          <span className="osir-badge">
                            {!desktop
                              ? "交互预览"
                              : installedTools.isPending
                                ? "检测中"
                                : installedTools.isError
                                  ? "检测失败"
                                  : version?.installed
                                    ? version.version
                                      ? "已安装 " + version.version
                                      : "已安装"
                                    : version?.error
                                      ? "检测失败"
                                      : "未安装"}
                          </span>
                          {desktop &&
                            version?.installed &&
                            LOCAL_TOOL_CATALOG[tool].desktop &&
                            version.runtimeStatus &&
                            !installedTools.isError && (
                              <span className="osir-badge">
                                {toolRuntimeLabel(version)}
                              </span>
                            )}
                        </div>
                        <p>
                          {tool === "codex"
                            ? "统一配置供应商与模型，管理 Codex 的使用环境。"
                            : "添加多个供应商，选择当前使用的连接。"}
                        </p>
                      </div>
                    </div>
                    <div className="osir-actions">
                      <button onClick={() => changeTab("install")}>
                        版本管理
                      </button>
                      <ToolLaunchButton
                        key={tool}
                        tool={tool}
                        installation={version}
                        desktop={desktop}
                        className="primary"
                        disabled={
                          busy || managementBlocked || installedTools.isError
                        }
                      />
                      {tool === "claude" && (
                        <ToolLaunchButton
                          tool="claude-desktop"
                          installation={installedTools.data?.find(
                            (item) => item.id === "claude-desktop",
                          )}
                          desktop={desktop}
                          disabled={
                            busy || managementBlocked || installedTools.isError
                          }
                        />
                      )}
                      {(LOCAL_TOOL_CATALOG[tool].desktop ||
                        tool === "claude") && (
                        <ToolCloseButton
                          tool={tool === "claude" ? "claude-desktop" : tool}
                          installation={
                            tool === "claude"
                              ? installedTools.data?.find(
                                  (item) => item.id === "claude-desktop",
                                )
                              : version
                          }
                          desktop={desktop}
                          disabled={
                            busy || managementBlocked || installedTools.isError
                          }
                        />
                      )}
                    </div>
                  </header>
                  <ToolUpdateResults />
                </ToolUpdatesProvider>
                <OsirTabs
                  label={`${TOOL_NAMES[tool]} 功能`}
                  value={tab}
                  onChange={changeTab}
                  items={[
                    { value: "models", label: "模型接入" },
                    { value: "install", label: "安装与更新" },
                    { value: "sessions", label: "历史会话" },
                  ]}
                />
                {tab === "models" ? (
                  <>
                    <div className="osir-section-title">
                      <h2>选择连接方式</h2>
                    </div>
                    <div className="osir-connection-cards">
                      <button
                        className={
                          connectionSource === "native" ? "highlight" : ""
                        }
                        aria-pressed={connectionSource === "native"}
                        disabled={
                          busy ||
                          (desktop &&
                            (connectionMode.isPending ||
                              connectionMode.isError))
                        }
                        onClick={() =>
                          requestLeave(() => {
                            if (!desktop) {
                              setPreviewSource((state) => ({
                                ...state,
                                [tool]: "native",
                              }));
                              return;
                            }
                            void action(
                              () =>
                                invoke("osir_set_connection_mode", {
                                  tool,
                                  mode: "native",
                                }),
                              "已恢复原生配置。",
                            );
                          })
                        }
                      >
                        <span className="osir-small-icon">
                          <Terminal size={22} />
                        </span>
                        <div>
                          <strong>原生配置</strong>
                          <small>使用本机配置与官方账号登录</small>
                        </div>
                        {connectionSource === "native" && <Check size={17} />}
                      </button>
                      <button
                        className={
                          connectionSource === "osir_api" ? "highlight" : ""
                        }
                        aria-pressed={connectionSource === "osir_api"}
                        disabled={
                          busy ||
                          (desktop &&
                            (connectionMode.isPending ||
                              connectionMode.isError))
                        }
                        onClick={() =>
                          requestLeave(() => {
                            if (!desktop) {
                              setPreviewSource((state) => ({
                                ...state,
                                [tool]: "osir_api",
                              }));
                              return;
                            }
                            if (connectionMode.data?.mode === "osir_api")
                              return;
                            const provider =
                              allRows.find(
                                (row) =>
                                  row.id === activeId && row.meta?.osirBinding,
                              ) || allRows.find((row) => row.meta?.osirBinding);
                            if (!provider) {
                              setDrawer({ tool, source: "account" });
                              return;
                            }
                            void action(async () => {
                              await invoke("osir_set_connection_mode", {
                                tool,
                                mode: "osir_api",
                                providerId: provider.id,
                                aggregation: aggregationIntent(),
                              });
                              if (tool === "codex") {
                                if (aggregationIntent())
                                  await applyAggregation(
                                    allRows.filter(
                                      (row) => !!row.meta?.osirBinding,
                                    ),
                                  );
                              }
                            }, "已切换至 OSIR API。");
                          })
                        }
                      >
                        <span className="osir-small-icon">
                          <Cloud size={22} />
                        </span>
                        <div>
                          <strong>OSIR API</strong>
                          <small>选择账户密钥，接入可用模型</small>
                        </div>
                        {connectionSource === "osir_api" && <Check size={17} />}
                      </button>
                    </div>
                    {desktop && !connectionSource && (
                      <p
                        role={connectionMode.isError ? "alert" : "status"}
                        className={
                          connectionMode.isError ? "osir-error" : "osir-muted"
                        }
                      >
                        {connectionMode.isPending
                          ? "正在读取连接方式…"
                          : connectionMode.isError
                            ? "连接方式读取失败，暂时无法确认当前使用的连接。"
                            : "尚未确认当前连接。使用 OSIR API 前，请在供应商卡片点击“切换使用”；直接打开会沿用本机已有配置。"}
                        {!connectionMode.isPending && (
                          <button
                            disabled={connectionMode.isFetching}
                            onClick={() => void connectionMode.refetch()}
                          >
                            重试连接状态
                          </button>
                        )}
                      </p>
                    )}
                    <div className="osir-section-title">
                      <h2>
                        已添加供应商 <span>{rows.length}</span>
                      </h2>
                      <div className="osir-actions">
                        <button
                          disabled={busy}
                          onClick={() => setDrawer({ tool })}
                        >
                          <Plus size={16} />
                          添加供应商
                        </button>
                        <button
                          disabled={busy || !desktop}
                          onClick={() => void refresh()}
                          aria-label="刷新供应商"
                        >
                          <RefreshCw size={16} />
                        </button>
                      </div>
                    </div>
                    {(providers.isError ||
                      current.isError ||
                      (tool === "codex" && (mode.isError || stack.isError)) ||
                      (tool === "opencode" && openCodeDefault.isError)) && (
                      <div role="alert" className="osir-error">
                        {providers.isError
                          ? "供应商列表读取失败。"
                          : "供应商已保存，使用状态暂时无法确认。"}
                        <button
                          disabled={
                            busy || providers.isFetching || current.isFetching
                          }
                          onClick={() => void refresh()}
                        >
                          重新读取
                        </button>
                      </div>
                    )}
                    {providers.isPending && desktop ? (
                      <div className="osir-empty" role="status">
                        正在读取供应商…
                      </div>
                    ) : providers.isError && desktop && rows.length === 0 ? (
                      <div className="osir-empty">
                        供应商列表暂不可用，请点击刷新重试。
                      </div>
                    ) : rows.length === 0 ? (
                      <div className="osir-empty">
                        <KeyRound size={28} />
                        <h3>添加第一个供应商</h3>
                        <p>选择账户密钥，或手动添加供应商连接。</p>
                        <button
                          className="primary"
                          onClick={() => setDrawer({ tool })}
                        >
                          添加供应商
                        </button>
                      </div>
                    ) : (
                      rows.map((p) => (
                        <article
                          key={p.id}
                          onClick={(event) => {
                            const target = event.target as HTMLElement;
                            if (
                              !target.closest(
                                ".osir-provider-body, button, input, select, textarea, a, label, [role='switch']",
                              )
                            ) {
                              setExpandedProviders((state) => ({
                                ...state,
                                [tool + p.id]: false,
                              }));
                            }
                          }}
                          className={
                            "osir-provider " +
                            (providerInUse(p) ? "active" : "")
                          }
                        >
                          <div className="osir-provider-head">
                            <span className="osir-provider-icon">
                              <ConnectionBrandIcon
                                provider={p}
                                tool={tool}
                                size={24}
                              />
                            </span>
                            <div className="osir-provider-name">
                              <strong>{connectionBrandName(p, tool)}</strong>
                              <small>{p.name}</small>
                            </div>
                            <div className="osir-provider-tags">
                              <span>
                                {modelNames(p, tool).length
                                  ? modelNames(p, tool).length + " 个模型"
                                  : "原生模型"}
                              </span>
                              {(() => {
                                const id =
                                  p.meta?.osirBinding?.binding?.group_id;
                                const group = providerGroups.data?.data.find(
                                  (g) => g.id === id,
                                );
                                return group ? (
                                  <span className="osir-group-tag">
                                    {group.name || "已关联分组"}
                                    {group.subscription_type === "subscription"
                                      ? " · 订阅"
                                      : group.subscription_type === "standard"
                                        ? " · 按量"
                                        : ""}
                                  </span>
                                ) : !p.meta?.osirBinding ? (
                                  <span className="osir-group-tag">
                                    原生配置
                                  </span>
                                ) : null;
                              })()}
                            </div>
                            {providerInUse(p) ? (
                              <span className="osir-badge">使用中</span>
                            ) : !aggregateActive ? (
                              <button
                                disabled={busy}
                                onClick={() => void switchTo(p)}
                              >
                                切换使用
                              </button>
                            ) : (
                              <span className="osir-badge">未接入</span>
                            )}
                            {p.meta?.osirBinding?.masked_key && (
                              <span className="osir-provider-key">
                                Key: {p.meta.osirBinding.masked_key}
                              </span>
                            )}
                            <button
                              disabled={busy}
                              onClick={() => {
                                if (p.meta?.osirBinding) {
                                  requestLeave(() =>
                                    setDrawer({
                                      tool,
                                      provider: p,
                                      source: "account",
                                    }),
                                  );
                                } else if (simpleEditable(p, tool)) {
                                  requestLeave(() =>
                                    setDrawer({ tool, provider: p }),
                                  );
                                } else {
                                  toast.info(
                                    "此连接使用工具原生配置，请在对应工具中调整。",
                                  );
                                }
                              }}
                            >
                              配置
                            </button>
                            <button
                              className="osir-provider-expand"
                              aria-label={`${expandedProviders[tool + p.id] ? "收起" : "展开"} ${p.name} 的模型配置`}
                              aria-expanded={!!expandedProviders[tool + p.id]}
                              aria-controls={`provider-models-${tool}-${p.id}`}
                              onClick={() =>
                                setExpandedProviders((state) => ({
                                  ...state,
                                  [tool + p.id]: !state[tool + p.id],
                                }))
                              }
                            >
                              {expandedProviders[tool + p.id] ? (
                                <ChevronDown size={18} />
                              ) : (
                                <ChevronRight size={18} />
                              )}
                            </button>
                          </div>
                          <div
                            id={`provider-models-${tool}-${p.id}`}
                            className="osir-provider-body"
                            hidden={!expandedProviders[tool + p.id]}
                          >
                            {tool !== "opencode" &&
                              !(
                                (tool === "codex" || tool === "claude") &&
                                (p.meta?.osirBinding || simpleEditable(p, tool))
                              ) && (
                                <div className="osir-provider-models">
                                  {modelNames(p, tool).map((m) => (
                                    <span key={m}>{m}</span>
                                  ))}
                                  {!modelNames(p, tool).length && (
                                    <span>由工具原生登录或配置决定模型</span>
                                  )}
                                </div>
                              )}
                            {tool === "claude" &&
                              (p.meta?.osirBinding ||
                                simpleEditable(p, tool)) && (
                                <ClaudeModelRoles
                                  provider={p}
                                  preview={!desktop}
                                  disabled={
                                    actionBusy ||
                                    (modelOperationCount > 0 &&
                                      !modelOperations.current.has(p.id))
                                  }
                                  onDirtyChange={(dirty) => {
                                    if (dirty) dirtyModels.current.add(p.id);
                                    else dirtyModels.current.delete(p.id);
                                  }}
                                  onBusyChange={(pending) => {
                                    if (pending)
                                      modelOperations.current.add(p.id);
                                    else modelOperations.current.delete(p.id);
                                    setModelOperationCount(
                                      modelOperations.current.size,
                                    );
                                  }}
                                  onSaved={() => {
                                    void refresh();
                                  }}
                                />
                              )}
                            {tool === "codex" &&
                              (p.meta?.osirBinding ||
                                simpleEditable(p, tool)) && (
                                <ModelSelection
                                  provider={p}
                                  onDirtyChange={(dirty) => {
                                    if (dirty) dirtyModels.current.add(p.id);
                                    else dirtyModels.current.delete(p.id);
                                  }}
                                  disabled={busy}
                                  preview={!desktop}
                                  onSave={async (next) => {
                                    if (!desktop) {
                                      setPreviewProviders((s) => ({
                                        ...s,
                                        [tool]: { ...s[tool], [next.id]: next },
                                      }));
                                      return;
                                    }
                                    const ok = await action(async () => {
                                      const confirmed =
                                        await providersApi.update(next, tool);
                                      if (!confirmed)
                                        throw new Error(
                                          "模型保存未确认，请刷新核对后重试。",
                                        );
                                      try {
                                        await syncSavedAggregation(next);
                                      } catch (e) {
                                        throw new Error(
                                          "模型已保存，聚合未完成：" +
                                            connectionError(e),
                                        );
                                      }
                                    }, "模型选择已保存，请重启 Codex 读取新目录。");
                                    if (!ok) throw new Error("保存失败");
                                  }}
                                />
                              )}
                            {tool === "opencode" &&
                              (modelNames(p, tool).length ? (
                                <div className="osir-opencode-model-control">
                                  <label htmlFor={`opencode-model-${p.id}`}>
                                    <strong>模型</strong>
                                    <select
                                      id={`opencode-model-${p.id}`}
                                      aria-label={`${p.name} 的默认模型`}
                                      value={
                                        openCodeDefault.data?.startsWith(
                                          p.id + "/",
                                        ) &&
                                        modelNames(p, tool).includes(
                                          openCodeDefault.data.slice(
                                            p.id.length + 1,
                                          ),
                                        )
                                          ? openCodeDefault.data.slice(
                                              p.id.length + 1,
                                            )
                                          : ""
                                      }
                                      disabled={
                                        !desktop ||
                                        busy ||
                                        openCodeDefault.isPending
                                      }
                                      onChange={(event) => {
                                        const model = event.currentTarget.value;
                                        if (!model) return;
                                        void action(async () => {
                                          await providersApi.addToLiveConfig(
                                            p.id,
                                            tool,
                                          );
                                          await providersApi.setOpenCodeDefaultModel(
                                            p.id + "/" + model,
                                          );
                                        }, "OpenCode 默认模型已更新。");
                                      }}
                                    >
                                      <option value="" disabled>
                                        {openCodeDefault.isPending
                                          ? "正在读取当前模型…"
                                          : "请选择模型"}
                                      </option>
                                      {modelNames(p, tool).map((model) => (
                                        <option key={model} value={model}>
                                          {model}
                                        </option>
                                      ))}
                                    </select>
                                  </label>
                                  {modelNames(p, tool).includes(
                                    openCodeDefault.data?.startsWith(p.id + "/")
                                      ? openCodeDefault.data.slice(
                                          p.id.length + 1,
                                        )
                                      : "",
                                  ) && (
                                    <span className="osir-opencode-model-status">
                                      当前默认模型
                                    </span>
                                  )}
                                </div>
                              ) : (
                                <div
                                  className="osir-opencode-model-control"
                                  role="status"
                                >
                                  暂无已配置模型
                                </div>
                              ))}
                          </div>
                        </article>
                      ))
                    )}
                    {tool === "codex" && stack.data && (
                      <CodexCatalogStatus
                        stack={stack.data}
                        disabled={!desktop || busy}
                        onAction={action}
                      />
                    )}
                    {tool === "codex" && connectionSource === "osir_api" && (
                      <div className="osir-capability osir-aggregation-control">
                        <Layers size={20} />
                        <div>
                          <strong id="osir-aggregation-label">启用聚合</strong>
                          <p role="status">
                            {!desktop
                              ? "仅预览"
                              : mode.isError
                                ? "状态读取失败，请刷新"
                                : mode.isPending
                                  ? "正在读取状态"
                                  : mode.data?.mode === "stack" &&
                                      mode.data.attached
                                    ? aggregationEnabled
                                      ? "已生效"
                                      : "仍在生效，关闭未完成，请重试"
                                    : aggregationEnabled
                                      ? rows.length
                                        ? "尚未生效，保存连接后接入"
                                        : "待添加连接"
                                      : "已关闭"}
                          </p>
                        </div>
                        <Switch
                          aria-labelledby="osir-aggregation-label"
                          checked={aggregationEnabled}
                          disabled={
                            busy ||
                            (desktop && (mode.isPending || mode.isError))
                          }
                          onCheckedChange={(enabled) => {
                            void action(async () => {
                              saveAggregationIntent(enabled);
                              setAggregationEnabled(enabled);
                              if (!desktop) return;
                              if (enabled) await applyAggregation(rows);
                              else await disableAggregation();
                            }, "聚合偏好已保存。");
                          }}
                        />
                      </div>
                    )}
                    <p className="osir-muted">
                      更改连接或模型后，请重新启动工具使配置生效。
                    </p>
                    {tool === "codex" &&
                      (connectionSource === "osir_api" ||
                        imagePanelVisible) && (
                        <OsirImageBindingPanel
                          key={accountSession.data?.generation}
                          openRequest={imageOpenRequest}
                          onOpenHandled={() => setImageOpenRequest(0)}
                        />
                      )}
                  </>
                ) : (
                  <OsirToolManagement
                    embedded
                    section={
                      tab === "sessions"
                        ? "history"
                        : tab === "install"
                          ? "versions"
                          : "extensions"
                    }
                    desktop={desktop}
                    appId={tool}
                    onNavigationBlockedChange={setManagementBlocked}
                    onOpenTerminalSettings={() => {
                      go("settings");
                      setTab("general");
                    }}
                  />
                )}
              </>
            ) : page === "overview" ? (
              <>
                <header className="osir-header">
                  <div>
                    <h1>工作台</h1>
                    <p>账户与本机工具，一处掌握</p>
                  </div>
                  <button
                    className="osir-overview-account-action"
                    aria-label={connected ? "查看 OSIR 账户" : "连接 OSIR 账户"}
                    title={connected ? "查看 OSIR 账户" : "连接 OSIR 账户"}
                    onClick={() => {
                      go("account");
                    }}
                  >
                    <UserRound size={19} aria-hidden="true" />
                  </button>
                </header>
                <AccountOverview onNavigate={go} />
                <AccountOverviewTools
                  connections={overviewConnections}
                  onConfigure={(target) => {
                    if (isManagedTool(target)) go(target);
                    else openToolPage(target);
                  }}
                  onHistory={(target) => openToolPage(target, "sessions")}
                  onManageTools={() => go("apps")}
                  onInstall={(target) => {
                    if (isManagedTool(target)) go(target);
                    else openToolPage(target, "install");
                  }}
                  onNavigationBlockedChange={setManagementBlocked}
                  onOpenImage={connected ? openImageConfiguration : undefined}
                />
              </>
            ) : page === "claude-desktop" ? (
              <OsirClaudeDesktop
                desktop={desktop}
                onNavigationBlockedChange={setManagementBlocked}
                onConnectAccount={() =>
                  requestLeave(() => {
                    setPage("account");
                    setUsageGroup(undefined);
                    setTab("models");
                  })
                }
              />
            ) : page === "apps" || isManagedTool(page) ? (
              <OsirToolsHub
                key={page}
                desktop={desktop}
                selectedTool={isManagedTool(page) ? page : undefined}
                onOpenTool={openToolPage}
                onNavigationBlockedChange={setManagementBlocked}
              />
            ) : page === "invoices" ? (
              <OsirInvoices
                onNavigationBlockedChange={setManagementBlocked}
                onOpenAccount={() => setInvoiceAccountRequested(true)}
              />
            ) : page === "extensions" ? (
              <OsirToolManagement
                section="extensions"
                desktop={desktop}
                onNavigationBlockedChange={setManagementBlocked}
              />
            ) : page === "settings" ? (
              <>
                <OsirTabs
                  label="客户端设置"
                  value={tab === "general" || tab === "about" ? tab : "backup"}
                  items={[
                    { value: "backup", label: "备份与恢复" },
                    { value: "general", label: "通用设置" },
                    { value: "about", label: "关于与更新" },
                  ]}
                  onChange={changeTab}
                />
                {(tab === "general" || tab === "about") && desktop ? (
                  <SettingsPage
                    section={tab === "about" ? "about" : "general"}
                    osirClient
                    onOpenApps={() => go("apps")}
                    onOpenApp={(app) =>
                      TOOL_IDS.includes(app as ToolId) && go(app as ToolId)
                    }
                  />
                ) : (
                  <OsirToolManagement
                    section="backup"
                    desktop={desktop}
                    onNavigationBlockedChange={setManagementBlocked}
                  />
                )}
              </>
            ) : (
              <>
                <header className="osir-header">
                  <div>
                    <h1>{resourceNames[page]}</h1>
                    <p>
                      {page === "account"
                        ? "安全连接 OSIR 账户，同步个人资料与账户资源"
                        : "OSIR 账户资源 · 连接后显示服务端实时数据"}
                    </p>
                  </div>
                </header>
                {page === "account" ? (
                  <>
                    <AccountOverview variant="profile" onNavigate={go} />
                    <OsirAccountPanel
                      compact
                      available={
                        desktop && authCapability.data?.status === "available"
                      }
                      reason={
                        authCapability.data?.reason || "浏览器授权尚未启用"
                      }
                    />
                  </>
                ) : (
                  <>
                    <ResourcePage
                      page={page as "keys" | "subscriptions" | "usage"}
                      initialUsageGroup={usageGroup}
                      onViewUsage={(groupId) => go("usage", groupId)}
                      onOpenTool={(target) => openToolPage(target)}
                      onConnect={() => {
                        go("account");
                      }}
                    />
                  </>
                )}
              </>
            )}
            <footer className="osir-workbench-footer">
              <span>
                {desktop
                  ? (clientBuild.data ?? "OSIR 客户端")
                  : "OSIR 客户端 · 界面预览"}
              </span>
            </footer>
          </Suspense>
        </main>
      </div>
      <Dialog
        open={!!leaveAction}
        onOpenChange={(open) => {
          if (!open) setLeaveAction(null);
        }}
      >
        <DialogContent className="osir-drawer">
          <DialogHeader>
            <DialogTitle>放弃未保存的模型修改？</DialogTitle>
            <DialogDescription>
              模型选择尚未保存。继续离开会丢弃本次修改，已应用的连接保持不变。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <button autoFocus onClick={() => setLeaveAction(null)}>
              继续编辑
            </button>
            <button
              className="primary"
              onClick={() => {
                const next = leaveAction;
                dirtyModels.current.clear();
                setLeaveAction(null);
                next?.();
              }}
            >
              放弃并离开
            </button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={!!deleteTarget}
        onOpenChange={(open) => {
          if (!open && !mutationLock.current) setDeleteTarget(null);
        }}
      >
        <DialogContent
          className="osir-drawer"
          zIndex="alert"
          role="alertdialog"
        >
          <DialogHeader>
            <DialogTitle>删除本地供应商？</DialogTitle>
            <DialogDescription>
              将删除“{deleteTarget?.provider.name}
              ”的本地供应商记录，并由客户端移除相应聚合成员或本地工具配置；不会删除云端账户密钥。
              当前连接或默认路由受保护，请先切换到其他供应商或恢复原生配置后再删除。
            </DialogDescription>
          </DialogHeader>
          {deleteError && (
            <p className="osir-error" role="alert">
              {deleteError} 删除未完成，请核对刷新后的连接状态再重试。
            </p>
          )}
          <DialogFooter>
            <button
              autoFocus
              disabled={busy}
              onClick={() => setDeleteTarget(null)}
            >
              取消
            </button>
            <button
              disabled={busy || !desktop}
              onClick={() => void deleteConnection()}
            >
              {busy ? "正在删除…" : "确认删除"}
            </button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {drawer && (
        <ConnectionDrawer
          key={drawer.tool + ":" + (drawer.provider?.id || "new")}
          tool={drawer.tool}
          provider={drawer.provider}
          onDelete={
            drawer.provider && desktop
              ? () => {
                  setDeleteError("");
                  setDeleteTarget({
                    provider: drawer.provider!,
                    tool: drawer.tool,
                  });
                }
              : undefined
          }
          initialSource={drawer.source}
          reconnectName={drawer.reconnectName}
          onConnectAccount={() => {
            setDrawer(null);
            requestLeave(() => {
              setPage("account");
              setTab("models");
            });
          }}
          preview={!desktop}
          onClose={() => setDrawer(null)}
          onSaved={(p) => saved(p, drawer.tool)}
        />
      )}
    </div>
  );
}
