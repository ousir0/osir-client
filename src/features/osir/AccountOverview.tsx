import { OsirLoading } from "@/components/ui/osir-loading";
import { useEffect, useState } from "react";
import {
  ArrowRight,
  CalendarDays,
  ExternalLink,
  History,
  RefreshCw,
  Receipt,
  Settings2,
  Wallet,
} from "lucide-react";
import {
  useLocalTools,
  LOCAL_TOOL_CATALOG,
  installedToolIds,
  isProviderTool,
  type LocalToolInstallation,
  type SupportedToolId,
  type ToolLaunchResult,
  type ToolLaunchTarget,
} from "./toolsApi";
import { ToolLaunchButton, canLaunchTool } from "./ToolLaunchButton";
import { ToolCloseButton } from "./ToolCloseButton";
import {
  ToolUpdateButton,
  ToolUpdatesCheckButton,
  ToolUpdatesProvider,
  ToolUpdateResults,
} from "./ToolUpdateButton";
import {
  useAccountResource,
  useAccountSession,
  type AccountProfile,
} from "./accountApi";

import "./account-pages.css";
import "./account-overview.css";
import { settingsApi } from "@/lib/api";
import { SupportedToolIcon } from "./SupportedToolIcon";
import { type ToolId } from "./providerConfig";
import { OverviewTokenActivity } from "./OverviewTokenActivity";
import { useOverviewTokenActivity } from "./useOverviewTokenActivity";
import { OsirClientTelemetrySettings } from "./OsirClientTelemetrySettings";
import { AccountAvatar } from "./AccountAvatar";

const money = (value: number | undefined) =>
  value === undefined || !Number.isFinite(value)
    ? "—"
    : new Intl.NumberFormat("en-US", {
        minimumFractionDigits: 2,
        maximumFractionDigits: 2,
      }).format(value);

type Subscription = {
  id: number;
  group_id?: number;
  group?: { id: number; name?: string } | null;
  status?: string;
  expires_at?: string;
};
type Stats = { total_actual_cost?: number; total_requests?: number };
type OverviewProps = {
  variant?: "overview" | "profile";
  onNavigate?: (page: "subscriptions" | "usage") => void;
};

function subscriptionName(sub: Subscription) {
  return sub.group_id != null &&
    sub.group?.id === sub.group_id &&
    sub.group.name
    ? sub.group.name
    : sub.group_id != null
      ? `分组 #${sub.group_id}`
      : `订阅 #${sub.id}`;
}

const localDateTime = (value: string) => {
  const date = new Date(value);
  return Number.isFinite(date.getTime())
    ? new Intl.DateTimeFormat(undefined, {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        timeZoneName: "short",
      }).format(date)
    : "—";
};

/** Mounted only on overview/profile; reuses the existing account resource cache. */
export function AccountOverview({
  variant = "overview",
  onNavigate,
}: OverviewProps = {}) {
  const [now, setNow] = useState(() => Date.now());
  const session = useAccountSession();
  const ready = !!session.data?.connected && !session.isError;
  const activity = useOverviewTokenActivity(variant === "overview" && ready);
  const profile = useAccountResource<AccountProfile>("profile", {}, ready);
  const subscriptions = useAccountResource<Subscription[]>(
    "subscriptions",
    {},
    ready,
  );
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  const parts = new Intl.DateTimeFormat("en", {
    timeZone: timezone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(new Date(now));
  const today = ["year", "month", "day"]
    .map((type) => parts.find((p) => p.type === type)?.value)
    .join("-");
  const stats = useAccountResource<Stats>(
    "usage_stats",
    { timezone, start_date: today, end_date: today },
    ready,
  );
  const generation = session.data?.generation;
  // Login/restoration already verified this profile natively. Keep it visible
  // while the independent resource refresh is pending or temporarily fails.
  const account = ready
    ? profile.data && profile.data.generation === generation
      ? profile.data.data
      : session.data?.profile || undefined
    : undefined;
  const subs =
    ready && subscriptions.data?.generation === generation
      ? subscriptions.data?.data
      : undefined;
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const schedule = () => {
      clearTimeout(timer);
      if (document.visibilityState === "hidden") return;
      const current = Date.now();
      const midnight = new Date(current);
      midnight.setHours(24, 0, 0, 0);
      const expiries = (subs || [])
        .filter((sub) => sub.status === "active")
        .map((sub) => Date.parse(sub.expires_at || ""))
        .filter((expiry) => Number.isFinite(expiry) && expiry > current);
      // Check clock/timezone changes at most once a minute, and wake exactly at
      // midnight or subscription expiry. Hidden windows have no running timer.
      const delay = Math.min(
        60_000,
        midnight.getTime() - current,
        ...expiries.map((expiry) => expiry - current),
      );
      timer = setTimeout(
        () => {
          setNow(Date.now());
          schedule();
        },
        Math.max(1, delay),
      );
    };
    const catchUp = () => {
      setNow(Date.now());
      schedule();
    };
    schedule();
    window.addEventListener("focus", catchUp);
    document.addEventListener("visibilitychange", catchUp);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("focus", catchUp);
      document.removeEventListener("visibilitychange", catchUp);
    };
  }, [subs]);
  const usage =
    ready && stats.data?.generation === generation
      ? stats.data?.data
      : undefined;
  // Match the account service: active status and a future expiry.
  const activeSubscriptions = subs
    ?.filter(
      (s) => s.status === "active" && Date.parse(s.expires_at || "") > now,
    )
    .sort(
      (a, b) =>
        Date.parse(a.expires_at!) - Date.parse(b.expires_at!) || a.id - b.id,
    );
  // Missing status/expiry cannot safely be counted as either active or zero.
  const subscriptionCount =
    subs &&
    subs.every(
      (s) =>
        typeof s.status === "string" &&
        Number.isFinite(Date.parse(s.expires_at || "")),
    )
      ? activeSubscriptions?.length
      : undefined;
  const hint = !ready
    ? session.isError
      ? "账户状态不可用"
      : session.isPending
        ? "正在读取账户状态…"
        : "登录 OSIR 账户后同步"
    : "资源暂不可用";
  if (variant === "overview") {
    const refreshing =
      profile.isFetching || subscriptions.isFetching || stats.isFetching;
    const failed = [profile, subscriptions, stats].some(
      (query) => query.isError,
    );
    return (
      <section
        className="osir-account-pages osir-account-overview osir-load-reveal"
        data-variant="overview"
        aria-label="账户总览"
      >
        <div className="osir-stats">
          <article aria-label="账户余额">
            <h3 className="osir-overview-card-title">账户余额（USD）</h3>
            <strong>{money(account?.balance)}</strong>
            <span className="sr-only">
              {profile.isError
                ? account
                  ? "刷新失败；显示最近记录的余额"
                  : "余额暂不可用"
                : account && profile.data?.generation !== generation
                  ? "登录时的余额，正在同步最新数据…"
                  : !account
                    ? hint
                    : ""}
            </span>
            {ready && (
              <div className="osir-overview-card-action">
                <AccountWebsiteButton label="充值" iconOnly />
              </div>
            )}
          </article>
          <article aria-label="有效订阅摘要">
            <h3 className="osir-overview-card-title">有效订阅</h3>
            <strong>
              {subscriptionCount ?? "—"}
              <span className="osir-overview-count-unit">项</span>
            </strong>
            {ready && onNavigate && (
              <button
                type="button"
                className="osir-overview-card-action osir-overview-metric-icon"
                aria-label="查看订阅"
                title="查看订阅"
                onClick={() => onNavigate("subscriptions")}
              >
                <ArrowRight size={16} aria-hidden="true" />
              </button>
            )}
          </article>
          <article aria-label="当日实际扣费">
            <h3 className="osir-overview-card-title">当日实际扣费（USD）</h3>
            <strong>{money(usage?.total_actual_cost)}</strong>
            <Receipt
              className="osir-overview-metric-glyph"
              size={17}
              aria-hidden="true"
            />
          </article>
        </div>
        {refreshing && !failed && (
          <OsirLoading label="正在同步账户资源…" compact />
        )}
        {failed && (
          <p className="osir-overview-resource-error" role="alert">
            部分账户资源读取失败，已保留最近可用数据。
            <button
              type="button"
              className="osir-overview-metric-icon"
              aria-label="重试账户资源"
              title="重试账户资源"
              disabled={refreshing}
              onClick={() => {
                void profile.refetch();
                void subscriptions.refetch();
                void stats.refetch();
              }}
            >
              <RefreshCw size={15} aria-hidden="true" />
            </button>
          </p>
        )}
        {session.isEventError && (
          <p className="osir-overview-resource-error" role="alert">
            账户状态同步暂不可用，当前资料仍可读取。
            <button type="button" onClick={() => void session.refetch()}>
              重试账户状态同步
            </button>
          </p>
        )}
        {session.isError && (
          <button
            type="button"
            disabled={session.isFetching}
            onClick={() => void session.refetch()}
          >
            重试账户状态
          </button>
        )}
        <OverviewTokenActivity
          {...activity}
          onViewUsage={
            ready && onNavigate ? () => onNavigate("usage") : undefined
          }
        />
      </section>
    );
  }
  return (
    <section
      className="osir-account-pages osir-account-overview"
      data-variant={variant}
      aria-label="个人中心账户摘要"
    >
      {!ready && !session.isPending && !session.isError && (
        <p className="osir-muted">
          登录账户后查看余额、订阅和用量。没有订阅或余额也可登录。
        </p>
      )}
      {account && <AccountIdentity account={account} />}
      <div className="osir-stats">
        <article aria-label="账户余额">
          <h3 className="osir-overview-card-title">
            <Wallet size={18} aria-hidden="true" />
            账户余额（USD）
          </h3>
          <strong>{money(account?.balance)}</strong>
          {(profile.isError ||
            !account ||
            profile.data?.generation !== generation) && (
            <small>
              {profile.isError
                ? account
                  ? profile.data?.generation === generation
                    ? "刷新失败，显示上次记录"
                    : "刷新失败；显示最近登录时的余额"
                  : "余额暂不可用，请刷新重试"
                : account
                  ? "登录时的余额，正在同步最新数据…"
                  : profile.isFetching
                    ? "正在读取余额…"
                    : hint}
            </small>
          )}
          {ready && (
            <div className="osir-overview-card-action">
              <AccountWebsiteButton label="充值" />
            </div>
          )}
        </article>
        <article aria-label="有效订阅摘要">
          <h3 className="osir-overview-card-title">
            <CalendarDays size={18} aria-hidden="true" />
            有效订阅
          </h3>
          <strong>
            {subscriptionCount ?? "—"}
            <span className="osir-overview-count-unit">项</span>
          </strong>
          {!!activeSubscriptions?.length && (
            <details className="osir-overview-subscription-details">
              <summary>查看到期信息</summary>
              <ul
                className="osir-overview-subscriptions"
                aria-label="当前有效订阅"
              >
                {activeSubscriptions.slice(0, 2).map((sub) => (
                  <li key={sub.id}>
                    <span>{subscriptionName(sub)}</span>
                    <time
                      dateTime={sub.expires_at}
                      title={localDateTime(sub.expires_at!)}
                    >
                      {Date.parse(sub.expires_at!) - now < 86_400_000
                        ? "不足 1 天到期"
                        : `剩余 ${Math.ceil((Date.parse(sub.expires_at!) - now) / 86_400_000)} 天`}
                    </time>
                  </li>
                ))}
              </ul>
              {(activeSubscriptions?.length ?? 0) > 2 && (
                <small>另有 {activeSubscriptions!.length - 2} 项有效订阅</small>
              )}
            </details>
          )}
          {(subscriptions.isError ||
            subscriptionCount === undefined ||
            subscriptionCount === 0) && (
            <small>
              {subscriptions.isError
                ? subs
                  ? "刷新失败，显示上次记录"
                  : "订阅暂不可用，请刷新重试"
                : subscriptions.isFetching && !subs
                  ? "正在读取订阅…"
                  : subscriptionCount === 0
                    ? "暂无有效订阅"
                    : subscriptionCount !== undefined
                      ? ""
                      : subs
                        ? "部分订阅信息暂不可用"
                        : hint}
            </small>
          )}
          {ready && onNavigate && (
            <button
              type="button"
              className="osir-overview-card-action"
              aria-label="查看订阅"
              onClick={() => onNavigate("subscriptions")}
            >
              查看订阅
              <ArrowRight size={14} aria-hidden="true" />
            </button>
          )}
        </article>
      </div>
      {ready && (
        <section className="osir-account-privacy" aria-label="账户与隐私">
          <h3>账户与隐私</h3>
          <div>
            <span>账户安全</span>
            <p>账户资料与安全设置在 OSIR 网站管理。</p>
            <AccountWebsiteButton label="在网页管理账户" />
          </div>
          <div>
            <span>账户保护</span>
            <p>登录信息安全保存在本机，密钥仅显示部分字符。</p>
          </div>
          <div>
            <span>用量记录</span>
            <p>当日实际扣费（USD）：{money(usage?.total_actual_cost)}。</p>
            {onNavigate && (
              <button type="button" onClick={() => onNavigate("usage")}>
                查看使用记录
                <ArrowRight size={14} aria-hidden="true" />
              </button>
            )}
          </div>
        </section>
      )}
      {ready && <OsirClientTelemetrySettings />}
      {ready && (
        <footer className="osir-overview-sync">
          {profile.isFetching ||
          subscriptions.isFetching ||
          stats.isFetching ? (
            <p role="status">正在同步账户总览…</p>
          ) : null}
          {[profile, subscriptions, stats].some((q) => q.isError) && (
            <p role="alert">部分账户资源读取失败，可刷新重试。</p>
          )}
          {session.isEventError && (
            <p role="alert">
              账户状态同步暂不可用，当前资料仍可读取。
              <button type="button" onClick={() => void session.refetch()}>
                重试账户状态同步
              </button>
            </p>
          )}
          <button
            type="button"
            disabled={
              profile.isFetching || subscriptions.isFetching || stats.isFetching
            }
            onClick={() => {
              void profile.refetch();
              void subscriptions.refetch();
              void stats.refetch();
            }}
          >
            刷新账户总览
          </button>
          {profile.data && profile.data.generation === generation && (
            <p className="osir-muted">
              余额最近同步：{localDateTime(profile.data.fetched_at)}
            </p>
          )}
        </footer>
      )}
      {session.isError && (
        <button
          type="button"
          disabled={session.isFetching}
          onClick={() => void session.refetch()}
        >
          重试账户状态
        </button>
      )}
    </section>
  );
}

export type AccountOverviewToolsProps = {
  onConfigure: (tool: SupportedToolId) => void;
  onHistory?: (tool: ToolId) => void;
  connections?: Partial<Record<ToolId, OverviewToolConnection>>;
  onManageTools?: () => void;
  onInstall?: (tool: SupportedToolId) => void;
  onBusyChange?: (busy: boolean) => void;
  onNavigationBlockedChange?: (blocked: boolean) => void;
  /** Optional host/test adapter; the native command validates at click time. */
  onLaunch?: (
    tool: SupportedToolId,
    target: ToolLaunchTarget,
  ) => Promise<ToolLaunchResult | void>;
  onOpenImage?: () => void;
};
export type OverviewToolConnection = {
  status: "native" | "osir_api" | "loading" | "unavailable" | "error";
  observedAt?: number;
};

/** Local inventory only; card actions never apply configuration. */
export function AccountOverviewTools({
  onConfigure,
  onHistory,
  connections,
  onManageTools,
  onInstall,
  onLaunch,
  onBusyChange,
  onNavigationBlockedChange,
}: AccountOverviewToolsProps) {
  const desktop =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  const versions = useLocalTools(desktop);
  const installed = desktop ? installedToolIds(versions.data) : [];
  return (
    <ToolUpdatesProvider
      desktop={desktop}
      installations={versions.data ?? []}
      onManageDesktop={onInstall ? () => onInstall("codex") : onManageTools}
      onBusyChange={onBusyChange}
      onNavigationBlockedChange={onNavigationBlockedChange}
    >
      <section
        className="osir-account-pages osir-overview-tools"
        aria-labelledby="osir-overview-tools-title"
      >
        <header className="osir-overview-tools-heading">
          <div>
            <h2 id="osir-overview-tools-title">我的工具</h2>
          </div>
          <div className="osir-overview-tools-toolbar">
            <span title="检查已安装工具更新">
              <ToolUpdatesCheckButton
                iconOnly
                className="osir-overview-version-check"
                disabled={!desktop || versions.isFetching}
              />
            </span>
          </div>
        </header>
        <ToolUpdateResults />
        {!desktop ? (
          <p className="osir-overview-tools-empty">
            预览无法检测本机已安装工具。
          </p>
        ) : versions.isError ? (
          <p role="alert">
            安装检测失败，请重新检测。
            <button
              type="button"
              aria-label="重新检测安装"
              onClick={() => void versions.refetch()}
            >
              <RefreshCw size={14} aria-hidden="true" />
            </button>
          </p>
        ) : versions.isFetching && !versions.data ? (
          <p role="status">正在检测已安装工具…</p>
        ) : !installed.length ? (
          <p className="osir-overview-tools-empty">
            暂未检测到已安装工具，可在安装与更新中添加。
          </p>
        ) : null}
        {!!installed.length && (
          <div className="osir-overview-tools-grid">
            {installed.map((tool) => (
              <OverviewToolCard
                key={tool}
                tool={tool}
                desktop={desktop}
                connection={
                  isProviderTool(tool) ? connections?.[tool] : undefined
                }
                version={versions.data!.find((row) => row.id === tool)!}
                failed={versions.isError}
                onConfigure={onConfigure}
                onHistory={onHistory}
                onLaunch={onLaunch}
              />
            ))}
          </div>
        )}
      </section>
    </ToolUpdatesProvider>
  );
}

function OverviewToolCard({
  tool,
  desktop,
  connection,
  version,
  failed,
  onConfigure,
  onHistory,
  onLaunch,
}: {
  tool: SupportedToolId;
  desktop: boolean;
  connection?: OverviewToolConnection;
  version: LocalToolInstallation;
  failed: boolean;
} & Pick<AccountOverviewToolsProps, "onConfigure" | "onHistory" | "onLaunch">) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!connection?.observedAt || connection.observedAt + 60_000 <= now)
      return;
    const timeout = setTimeout(
      () => setNow(Date.now()),
      Math.max(0, connection.observedAt + 60_000 - now + 1),
    );
    return () => clearTimeout(timeout);
  }, [connection?.observedAt, now]);
  const connectionFresh =
    connection?.observedAt !== undefined &&
    connection.observedAt <= now &&
    now - connection.observedAt < 60_000;
  const usable = !failed;
  const status = failed
    ? "检测失败"
    : version.error
      ? "安装异常"
      : LOCAL_TOOL_CATALOG[tool].desktop
        ? "桌面应用"
        : "命令行工具";
  const connectionLabel = !isProviderTool(tool)
    ? tool === "workbuddy"
      ? "自定义模型在配置中管理"
      : "连接在工具内管理"
    : connection?.status === "loading"
      ? "正在读取连接…"
      : connection?.status === "error"
        ? "连接读取失败"
        : connection?.status === "osir_api" && connectionFresh
          ? "OSIR API 配置"
          : connection?.status === "native" && connectionFresh
            ? "原生配置"
            : "进入配置查看连接";
  const canLaunch = usable && canLaunchTool(version);
  const name = LOCAL_TOOL_CATALOG[tool].name;
  const target: ToolLaunchTarget = LOCAL_TOOL_CATALOG[tool].desktop
    ? "desktop"
    : "cli";
  const configureLabel =
    isProviderTool(tool) || tool === "workbuddy" || tool === "claude-desktop"
      ? "配置"
      : "管理";
  return (
    <article
      className="osir-overview-tool-card"
      aria-label={name + " 工具状态"}
    >
      <header className="osir-overview-tool-title">
        <SupportedToolIcon id={tool} size={30} decorative />
        <div>
          <div className="osir-overview-tool-name">
            <h3>{name}</h3>
            <ToolUpdateButton
              tool={tool}
              installation={version}
              desktop={desktop}
              disabled={failed}
            />
          </div>
          <p className="osir-overview-tool-version">
            版本 {version.version || "未知"}
          </p>
        </div>
        <span
          className="osir-overview-tool-status"
          data-unavailable={!usable || !!version.error}
        >
          {status}
        </span>
      </header>
      <p className="osir-overview-tool-connection">{connectionLabel}</p>
      {!canLaunch && (
        <small className="osir-overview-tool-note">
          {!usable ? "请先在安装与更新中检查工具。" : "暂不支持从客户端打开。"}
        </small>
      )}
      <div className="osir-overview-tool-actions">
        <ToolLaunchButton
          tool={tool}
          installation={version}
          desktop={desktop}
          disabled={!usable}
          className={canLaunch ? "primary" : undefined}
          onLaunch={onLaunch ? () => onLaunch(tool, target) : undefined}
        />
        <button
          type="button"
          className="osir-overview-tool-icon"
          onClick={() => onConfigure(tool)}
          aria-label={configureLabel + " " + name}
          title={configureLabel + " " + name}
        >
          <Settings2 size={17} aria-hidden="true" />
        </button>
        {onHistory && isProviderTool(tool) && (
          <button
            type="button"
            className="osir-overview-tool-icon"
            onClick={() => onHistory(tool)}
            aria-label={`查看 ${name} 会话记录`}
            title={`查看 ${name} 会话记录`}
          >
            <History size={17} aria-hidden="true" />
          </button>
        )}
        <ToolCloseButton
          tool={tool}
          installation={version}
          desktop={desktop}
          disabled={failed}
        />
      </div>
    </article>
  );
}

/** Safe identity module for the overview and personal-center shell. */
export function AccountIdentity({ account }: { account: AccountProfile }) {
  const name = account.username || account.email || "账户 #" + account.id;
  return (
    <header className="osir-account-identity">
      <AccountAvatar accountId={account.id} label="更换个人中心头像" />
      <div>
        <h3>{name}</h3>
        <p>{account.email || "邮箱暂不可用"}</p>
        <small>账户 ID：{account.id}</small>
      </div>
      <div className="osir-overview-identity-action">
        <span className="osir-account-badge">OSIR 账户</span>
        <AccountWebsiteButton label="网页管理资料" />
      </div>
    </header>
  );
}

export function AccountWebsiteButton({
  label,
  iconOnly = false,
}: {
  label: string;
  iconOnly?: boolean;
}) {
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  async function open() {
    setFailed(false);
    setBusy(true);
    try {
      if ("__TAURI_INTERNALS__" in window)
        await settingsApi.openExternal("https://osirclaw.com");
      else window.open("https://osirclaw.com", "_blank", "noopener,noreferrer");
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <button
        type="button"
        className={iconOnly ? "osir-overview-metric-icon" : undefined}
        aria-label={iconOnly ? label : undefined}
        title={iconOnly ? label : undefined}
        disabled={busy}
        aria-busy={busy || undefined}
        onClick={() => void open()}
      >
        {iconOnly ? (
          <ExternalLink size={16} aria-hidden="true" />
        ) : busy ? (
          "正在打开…"
        ) : (
          label + " ↗"
        )}
      </button>
      {failed && <p role="alert">无法打开 OSIR 网站，请重试。</p>}
    </>
  );
}
