import {
  useContext,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  Activity,
  CalendarClock,
  CheckCircle2,
  Clock3,
  CreditCard,
  Download,
  ExternalLink,
  Infinity as InfinityIcon,
  KeyRound,
  Layers,
  Plus,
  RefreshCw,
  RotateCcw,
  Search,
  Wallet,
} from "lucide-react";
import { toast } from "sonner";
import { settingsApi } from "@/lib/api";
import { ProviderIcon } from "@/components/ProviderIcon";
import { invoke } from "@tauri-apps/api/core";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import {
  Sheet,
  SheetTrigger,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
  SheetBody,
  SheetFooter,
  SheetClose,
} from "@/components/ui/sheet";
import { AccountKeyCreation } from "./AccountKeyCreation";
import { OsirKeyDetails, type KeyUsageTool } from "./OsirKeyDetails";
import { QueryClientContext, useQueryClient } from "@tanstack/react-query";
import {
  useAccountResource,
  useAccountSession,
  accountQueryRoot,
  type AccountProfile,
  type ResourceReply,
} from "./accountApi";

import "./account-pages.css";
import { OsirTabs } from "./OsirTabs";
import { AccountWebsiteButton } from "./AccountOverview";
import {
  UsageStatsCards,
  UsageModel,
  UsageBadges,
  UsageTokens,
  UsageCost,
  UsageLatency,
  UsageBreakdown,
  usageMoney,
  usageDuration,
  usageKeyName,
  usageGroupName,
  usageBillingMode,
  usageRequest,
  usageTier,
  usageReasoning,
} from "./OsirUsagePresentation";

import { OsirLoading, OsirReveal } from "@/components/ui/osir-loading";

type Page = "keys" | "groups" | "subscriptions" | "usage";
type Params = Record<string, unknown>;
// Render only safe DTO fields. Never stringify wire objects or key bodies.
type Group = {
  id: number;
  name?: string;
  platform?: string;
  description?: string;
  status?: string;
  subscription_type?: string;
  rate_multiplier?: number;
  daily_limit_usd?: number | null;
  weekly_limit_usd?: number | null;
  monthly_limit_usd?: number | null;
  peak_rate_enabled?: boolean;
  peak_start?: string;
  peak_end?: string;
  peak_rate_multiplier?: number;
  image_rate_multiplier?: number;
  video_rate_multiplier?: number;
  image_price_1k?: number | null;
  image_price_2k?: number | null;
  image_price_4k?: number | null;
  is_exclusive?: boolean;
};
type Key = {
  id: number;
  name?: string;
  masked_key?: string;
  group_id?: number | null;
  group?: Group | null;
  status?: string;
  quota?: number;
  quota_used?: number;
  expires_at?: string | null;
  last_used_at?: string | null;
  current_concurrency?: number;
  rate_limit_5h?: number;
  rate_limit_1d?: number;
  rate_limit_7d?: number;
  usage_5h?: number;
  usage_1d?: number;
  usage_7d?: number;
  reset_5h_at?: string | null;
  reset_1d_at?: string | null;
  reset_7d_at?: string | null;
};
type Subscription = {
  id: number;
  group_id?: number;
  group?: Group | null;
  status?: string;
  starts_at?: string;
  expires_at?: string;
  daily_window_start?: string | null;
  weekly_window_start?: string | null;
  monthly_window_start?: string | null;
  daily_usage_usd?: number;
  weekly_usage_usd?: number;
  monthly_usage_usd?: number;
};
type Usage = {
  id: number;
  created_at?: string;
  model?: string;
  group_id?: number | null;
  api_key_id?: number;
  group?: Group | null;
  api_key?: Key | null;
  billing_type?: number;
  billing_mode?: string | null;
  request_type?: string;
  stream?: boolean;
  request_id?: string;
  subscription_id?: number | null;
  openai_ws_mode?: boolean;
  cache_ttl_overridden?: boolean;
  actual_cost?: number;
  total_cost?: number;
  rate_multiplier?: number;
  input_tokens?: number;
  output_tokens?: number;
  cache_creation_tokens?: number;
  cache_read_tokens?: number;
  cache_creation_5m_tokens?: number;
  cache_creation_1h_tokens?: number;
  input_cost?: number;
  output_cost?: number;
  cache_creation_cost?: number;
  cache_read_cost?: number;
  duration_ms?: number | null;
  first_token_ms?: number | null;
  image_count?: number;
  image_size?: string | null;
  image_input_size?: string | null;
  image_output_size?: string | null;
  image_output_tokens?: number;
  image_output_cost?: number;
  image_size_source?: string | null;
  image_size_breakdown?: Record<string, number> | null;
  media_type?: string | null;
  service_tier?: string | null;
  reasoning_effort?: string | null;
};
type Paged<T> = { items: T[]; total: number; page: number; page_size: number };
type Stats = {
  total_requests?: number;
  total_tokens?: number;
  total_cost?: number;
  total_actual_cost?: number;
  total_input_tokens?: number;
  total_output_tokens?: number;
  total_cache_tokens?: number;
  total_cache_creation_tokens?: number;
  total_cache_read_tokens?: number;
  average_duration_ms?: number;
  endpoints?: {
    endpoint?: string;
    requests?: number;
    total_tokens?: number;
    cost?: number;
    actual_cost?: number;
  }[];
};
type Aggregate = {
  date?: string;
  model?: string;
  group_id?: number;
  group_name?: string;
  requests?: number;
  input_tokens?: number;
  output_tokens?: number;
  cache_creation_tokens?: number;
  cache_read_tokens?: number;
  total_tokens?: number;
  cost?: number;
  actual_cost?: number;
};
type Snapshot = {
  generated_at?: string;
  start_date?: string;
  end_date?: string;
  granularity?: string;
  trend?: Aggregate[] | null;
  models?: Aggregate[] | null;
  groups?: Aggregate[] | null;
};
const show = (value: string | number | null | undefined) => value ?? "—";
const usd = (value: number | null | undefined) =>
  value == null || !Number.isFinite(value)
    ? "—"
    : "$" +
      new Intl.NumberFormat("en-US", { maximumFractionDigits: 4 }).format(
        value,
      );
const summaryUsd = (value: number | null | undefined) =>
  value == null || !Number.isFinite(value)
    ? "—"
    : "$" +
      new Intl.NumberFormat("en-US", {
        minimumFractionDigits: 2,
        maximumFractionDigits: 2,
      }).format(value);
const status = (value?: string) =>
  ({
    active: "有效",
    inactive: "已停用",
    expired: "已过期",
    quota_exhausted: "额度耗尽",
    revoked: "已撤销",
    suspended: "已暂停",
  })[value ?? ""] ?? show(value);
function expiryInfo(value?: string | null) {
  if (value === null) return { label: "无到期时间", tone: "neutral" };
  if (value === undefined) return { label: "到期信息未提供", tone: "neutral" };
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp))
    return { label: "到期信息不可用", tone: "neutral" };
  const remaining = timestamp - Date.now();
  if (remaining <= 0) return { label: "已到期", tone: "danger" };
  if (remaining < 86_400_000) return { label: "不足 1 天到期", tone: "danger" };
  const days = Math.ceil(remaining / 86_400_000);
  if (days <= 3) return { label: `剩余 ${days} 天`, tone: "danger" };
  if (days <= 7) return { label: `剩余 ${days} 天`, tone: "warning" };
  return { label: `剩余 ${days} 天`, tone: "success" };
}
function keyStatus(key: Key) {
  if (key.status === "active" && key.expires_at) {
    const expires = Date.parse(key.expires_at);
    if (Number.isFinite(expires) && expires <= Date.now()) return "expired";
  }
  return key.status ?? "unknown";
}
function PlatformBadge({ group }: { group?: Group | null }) {
  const platform = group?.platform?.trim();
  return (
    <span
      className="osir-account-badge osir-platform-badge"
      data-platform={platform}
    >
      {platform &&
      ["openai", "anthropic", "gemini", "google"].includes(platform) ? (
        <ProviderIcon
          name={platformName(platform)}
          icon={platform === "gemini" ? "google" : platform}
          size={14}
          showFallback={false}
        />
      ) : (
        <Layers size={13} aria-hidden="true" />
      )}
      {platform ? platformName(platform) : "未提供平台"}
    </span>
  );
}
function BillingBadge({ group }: { group?: Group | null }) {
  const subscription = group?.subscription_type === "subscription";
  return (
    <span
      className="osir-account-badge"
      data-tone={subscription ? "violet" : "neutral"}
    >
      {subscription ? (
        <CreditCard size={13} aria-hidden="true" />
      ) : (
        <Wallet size={13} aria-hidden="true" />
      )}
      {subscription
        ? "订阅分组"
        : group?.subscription_type === "standard"
          ? "余额分组"
          : "计费方式未提供"}
    </span>
  );
}
function KeyQuota({ item }: { item: Key }) {
  const quota =
    typeof item.quota === "number" && Number.isFinite(item.quota)
      ? item.quota
      : undefined;
  const used =
    typeof item.quota_used === "number" && Number.isFinite(item.quota_used)
      ? item.quota_used
      : undefined;
  const unlimited = quota === 0;
  return (
    <div className="osir-key-quota">
      <div className="osir-key-quota-heading">
        <span>
          <Wallet size={13} aria-hidden="true" />
          额度
        </span>
        <strong>
          {quota === undefined
            ? "上限未提供"
            : unlimited
              ? "不限额"
              : `${summaryUsd(used)} / ${summaryUsd(quota)}`}
        </strong>
      </div>
      {quota !== undefined && quota > 0 && used !== undefined ? (
        <progress
          max={quota}
          value={Math.max(0, Math.min(used, quota))}
          aria-label="密钥额度使用进度"
        />
      ) : (
        <small>
          {unlimited
            ? "按分组权益计费"
            : quota === undefined
              ? "额度信息暂不可用"
              : "用量未提供"}
        </small>
      )}
    </div>
  );
}
function date(value?: string | null) {
  if (!value) return "—";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime())
    ? "—"
    : parsed.toLocaleString("zh-CN", { hour12: false });
}
function groupName(id?: number | null, group?: Group | null) {
  // Existing key's group_id remains authoritative even if a relation is stale.
  return id == null
    ? "—"
    : group?.id === id && group.name
      ? group.name
      : "分组 #" + id;
}
function platformName(platform: string) {
  const names: Record<string, string> = {
    openai: "OpenAI",
    anthropic: "Anthropic",
    gemini: "Gemini",
    antigravity: "Antigravity",
    multi: "多模型",
  };
  return Object.prototype.hasOwnProperty.call(names, platform)
    ? names[platform]
    : platform;
}
const columns: Record<Page, string[]> = {
  keys: ["密钥名称", "所属分组", "状态", "额度（USD）", "详情"],
  groups: ["分组名称", "平台", "计费方式", "倍率", "说明"],
  subscriptions: [
    "订阅分组",
    "状态",
    "周期用量 / 额度（USD）",
    "有效期",
    "窗口起点",
  ],
  usage: [
    "时间 / 详情",
    "模型 / 分组 / 密钥",
    "类型 / 计费",
    "Token / 图片",
    "实际费用（USD）",
    "首字 / 耗时",
  ],
};
function Table({
  labels,
  children,
  className = "",
}: {
  labels: string[];
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={"osir-resource-table max-w-full " + className}>
      <table>
        <thead>
          <tr>
            {labels.map((label) => (
              <th scope="col" key={label}>
                {label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}
function Unconnected({
  page,
  onConnect,
}: {
  page: Page;
  onConnect: () => void;
}) {
  return (
    <>
      {page === "usage" && (
        <div className="osir-stats">
          {["实际扣费", "请求数", "Token"].map((label) => (
            <article key={label}>
              <span>{label}</span>
              <strong>—</strong>
              <small>连接账户后同步</small>
            </article>
          ))}
        </div>
      )}
      <Table labels={columns[page]}>
        <tr>
          <td colSpan={columns[page].length}>
            <div className="osir-empty">
              <h3>连接账户后查看资源</h3>
              <p>当前尚未登录，暂无可展示的账户数据。</p>
              <button className="primary" onClick={onConnect}>
                连接 OSIR 账户
              </button>
            </div>
          </td>
        </tr>
      </Table>
    </>
  );
}
type ResourceProps = {
  page: Page;
  onConnect: () => void;
  onOpenTool?: (tool: KeyUsageTool) => void;
  onViewUsage?: (groupId?: number) => void;
  initialUsageGroup?: number;
};
export function AccountResources(props: ResourceProps) {
  const context = useContext(QueryClientContext);
  if (!("__TAURI_INTERNALS__" in window) || !context)
    return <Unconnected {...props} />;
  return <SessionResources {...props} />;
}
function SessionResources(props: ResourceProps) {
  const session = useAccountSession();
  if (session.isPending) return <p role="status">正在读取账户状态…</p>;
  if (session.isError)
    return (
      <div role="alert">
        账户状态暂不可用。
        <button onClick={() => void session.refetch()}>重试账户状态</button>
      </div>
    );
  if (!session.data?.connected) return <Unconnected {...props} />;
  return (
    <ConnectedResources
      key={session.data.generation + ":" + props.page}
      page={props.page}
      generation={session.data.generation}
      canCreateKeys={session.data.canCreateKeys === true}
      onOpenTool={props.onOpenTool}
      onViewUsage={props.onViewUsage}
      initialUsageGroup={props.initialUsageGroup}
    />
  );
}
// Ignore cached replies belonging to another account, including during logout.
function current<T>(reply: ResourceReply<T> | undefined, generation: number) {
  return reply?.generation === generation ? reply.data : undefined;
}
type QueryState = {
  isPending: boolean;
  isError: boolean;
  isFetching: boolean;
  refetch: () => unknown;
  data?: { fetched_at: string; generation: number };
};
function QueryMessage({
  query,
  hasData,
}: {
  query: QueryState;
  hasData: boolean;
}) {
  if (query.isError)
    return (
      <p role="alert">
        {hasData
          ? "刷新失败，以下为最近成功的数据。"
          : "资源暂不可用，请重试。"}
        <button
          disabled={query.isFetching}
          onClick={() => void query.refetch()}
        >
          重试
        </button>
      </p>
    );
  if (query.isPending || query.isFetching)
    return (
      <OsirLoading
        label={hasData ? "正在刷新…" : "正在加载资源…"}
        compact={hasData}
      />
    );
  return null;
}
function Updated({
  reply,
  label = "最近同步",
}: {
  reply?: { fetched_at: string };
  label?: string;
}) {
  if (!reply) return null;
  const parsed = new Date(reply.fetched_at);
  return (
    <p className="osir-muted">
      {label}：
      {Number.isNaN(parsed.getTime()) ? "—" : date(parsed.toISOString())}
    </p>
  );
}
function todayIn(timezone: string) {
  const parts = new Intl.DateTimeFormat("en", {
    timeZone: timezone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(new Date());
  return ["year", "month", "day"]
    .map((type) => parts.find((part) => part.type === type)?.value)
    .join("-");
}
function Pager({
  data,
  busy,
  requestedPage,
  onPage,
}: {
  data?: Paged<unknown>;
  requestedPage: number;
  busy: boolean;
  onPage: (n: number) => void;
}) {
  if (!data && requestedPage <= 1) return null;
  const page = data?.page ?? requestedPage;
  return (
    <nav aria-label="资源分页" className="osir-resource-toolbar">
      <span>
        {data
          ? `共 ${data.total} 条 · 第 ${page} 页`
          : `第 ${page} 页 · 数据尚不可用`}
      </span>
      <div className="flex gap-2">
        <button disabled={busy || page <= 1} onClick={() => onPage(page - 1)}>
          上一页
        </button>
        <button
          disabled={busy || !data || data.page * data.page_size >= data.total}
          onClick={() => onPage(page + 1)}
        >
          下一页
        </button>
      </div>
    </nav>
  );
}
function Filter({
  page,
  onApply,
  availableGroups,
  initialGroup,
  generation,
}: {
  page: Page;
  availableGroups?: Group[];
  onApply: (params: Params) => void;
  initialGroup?: number;
  generation?: number;
}) {
  const [error, setError] = useState("");
  const form = useRef<HTMLFormElement>(null);
  const [rangeMode, setRangeMode] = useState("today");
  const [rangeStart, setRangeStart] = useState(() =>
    todayIn(Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"),
  );
  const [rangeEnd, setRangeEnd] = useState(rangeStart);
  const [resetVersion, setResetVersion] = useState(0);
  if (page !== "keys" && page !== "usage") return null;
  function applyFilters(form: HTMLFormElement, preset?: "today" | "week") {
    const data = new FormData(form);
    const params: Params = {};
    for (const [key, raw] of data) {
      const value = String(raw).trim();
      if (!value) continue;
      if (["api_key_id", "group_id", "page_size"].includes(key)) {
        const n = Number(value);
        if (!Number.isSafeInteger(n) || n <= 0) {
          setError("请输入有效的正整数 ID。");
          return;
        }
        params[key] = n;
      } else if (key === "billing_type") params[key] = Number(value);
      else if (key === "stream") params[key] = value === "true";
      else params[key] = value;
    }
    if (page === "usage") {
      params.timezone ||=
        Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
      try {
        todayIn(String(params.timezone));
      } catch {
        setError("请输入有效的时区，例如 Asia/Shanghai。");
        return;
      }
      if (preset) {
        const end = todayIn(String(params.timezone));
        const start = new Date(end + "T00:00:00Z");
        // Calendar dates in the selected zone; DST does not change day count.
        if (preset === "week") start.setUTCDate(start.getUTCDate() - 6);
        params.start_date = start.toISOString().slice(0, 10);
        params.end_date = end;
        setRangeStart(String(params.start_date));
        setRangeEnd(end);
        setRangeMode(preset);
      }
      if (
        params.start_date &&
        params.end_date &&
        String(params.start_date) > String(params.end_date)
      ) {
        setError("结束日期不能早于开始日期。");
        return;
      }
      if (params.start_date && !params.end_date)
        params.end_date = params.start_date;
      if (params.end_date && !params.start_date)
        params.start_date = params.end_date;
      if (params.request_type) delete params.stream;
    }
    setError("");
    onApply(params);
  }
  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    applyFilters(event.currentTarget);
  }
  function resetFilters() {
    form.current?.reset();
    setError("");
    setResetVersion((version) => version + 1);
    const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    const today = todayIn(timezone);
    setRangeStart(today);
    setRangeEnd(today);
    setRangeMode("today");
    onApply(
      page === "usage" ? { timezone, start_date: today, end_date: today } : {},
    );
  }
  const actions = (
    <div className="osir-filter-actions">
      <button type="button" onClick={resetFilters}>
        <RotateCcw size={14} aria-hidden="true" />
        重置
      </button>
      <button type="submit" className="osir-filter-query">
        <Search size={14} aria-hidden="true" />
        查询
      </button>
    </div>
  );
  return (
    <form
      ref={form}
      onSubmit={submit}
      className="osir-account-filter"
      data-page={page}
      aria-label={page === "keys" ? "密钥筛选" : "用量筛选"}
    >
      {page === "keys" ? (
        <>
          <Input label="密钥名称搜索" name="search" />
          <Select
            label="密钥状态"
            name="status"
            options={[
              ["active", "有效"],
              ["inactive", "已停用"],
              ["expired", "已过期"],
              ["quota_exhausted", "额度耗尽"],
            ]}
          />
        </>
      ) : (
        <>
          <div className="osir-usage-date-presets">
            <OsirTabs
              label="用量日期范围"
              value={rangeMode}
              items={[
                { value: "today", label: "今天" },
                { value: "week", label: "最近7天" },
                { value: "custom", label: "自定义" },
              ]}
              onChange={(value) => {
                if (value === "custom") {
                  setRangeMode(value);
                  form.current
                    ?.querySelector<HTMLInputElement>('[name="start_date"]')
                    ?.focus();
                } else if (form.current) {
                  applyFilters(
                    form.current,
                    value === "week" ? "week" : "today",
                  );
                }
              }}
            />
            {actions}
          </div>
          <div className="osir-usage-date-range">
            <Input
              label="开始日期"
              name="start_date"
              type="date"
              value={rangeStart}
              onChange={(value) => {
                setRangeStart(value);
                setRangeMode("custom");
              }}
            />
            <span aria-hidden="true">至</span>
            <Input
              label="结束日期"
              name="end_date"
              type="date"
              value={rangeEnd}
              onChange={(value) => {
                setRangeEnd(value);
                setRangeMode("custom");
              }}
            />
          </div>
          <UsageNameFilters
            key={resetVersion}
            generation={generation!}
            initialGroup={resetVersion === 0 ? initialGroup : undefined}
          >
            <Input label="模型" name="model" />
            <Input
              label="时区"
              name="timezone"
              initial={
                Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"
              }
            />
            <Select
              label="请求类型"
              name="request_type"
              options={[
                ["sync", "同步"],
                ["stream", "流式"],
                ["ws_v2", "WebSocket"],
                ["cyber", "安全策略拒绝"],
                ["unknown", "未知类型"],
              ]}
            />
            <Select
              label="流式请求"
              name="stream"
              options={[
                ["true", "是"],
                ["false", "否"],
              ]}
            />
            <Select
              label="计费来源"
              name="billing_type"
              options={[
                ["0", "余额"],
                ["1", "订阅"],
              ]}
            />
            <Select
              label="计费模式"
              name="billing_mode"
              options={[
                ["token", "Token"],
                ["per_request", "按次"],
                ["image", "图片"],
                ["video", "视频"],
              ]}
            />
          </UsageNameFilters>
        </>
      )}
      {page === "keys" && (
        <Select
          label="所属分组"
          name="group_id"
          options={(Array.isArray(availableGroups) ? availableGroups : []).map(
            (group) => [String(group.id), group.name || "分组 #" + group.id],
          )}
        />
      )}
      <details className="osir-account-advanced">
        <summary>排序与分页</summary>
        <div className="osir-account-filter-fields">
          <Select
            label="排序字段"
            name="sort_by"
            blank={false}
            options={
              page === "keys"
                ? [
                    ["created_at", "创建时间"],
                    ["id", "ID"],
                    ["name", "名称"],
                    ["status", "状态"],
                    ["expires_at", "到期时间"],
                    ["last_used_at", "最近使用"],
                  ]
                : [
                    ["created_at", "时间"],
                    ["id", "ID"],
                    ["model", "模型"],
                  ]
            }
          />
          <Select
            label="排序方向"
            name="sort_order"
            blank={false}
            options={[
              ["desc", "降序"],
              ["asc", "升序"],
            ]}
          />
          <Select
            label="每页条数"
            name="page_size"
            blank={false}
            options={[
              ["20", "20"],
              ["50", "50"],
              ["100", "100"],
            ]}
          />
        </div>
      </details>
      {page === "keys" && actions}
      {error && <p role="alert">{error}</p>}
    </form>
  );
}
function UsageNameFilters({
  generation,
  initialGroup,
  children,
}: {
  generation: number;
  initialGroup?: number;
  children?: ReactNode;
}) {
  const [keyPage, setKeyPage] = useState(1);
  const [selectedKey, setSelectedKey] = useState<{
    id: number;
    name?: string;
  }>();
  const [selectedGroup, setSelectedGroup] = useState(
    initialGroup == null ? "" : String(initialGroup),
  );
  const keys = useAccountResource<Paged<Key>>("keys", {
    page: keyPage,
    page_size: 100,
  });
  const groups = useAccountResource<Group[]>("groups");
  const keyData = current(keys.data, generation);
  const groupData = current(groups.data, generation);
  const keyItems = Array.isArray(keyData?.items) ? keyData.items : undefined;
  const groupItems = Array.isArray(groupData) ? groupData : undefined;
  const selectedKeyMissing =
    selectedKey && !keyItems?.some((item) => item.id === selectedKey.id);
  function selector(
    kind: "密钥" | "分组",
    name: string,
    items: { id: number; name?: string }[] | undefined,
    query: QueryState,
    value: string,
    onChange: (value: string) => void,
    missingLabel: string,
  ) {
    const missing = value && !items?.some((item) => String(item.id) === value);
    return (
      <div className="osir-usage-name-filter flex min-w-0 flex-col gap-1">
        <label className="flex flex-col gap-1 text-xs">
          <span className="osir-filter-label">{kind}</span>
          <select
            className="min-h-[34px] rounded border border-slate-200 bg-white px-2 text-sm"
            name={name}
            value={value}
            disabled={!items && !value}
            aria-busy={query.isFetching}
            onChange={(event) => {
              onChange(event.target.value);
              event.currentTarget.form?.requestSubmit();
            }}
          >
            <option value="">全部{kind}</option>
            {missing && <option value={value}>{missingLabel}</option>}
            {items?.map((item) => (
              <option key={item.id} value={item.id}>
                {item.name || kind + " #" + item.id}
                {item.name &&
                items.some(
                  (other) => other.id !== item.id && other.name === item.name,
                )
                  ? " (#" + item.id + ")"
                  : ""}
              </option>
            ))}
          </select>
        </label>
        {query.isError ? (
          <p className="osir-muted" role="alert">
            {kind}名称读取失败{items ? "，保留上次选项" : ""}。
            <button
              type="button"
              disabled={query.isFetching}
              onClick={() => void query.refetch()}
            >
              重试{kind}列表
            </button>
          </p>
        ) : query.isPending || query.isFetching ? (
          <small className="osir-muted" role="status">
            正在读取{kind}名称…
          </small>
        ) : items?.length === 0 ? (
          <small className="osir-muted">当前账户暂无可选{kind}</small>
        ) : null}
      </div>
    );
  }
  return (
    <>
      {selector(
        "分组",
        "group_id",
        groupItems,
        groups,
        selectedGroup,
        setSelectedGroup,
        "分组 #" +
          selectedGroup +
          (groupItems ? "（不在当前可用分组中）" : "（名称暂不可用）"),
      )}
      <details className="osir-account-advanced">
        <summary>更多筛选</summary>
        <div className="osir-account-filter-fields">
          <div className="flex min-w-0 flex-col gap-1">
            {selector(
              "密钥",
              "api_key_id",
              keyItems,
              keys,
              selectedKey ? String(selectedKey.id) : "",
              (value) => {
                const item = keyItems?.find((key) => String(key.id) === value);
                setSelectedKey(
                  item ? { id: item.id, name: item.name } : undefined,
                );
              },
              (selectedKey?.name || "密钥 #" + selectedKey?.id) +
                "（不在当前列表页）",
            )}
            {selectedKeyMissing && (
              <small className="osir-muted">
                已保留所选密钥 ID；切换列表页不会更改用量筛选。
              </small>
            )}
            {keyData && (keyPage > 1 || keyData.total > keyData.page_size) && (
              <div
                className="flex flex-wrap items-center gap-2 text-xs"
                aria-label="密钥选项分页"
              >
                <button
                  type="button"
                  disabled={keys.isFetching || keyPage <= 1}
                  onClick={() => setKeyPage(keyPage - 1)}
                >
                  上一页密钥
                </button>
                <span>
                  第 {keyPage} 页 · 共 {keyData.total} 个
                </span>
                <button
                  type="button"
                  disabled={
                    keys.isFetching ||
                    keyData.page * keyData.page_size >= keyData.total
                  }
                  onClick={() => setKeyPage(keyPage + 1)}
                >
                  下一页密钥
                </button>
              </div>
            )}
          </div>
          {children}
        </div>
      </details>
    </>
  );
}
function Input({
  label,
  name,
  type = "text",
  initial,
  value,
  onChange,
}: {
  label: string;
  name: string;
  type?: string;
  initial?: string;
  value?: string;
  onChange?: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-1 text-xs">
      <span className="osir-filter-label">{label}</span>
      <input
        className="min-h-[34px] w-36 rounded border border-slate-200 bg-white px-2 text-sm"
        name={name}
        type={type}
        title={label}
        min={type === "number" ? 1 : undefined}
        maxLength={256}
        defaultValue={initial}
        value={value}
        onChange={
          onChange ? (event) => onChange(event.target.value) : undefined
        }
      />
    </label>
  );
}
function Select({
  label,
  name,
  options,
  blank = true,
  onChange,
  value,
  disabled,
}: {
  label: string;
  name: string;
  options: string[][];
  blank?: boolean;
  onChange?: (v: string) => void;
  value?: string;
  disabled?: boolean;
}) {
  return (
    <label className="flex flex-col gap-1 text-xs">
      <span className="osir-filter-label">{label}</span>
      <select
        className="min-h-[34px] rounded border border-slate-200 bg-white px-2 text-sm"
        name={name}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange?.(event.target.value)}
      >
        {blank && <option value="">全部</option>}
        {options.map(([v, l]) => (
          <option key={v} value={v}>
            {l}
          </option>
        ))}
      </select>
    </label>
  );
}
function SubscriptionCards({
  items,
  profile,
  generation,
  onViewUsage,
}: {
  items?: Subscription[];
  profile: QueryState & { data?: ResourceReply<AccountProfile> };
  generation: number;
  onViewUsage?: (groupId?: number) => void;
}) {
  const balance = current(profile.data, generation)?.balance;
  const hasBalance = typeof balance === "number" && Number.isFinite(balance);
  const [tab, setTab] = useState<"current" | "expired">("current");
  const [websiteBusy, setWebsiteBusy] = useState(false);
  async function openSubscriptions() {
    if (websiteBusy) return;
    setWebsiteBusy(true);
    try {
      await settingsApi.openExternal("https://osirclaw.com/subscriptions");
    } catch {
      toast.error("无法打开订阅管理页面，请重试。", { duration: 4000 });
    } finally {
      setWebsiteBusy(false);
    }
  }
  const isExpired = (sub: Subscription) =>
    sub.status === "expired" ||
    (Number.isFinite(Date.parse(sub.expires_at || "")) &&
      Date.parse(sub.expires_at!) <= Date.now());
  const isHistorical = (sub: Subscription) =>
    isExpired(sub) ||
    ["revoked", "suspended", "inactive"].includes(sub.status ?? "");
  const visible = items?.filter(
    (sub) => isHistorical(sub) === (tab === "expired"),
  );
  return (
    <div className="osir-account-subscriptions">
      <OsirTabs
        label="订阅范围"
        value={tab}
        onChange={(value) => setTab(value as "current" | "expired")}
        items={[
          { value: "current", label: "当前订阅" },
          { value: "expired", label: "历史订阅" },
        ]}
      />
      <div className="osir-account-subscription-layout">
        <div className="osir-account-subscription-list">
          {visible?.map((sub) => {
            // Only the matching relation can supply this subscription's limits.
            const group =
              sub.group?.id === sub.group_id ? sub.group : undefined;
            const expiryState = expiryInfo(sub.expires_at);
            const platform = group?.platform?.toLowerCase();
            const brand =
              platform === "openai"
                ? "OpenAI"
                : platform === "anthropic"
                  ? "Anthropic"
                  : platform === "gemini" || platform === "google"
                    ? "Google"
                    : platform === "multi"
                      ? "多模型"
                      : group?.platform;
            return (
              <article
                className="osir-account-subscription"
                data-platform={platform}
                key={sub.id}
              >
                <header>
                  <div className="osir-subscription-heading">
                    {brand &&
                    ["openai", "anthropic", "google", "gemini"].includes(
                      platform || "",
                    ) ? (
                      <ProviderIcon
                        name={brand}
                        icon={platform === "gemini" ? "google" : platform}
                        size={24}
                        showFallback={false}
                      />
                    ) : (
                      <Layers size={24} aria-hidden="true" />
                    )}
                    <div>
                      <h3>{groupName(sub.group_id, sub.group)}</h3>
                      <div
                        className="osir-subscription-meta"
                        aria-label="包含权益"
                      >
                        <PlatformBadge group={group} />
                        <span>#{sub.id}</span>
                        {typeof group?.rate_multiplier === "number" &&
                          Number.isFinite(group.rate_multiplier) && (
                            <span>倍率 {group.rate_multiplier}×</span>
                          )}
                      </div>
                    </div>
                  </div>
                  <span
                    className="osir-account-badge"
                    data-tone={
                      sub.status === "active" && !isExpired(sub)
                        ? "success"
                        : "neutral"
                    }
                  >
                    {sub.status === "active" && !isExpired(sub) && (
                      <CheckCircle2 size={13} aria-hidden="true" />
                    )}
                    {["revoked", "suspended", "inactive"].includes(
                      sub.status ?? "",
                    )
                      ? status(sub.status)
                      : isExpired(sub)
                        ? "已到期"
                        : sub.status
                          ? status(sub.status)
                          : "状态未提供"}
                  </span>
                </header>
                <div className="osir-subscription-period">
                  <span>
                    <CalendarClock size={14} aria-hidden="true" />
                    到期时间
                  </span>
                  <strong>{date(sub.expires_at)}</strong>
                  <span
                    className="osir-account-badge"
                    data-tone={expiryState.tone}
                  >
                    {expiryState.label}
                  </span>
                </div>
                <div className="osir-account-quotas">
                  {(["daily", "weekly", "monthly"] as const).map(
                    (period, index) => {
                      const used =
                        sub[(period + "_usage_usd") as "daily_usage_usd"];
                      const limit =
                        group?.[(period + "_limit_usd") as "daily_limit_usd"];
                      const hasUsage =
                        typeof used === "number" && Number.isFinite(used);
                      const hasLimit =
                        typeof limit === "number" && Number.isFinite(limit);
                      const measured = hasUsage && hasLimit && limit > 0;
                      // OSIR only enforces positive group limits; absent fields stay unavailable.
                      const unlimited = limit === null || limit === 0;
                      const percentage = measured
                        ? Math.max(0, (used / limit) * 100)
                        : undefined;
                      return (
                        <section
                          key={period}
                          aria-label={["每日", "每周", "每月"][index] + "额度"}
                        >
                          <span>{["每日", "每周", "每月"][index]}额度</span>
                          <strong>
                            {summaryUsd(used)} /{" "}
                            {unlimited ? "不限额" : summaryUsd(limit)}
                          </strong>
                          {measured ? (
                            <>
                              <progress
                                max={limit}
                                value={Math.max(0, Math.min(used, limit))}
                                aria-label={
                                  ["每日", "每周", "每月"][index] + "已用额度"
                                }
                                data-level={
                                  percentage! >= 90
                                    ? "high"
                                    : percentage! >= 70
                                      ? "medium"
                                      : "normal"
                                }
                              />
                              <small>
                                <span>{percentage!.toFixed(0)}% 已用</span>
                                <span>
                                  剩余 {summaryUsd(Math.max(0, limit - used))}
                                </span>
                              </small>
                            </>
                          ) : (
                            <small>
                              {unlimited ? (
                                <>
                                  <InfinityIcon size={13} aria-hidden="true" />
                                  此周期不限额
                                </>
                              ) : (
                                "额度进度暂不可用"
                              )}
                            </small>
                          )}
                        </section>
                      );
                    },
                  )}
                </div>
                <footer className="osir-subscription-actions">
                  <details className="osir-subscription-details">
                    <summary>订阅详情</summary>
                    {group?.description && <p>{group.description}</p>}
                    <p>开始时间：{date(sub.starts_at)}</p>
                    <p>
                      周期起点 · 日 {date(sub.daily_window_start)} · 周{" "}
                      {date(sub.weekly_window_start)} · 月{" "}
                      {date(sub.monthly_window_start)}
                    </p>
                  </details>
                  <div className="osir-subscription-action-buttons">
                    {onViewUsage && (
                      <button
                        type="button"
                        onClick={() => onViewUsage(sub.group_id)}
                      >
                        <Activity size={14} aria-hidden="true" />
                        查看用量
                      </button>
                    )}
                    <button
                      type="button"
                      disabled={websiteBusy}
                      onClick={() => void openSubscriptions()}
                    >
                      <ExternalLink size={14} aria-hidden="true" />
                      {websiteBusy ? "正在打开…" : "网页续订"}
                    </button>
                  </div>
                </footer>
              </article>
            );
          })}
          {visible?.length === 0 && (
            <div className="osir-empty">
              {items?.length === 0
                ? "暂无订阅"
                : tab === "expired"
                  ? "暂无历史订阅"
                  : "暂无当前订阅"}
            </div>
          )}
        </div>
        <aside className="osir-account-wallet" aria-label="按量账户余额">
          <h3>
            <Wallet size={18} aria-hidden="true" />
            按量账户余额
          </h3>
          <strong>{summaryUsd(balance)}</strong>
          <QueryMessage query={profile} hasData={hasBalance} />
          <AccountWebsiteButton label="前往 OSIR 网站" />
          <p className="osir-muted">
            {hasBalance ? "余额与订阅额度独立计算" : "余额暂未提供"}
          </p>
          <Updated
            label="余额同步"
            reply={
              profile.data?.generation === generation ? profile.data : undefined
            }
          />
        </aside>
      </div>
    </div>
  );
}
function KeyMetadata({ item: key }: { item: Key }) {
  return (
    <>
      <p>
        有效期：
        {key.expires_at === null ? "永不过期" : date(key.expires_at)}
      </p>
      <p>
        最近使用：{date(key.last_used_at)} · 当前并发：
        {show(key.current_concurrency)}
      </p>
      {(["5h", "1d", "7d"] as const).map((window) => (
        <p key={window}>
          {window} 已用 {usd(key[("usage_" + window) as "usage_5h"])} / 上限{" "}
          {key[("rate_limit_" + window) as "rate_limit_5h"] === 0
            ? "不限额"
            : usd(key[("rate_limit_" + window) as "rate_limit_5h"])}{" "}
          · 重置 {date(key[("reset_" + window + "_at") as "reset_5h_at"])}
        </p>
      ))}
    </>
  );
}
function ResourceDrawer({
  title,
  description,
  trigger,
  children,
}: {
  title: string;
  description: string;
  trigger: ReactNode;
  children: ReactNode;
}) {
  return (
    <Sheet>
      <SheetTrigger asChild>{trigger}</SheetTrigger>
      <SheetContent
        className="osir-drawer osir-account-detail-drawer"
        width={420}
        closeLabel={"关闭" + title}
        dismissOnOutsideClick
      >
        <SheetHeader>
          <SheetTitle>{title}</SheetTitle>
          <SheetDescription>{description}</SheetDescription>
        </SheetHeader>
        <SheetBody>{children}</SheetBody>
        <SheetFooter>
          <SheetClose asChild>
            <button type="button">关闭</button>
          </SheetClose>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
function CreateAccountKey({
  generation,
  canCreateKeys,
  onCreated,
}: {
  generation: number;
  canCreateKeys: boolean;
  onCreated: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [confirmClose, setConfirmClose] = useState(false);
  const [actionContainer, setActionContainer] = useState<HTMLDivElement | null>(
    null,
  );
  function close() {
    if (busy) return;
    if (dirty) {
      setConfirmClose(true);
      return;
    }
    setOpen(false);
  }
  return (
    <>
      <Sheet
        open={open}
        onOpenChange={(next) => {
          if (next) {
            setDirty(false);
            setOpen(true);
          } else close();
        }}
      >
        <SheetTrigger asChild>
          <button type="button" className="primary">
            <Plus size={15} aria-hidden="true" />
            创建密钥
          </button>
        </SheetTrigger>
        <SheetContent
          className="osir-drawer osir-account-detail-drawer osir-account-create-drawer"
          width={420}
          closeLabel="关闭创建密钥"
        >
          <SheetHeader>
            <SheetTitle>创建分组密钥</SheetTitle>
            <SheetDescription>
              选择所属分组，创建后可在工具配置中使用。
            </SheetDescription>
          </SheetHeader>
          <SheetBody>
            <AccountKeyCreation
              generation={generation}
              canCreateKeys={canCreateKeys}
              onBusy={setBusy}
              onDirtyChange={setDirty}
              actionContainer={actionContainer}
              onCreated={() => {
                setDirty(false);
                setOpen(false);
                onCreated();
              }}
            />
          </SheetBody>
          <SheetFooter>
            <button type="button" disabled={busy} onClick={close}>
              取消
            </button>
            <div
              className="osir-account-create-action"
              ref={setActionContainer}
            />
          </SheetFooter>
        </SheetContent>
      </Sheet>
      <ConfirmDialog
        isOpen={confirmClose}
        title="放弃未保存的密钥设置？"
        message="尚未提交的名称、分组和额度设置将被丢弃。"
        confirmText="放弃设置"
        cancelText="继续编辑"
        variant="info"
        onCancel={() => setConfirmClose(false)}
        onConfirm={() => {
          setConfirmClose(false);
          setDirty(false);
          setOpen(false);
        }}
      />
    </>
  );
}
function ConnectedResources({
  page,
  generation,
  canCreateKeys,
  onOpenTool,
  onViewUsage,
  initialUsageGroup,
}: {
  page: Page;
  generation: number;
  canCreateKeys: boolean;
  onOpenTool?: (tool: KeyUsageTool) => void;
  onViewUsage?: (groupId?: number) => void;
  initialUsageGroup?: number;
}) {
  const client = useQueryClient();
  const [filters, setFilters] = useState<Params>({});
  const [groupPlatform, setGroupPlatform] = useState("");
  const [pageNumber, setPageNumber] = useState(1);
  const parameters = { page: pageNumber, page_size: 20, ...filters };
  const keys = useAccountResource<Paged<Key>>(
    "keys",
    parameters,
    page === "keys",
  );
  const groups = useAccountResource<Group[]>(
    "groups",
    {},
    page === "groups" || page === "keys",
  );
  const rates = useAccountResource<Record<string, number> | null>(
    "group_rates",
    {},
    page === "groups",
  );
  const subscriptions = useAccountResource<Subscription[]>(
    "subscriptions",
    {},
    page === "subscriptions",
  );
  const profile = useAccountResource<AccountProfile>(
    "profile",
    {},
    page === "subscriptions",
  );
  const query =
    page === "keys" ? keys : page === "groups" ? groups : subscriptions;
  const data = current<Paged<Key> | Group[] | Subscription[]>(
    query.data,
    generation,
  );
  if (page === "usage")
    return (
      <UsageResources
        generation={generation}
        initialGroup={initialUsageGroup}
      />
    );
  const keyData = current(keys.data, generation);
  const groupData = current(groups.data, generation);
  const subscriptionData = current(subscriptions.data, generation);
  const rateData = current(rates.data, generation);
  // Available groups are unpaged. Never apply this filter to a key page or
  // send it as a query parameter: neither native nor user key APIs accept it.
  const platformGroups =
    page === "groups" && Array.isArray(groupData) ? groupData : undefined;
  const platforms = [
    ...new Set(
      platformGroups?.flatMap((group) =>
        group.platform?.trim() ? [group.platform] : [],
      ) ?? [],
    ),
  ];
  const platformOptions = platforms.map((platform) => [
    platform,
    platformName(platform),
  ]);
  if (groupPlatform && !platforms.includes(groupPlatform)) {
    platformOptions.push([
      groupPlatform,
      platformName(groupPlatform) + "（当前无可用分组）",
    ]);
  }
  const visibleGroups = platformGroups?.filter(
    (group) => !groupPlatform || group.platform === groupPlatform,
  );
  const rows =
    page === "keys"
      ? keyData?.items
      : page === "groups"
        ? visibleGroups
        : subscriptionData;
  return (
    <section
      className="osir-account-pages osir-account-resource"
      aria-label="账户资源"
    >
      <Filter
        page={page}
        availableGroups={groupData}
        onApply={(params) => {
          setFilters(params);
          setPageNumber(1);
        }}
      />
      {page === "groups" && (
        <form
          className="osir-account-filter"
          aria-label="分组筛选"
          onSubmit={(event) => event.preventDefault()}
        >
          <Select
            label="分组平台"
            name="platform"
            value={groupPlatform}
            disabled={platformGroups === undefined}
            options={platformOptions}
            onChange={setGroupPlatform}
          />
          <p className="osir-muted">
            仅筛选当前账户的可用分组；密钥列表请按所属分组筛选。
          </p>
        </form>
      )}
      <div className="osir-resource-toolbar">
        <span>
          {page === "keys"
            ? "密钥所属分组只读"
            : page === "groups"
              ? "基础倍率与账户专属倍率分别展示"
              : "各订阅额度独立，不与余额相加"}
        </span>
        <div className="osir-resource-actions">
          {page === "keys" && (
            <CreateAccountKey
              generation={generation}
              canCreateKeys={canCreateKeys}
              onCreated={() => {
                toast.success("密钥已创建，可在工具配置中选择使用", {
                  duration: 3500,
                });
                void client.invalidateQueries({
                  queryKey: [...accountQueryRoot, generation, "keys"],
                });
              }}
            />
          )}
          <button
            disabled={
              query.isFetching ||
              (page === "groups" && rates.isFetching) ||
              (page === "subscriptions" && profile.isFetching)
            }
            onClick={() => {
              void query.refetch();
              if (page === "groups") void rates.refetch();
              if (page === "subscriptions") void profile.refetch();
            }}
          >
            <RefreshCw size={14} aria-hidden="true" />
            刷新资源
          </button>
        </div>
      </div>
      <QueryMessage query={query} hasData={data !== undefined} />
      {page === "keys" && (
        <>
          <QueryMessage query={groups} hasData={groupData !== undefined} />
        </>
      )}
      {page === "groups" && (
        <QueryMessage query={rates} hasData={rateData !== undefined} />
      )}
      <OsirReveal>
        <div className="osir-account-resource-layout">
          {page === "subscriptions" ? (
            <SubscriptionCards
              items={subscriptionData}
              profile={profile}
              generation={generation}
              onViewUsage={onViewUsage}
            />
          ) : (
            <Table
              labels={columns[page]}
              className={
                page === "keys" ? "osir-keys-table" : "osir-groups-table"
              }
            >
              {page === "keys" &&
                keyData?.items.map((key) => (
                  <tr key={key.id}>
                    <td>
                      <div className="osir-resource-name">
                        <span className="osir-resource-symbol">
                          <KeyRound size={16} aria-hidden="true" />
                        </span>
                        <strong>{show(key.name)}</strong>
                      </div>
                      <small className="osir-key-mask">
                        ID {key.id} ·{" "}
                        {key.masked_key &&
                        /^•{4}[a-zA-Z0-9_-]{0,4}$/.test(key.masked_key)
                          ? key.masked_key
                          : "••••"}
                      </small>
                      <small className="osir-key-last-used">
                        <Clock3 size={12} aria-hidden="true" />
                        {key.last_used_at === null
                          ? "尚未使用"
                          : key.last_used_at
                            ? date(key.last_used_at)
                            : "最近使用未提供"}
                      </small>
                    </td>
                    <td>
                      <div className="osir-key-group">
                        <strong>{groupName(key.group_id, key.group)}</strong>
                        {key.group_id != null &&
                          key.group?.id === key.group_id && (
                            <>
                              <PlatformBadge group={key.group} />
                              <BillingBadge group={key.group} />
                            </>
                          )}
                      </div>
                    </td>
                    <td>
                      <span
                        className="osir-account-badge osir-key-state"
                        data-tone={
                          keyStatus(key) === "active"
                            ? "success"
                            : ["expired", "quota_exhausted"].includes(
                                  keyStatus(key),
                                )
                              ? "warning"
                              : "neutral"
                        }
                      >
                        {keyStatus(key) === "active" && (
                          <CheckCircle2 size={13} aria-hidden="true" />
                        )}
                        {keyStatus(key) === "unknown"
                          ? "状态未提供"
                          : status(keyStatus(key))}
                      </span>
                      <small
                        className="osir-key-expiry"
                        title={
                          key.expires_at ? date(key.expires_at) : undefined
                        }
                        data-tone={expiryInfo(key.expires_at).tone}
                      >
                        <CalendarClock size={12} aria-hidden="true" />
                        {expiryInfo(key.expires_at).label}
                      </small>
                    </td>
                    <td>
                      <KeyQuota item={key} />
                      <small className="osir-key-use-summary">
                        已用 {summaryUsd(key.quota_used)}
                        {typeof key.current_concurrency === "number" && (
                          <> · 并发 {key.current_concurrency}</>
                        )}
                      </small>
                    </td>
                    <td>
                      <OsirKeyDetails
                        generation={generation}
                        keyId={key.id}
                        status={keyStatus(key)}
                        onOpenTool={onOpenTool}
                      >
                        <h3>{show(key.name)}</h3>
                        <span className="osir-account-badge">
                          {groupName(key.group_id, key.group)}
                        </span>
                        <p>
                          {status(keyStatus(key))} · 已用{" "}
                          {summaryUsd(key.quota_used)} /{" "}
                          {key.quota === 0 ? "不限额" : summaryUsd(key.quota)}
                        </p>
                        <KeyMetadata item={key} />
                        <p className="osir-muted">
                          所属分组只读。此页面不显示完整密钥。
                        </p>
                      </OsirKeyDetails>
                    </td>
                  </tr>
                ))}
              {page === "groups" &&
                visibleGroups?.map((group) => (
                  <tr key={group.id}>
                    <td>
                      <div className="osir-resource-name">
                        <span className="osir-resource-symbol">
                          <Layers size={16} aria-hidden="true" />
                        </span>
                        <strong>{show(group.name)}</strong>
                      </div>
                      <small className="block osir-muted">
                        ID {group.id} · {status(group.status)}
                      </small>
                    </td>
                    <td>
                      <PlatformBadge group={group} />
                    </td>
                    <td>
                      <div className="osir-group-badges">
                        <BillingBadge group={group} />
                        {group.is_exclusive === true && (
                          <span className="osir-account-badge">专属分组</span>
                        )}
                      </div>
                    </td>
                    <td>
                      <div className="osir-group-rate">
                        <span>
                          基础 <strong>{show(group.rate_multiplier)}</strong>
                          {group.rate_multiplier != null ? "×" : ""}
                        </span>
                        <span
                          className={
                            rateData?.[String(group.id)] != null
                              ? "osir-group-custom-rate"
                              : ""
                          }
                        >
                          专属{" "}
                          <strong>{show(rateData?.[String(group.id)])}</strong>
                          {rateData?.[String(group.id)] != null ? "×" : ""}
                        </span>
                      </div>
                      <details>
                        <summary>计费详情</summary>
                        <p>
                          日 / 周 / 月上限：{summaryUsd(group.daily_limit_usd)}{" "}
                          / {summaryUsd(group.weekly_limit_usd)} /{" "}
                          {summaryUsd(group.monthly_limit_usd)}
                        </p>
                        <p>
                          图片倍率：{show(group.image_rate_multiplier)} ·
                          视频倍率：
                          {show(group.video_rate_multiplier)}
                        </p>
                        <p>
                          图片 1K / 2K / 4K：{usd(group.image_price_1k)} /{" "}
                          {usd(group.image_price_2k)} /{" "}
                          {usd(group.image_price_4k)}
                        </p>
                        {group.peak_rate_enabled && (
                          <p>
                            高峰 {show(group.peak_start)}–{show(group.peak_end)}{" "}
                            · 倍率 {show(group.peak_rate_multiplier)}
                          </p>
                        )}
                      </details>
                    </td>
                    <td className="max-w-48 break-words">
                      {show(group.description)}
                    </td>
                  </tr>
                ))}
              {rows?.length === 0 && (
                <tr>
                  <td colSpan={columns[page].length}>
                    <p className="osir-empty">
                      {page === "keys"
                        ? "没有符合筛选条件的密钥"
                        : page === "groups"
                          ? groupPlatform
                            ? "该平台暂无可用分组"
                            : "暂无可用分组"
                          : "暂无订阅"}
                    </p>
                  </td>
                </tr>
              )}
            </Table>
          )}
        </div>
      </OsirReveal>
      {page === "keys" && (
        <Pager
          data={keyData}
          requestedPage={pageNumber}
          busy={keys.isFetching}
          onPage={setPageNumber}
        />
      )}
      <Updated
        reply={query.data?.generation === generation ? query.data : undefined}
      />
      {page === "groups" && rateData === null && (
        <p className="osir-muted">账户专属倍率暂不可用。</p>
      )}
      {page === "subscriptions" && (
        <p className="osir-muted">
          额度重置时间暂不可用；未提供的额度显示为 —。
        </p>
      )}
    </section>
  );
}
function UsageResources({
  generation,
  initialGroup,
}: {
  generation: number;
  initialGroup?: number;
}) {
  const exportLock = useRef(false);
  const [exporting, setExporting] = useState(false);
  const [exportMessage, setExportMessage] = useState("");
  const [exportFailed, setExportFailed] = useState(false);
  const [filters, setFilters] = useState<Params>({
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    ...(initialGroup == null ? {} : { group_id: initialGroup }),
  });
  const [page, setPage] = useState(1);
  const [chart, setChart] = useState("");
  const [granularity, setGranularity] = useState("day");
  const refreshes = useRef(new Set<string>());
  const [refreshResult, setRefreshResult] = useState<{
    selection: string;
    status: "refreshing" | "success" | "error";
  } | null>(null);
  const { page_size = 20, sort_by, sort_order, ...shared } = filters;
  // List defaults to today as well as the summary. Explicit dates override it.
  const today = todayIn(String(shared.timezone || "UTC"));
  const range: Params = {
    ...shared,
    start_date: shared.start_date ?? today,
    end_date: shared.end_date ?? today,
  };
  const listParameters = {
    ...range,
    page,
    page_size,
    ...(sort_by ? { sort_by } : {}),
    ...(sort_order ? { sort_order } : {}),
  };
  const list = useAccountResource<Paged<Usage>>("usage", listParameters);
  const stats = useAccountResource<Stats>("usage_stats", range);
  const snapshot = useAccountResource<Snapshot>(
    "usage_snapshot",
    {
      ...range,
      granularity,
      include_trend: chart === "trend",
      include_model_stats: chart === "models",
      include_group_stats: chart === "groups",
    },
    !!chart,
  );
  const data = current(list.data, generation);
  const summary = current(stats.data, generation);
  const charts = current(snapshot.data, generation);
  const selection = JSON.stringify([listParameters, chart, granularity]);
  const activeSelection = useRef(selection);
  activeSelection.current = selection;
  const refreshStatus =
    refreshResult?.selection === selection ? refreshResult.status : null;
  async function refreshUsage() {
    if (refreshes.current.has(selection)) return;
    refreshes.current.add(selection);
    setRefreshResult({ selection, status: "refreshing" });
    const results = await Promise.allSettled([
      list.refetch({ throwOnError: true }),
      stats.refetch({ throwOnError: true }),
      ...(chart ? [snapshot.refetch({ throwOnError: true })] : []),
    ]);
    refreshes.current.delete(selection);
    if (activeSelection.current === selection)
      setRefreshResult({
        selection,
        status: results.every((result) => result.status === "fulfilled")
          ? "success"
          : "error",
      });
  }
  async function exportPage() {
    if (exportLock.current || list.isFetching || !data?.items.length) return;
    exportLock.current = true;
    setExporting(true);
    setExportMessage("");
    setExportFailed(false);
    try {
      const count = await invoke<number | null>("osir_export_usage_page", {
        parameters: listParameters,
        generation,
      });
      if (count !== null)
        toast.success("已导出当前页 " + count + " 条记录；不包含其他分页。");
    } catch {
      setExportFailed(true);
      setExportMessage("导出未完成。请选择可写目录和未使用的文件名后重试。");
    } finally {
      exportLock.current = false;
      setExporting(false);
    }
  }
  return (
    <section
      className="osir-account-pages osir-account-usage"
      aria-label="服务端用量"
    >
      <Filter
        page="usage"
        initialGroup={initialGroup}
        generation={generation}
        onApply={(params) => {
          setFilters(params);
          setPage(1);
        }}
      />
      <p className="osir-usage-range-caption osir-muted">
        {String(range.start_date)} 至 {String(range.end_date)} ·{" "}
        {String(range.timezone)}
      </p>
      <QueryMessage query={stats} hasData={summary !== undefined} />
      <UsageStatsCards stats={summary} />
      {summary && (
        <details>
          <summary>汇总计费与 Token 明细</summary>
          <p>
            标准费用 {usageMoney(summary.total_cost)} · 平均耗时{" "}
            {usageDuration(summary.average_duration_ms)}
          </p>
          <p>
            输入 {show(summary.total_input_tokens)} · 输出{" "}
            {show(summary.total_output_tokens)} · 缓存合计{" "}
            {show(summary.total_cache_tokens)} · 缓存写入{" "}
            {show(summary.total_cache_creation_tokens)} · 缓存读取{" "}
            {show(summary.total_cache_read_tokens)}
          </p>
          {summary.endpoints?.map((item, i) => (
            <p key={i}>
              {show(item.endpoint)}：请求 {show(item.requests)} · Token{" "}
              {show(item.total_tokens)} · 标准 {usageMoney(item.cost)} · 实际{" "}
              {usageMoney(item.actual_cost)}
            </p>
          ))}
        </details>
      )}
      <Updated
        reply={stats.data?.generation === generation ? stats.data : undefined}
      />
      <div className="osir-resource-toolbar">
        <span>服务端账单记录 · 各计费来源逐条标注</span>
        <div className="osir-resource-actions">
          <button
            type="button"
            disabled={exporting || list.isFetching || !data?.items.length}
            onClick={() => void exportPage()}
          >
            <Download size={15} aria-hidden="true" />
            {exporting ? "正在导出…" : "导出当前页"}
          </button>
          <button
            type="button"
            aria-busy={refreshStatus === "refreshing"}
            disabled={
              refreshStatus === "refreshing" ||
              list.isFetching ||
              stats.isFetching ||
              (!!chart && snapshot.isFetching)
            }
            onClick={() => void refreshUsage()}
          >
            <RefreshCw size={15} aria-hidden="true" />
            {refreshStatus === "refreshing" ? "正在刷新用量…" : "刷新用量"}
          </button>
        </div>
      </div>
      {refreshStatus === "success" && <p role="status">用量已刷新。</p>}
      {refreshStatus === "error" && (
        <p role="alert">
          {data || summary || charts
            ? "刷新未完成，已保留最近成功的数据。请重试。"
            : "刷新未完成，请重试。"}
        </p>
      )}
      {exportMessage && (
        <p role={exportFailed ? "alert" : "status"}>{exportMessage}</p>
      )}
      <QueryMessage query={list} hasData={data !== undefined} />
      <Table labels={columns.usage} className="osir-usage-records-table">
        {data?.items.map((item) => (
          <tr key={item.id}>
            <td>
              <time>{date(item.created_at)}</time>
              <UsageDetail item={item} />
            </td>
            <td>
              <UsageModel item={item} />
            </td>
            <td>
              <UsageBadges item={item} />
            </td>
            <td>
              <UsageTokens item={item} />
            </td>
            <td>
              <UsageCost item={item} />
            </td>
            <td>
              <UsageLatency item={item} />
            </td>
          </tr>
        ))}
        {data?.items.length === 0 && (
          <tr>
            <td colSpan={columns.usage.length}>
              <p className="osir-empty">所选范围暂无用量记录</p>
            </td>
          </tr>
        )}
      </Table>
      <Pager
        data={data}
        requestedPage={page}
        busy={list.isFetching}
        onPage={setPage}
      />
      <Updated
        reply={list.data?.generation === generation ? list.data : undefined}
      />
      <div className="my-4 flex flex-wrap gap-3">
        <Select
          label="用量图表"
          name="chart"
          options={[
            ["trend", "时间趋势"],
            ["models", "模型汇总"],
            ["groups", "分组汇总"],
          ]}
          onChange={setChart}
        />
        {chart === "trend" && (
          <Select
            label="统计粒度"
            name="granularity"
            blank={false}
            options={[
              ["day", "按日"],
              ["hour", "按小时"],
            ]}
            onChange={setGranularity}
          />
        )}
      </div>
      {chart && (
        <>
          <QueryMessage query={snapshot} hasData={charts !== undefined} />
          {charts && (
            <>
              <p className="osir-muted">
                {show(charts.start_date)} 至 {show(charts.end_date)} · 生成于{" "}
                {date(charts.generated_at)}
              </p>
              <UsageBreakdown
                rows={charts[chart as "trend" | "models" | "groups"]}
                kind={chart}
              />
              <Table
                labels={[
                  chart === "trend"
                    ? "时间"
                    : chart === "models"
                      ? "模型"
                      : "分组",
                  "请求数",
                  "Token",
                  "标准费用（USD）",
                  "实际费用（USD）",
                ]}
              >
                {charts[chart as "trend" | "models" | "groups"]?.map(
                  (item, i) => (
                    <tr key={i}>
                      <td>
                        {show(
                          chart === "trend"
                            ? item.date
                            : chart === "models"
                              ? item.model
                              : (item.group_name ?? item.group_id),
                        )}
                      </td>
                      <td>{show(item.requests)}</td>
                      <td>
                        {show(item.total_tokens)}
                        <small className="block">
                          输入 {show(item.input_tokens)} / 输出{" "}
                          {show(item.output_tokens)}
                          <br />
                          缓存写 {show(item.cache_creation_tokens)} / 读{" "}
                          {show(item.cache_read_tokens)}
                        </small>
                      </td>
                      <td>{usageMoney(item.cost)}</td>
                      <td>{usageMoney(item.actual_cost)}</td>
                    </tr>
                  ),
                )}
                {charts[chart as "trend" | "models" | "groups"]?.length ===
                  0 && (
                  <tr>
                    <td colSpan={5}>暂无图表数据</td>
                  </tr>
                )}
              </Table>
              {charts[chart as "trend" | "models" | "groups"] == null && (
                <p role="status">当前图表数据不可用。</p>
              )}
            </>
          )}
        </>
      )}
      <p className="osir-muted">
        订阅额度消耗与余额扣费按记录分别展示，不从汇总总额推算拆分金额。
      </p>
    </section>
  );
}
function UsageDetail({ item }: { item: Usage }) {
  return (
    <ResourceDrawer
      title="用量详情"
      description="此请求的计费与用量记录，以 OSIR 服务端账单为准。"
      trigger={
        <button type="button" className="osir-usage-detail-trigger">
          用量详情
        </button>
      }
    >
      <div className="osir-request-summary">
        <h3>{show(item.model)}</h3>
        <UsageBadges item={item} />
        <p>{date(item.created_at)}</p>
        <p>
          {usageGroupName(item)} · {usageKeyName(item)}
        </p>
      </div>
      <section className="osir-detail-section" aria-label="请求费用">
        <h4>请求费用</h4>
        <dl className="osir-detail-values">
          <dt>
            {item.billing_type === 1
              ? "订阅额度消耗"
              : item.billing_type === 0
                ? "余额扣费"
                : "实际费用"}
          </dt>
          <dd>{usageMoney(item.actual_cost)}</dd>
          <dt>标准费用</dt>
          <dd>{usageMoney(item.total_cost)}</dd>
          <dt>计费模式</dt>
          <dd>{usageBillingMode(item)}</dd>
          <dt>倍率</dt>
          <dd>{show(item.rate_multiplier)}</dd>
        </dl>
      </section>
      <section className="osir-detail-section" aria-label="请求耗时">
        <h4>请求耗时</h4>
        <UsageLatency item={item} />
      </section>
      <details className="osir-detail-section">
        <summary>更多请求明细</summary>
        <section aria-label="Token 与缓存">
          <h4>Token 与缓存</h4>
          <dl className="osir-detail-values">
            <dt>输入 Token</dt>
            <dd>{show(item.input_tokens)}</dd>
            <dt>输出 Token</dt>
            <dd>{show(item.output_tokens)}</dd>
          </dl>
          <p>
            缓存写入 {show(item.cache_creation_tokens)} · 5m{" "}
            {show(item.cache_creation_5m_tokens)} · 1h{" "}
            {show(item.cache_creation_1h_tokens)} · 缓存读取{" "}
            {show(item.cache_read_tokens)}
          </p>
          <p>
            费用：输入 {usageMoney(item.input_cost)} · 输出{" "}
            {usageMoney(item.output_cost)} · 缓存写{" "}
            {usageMoney(item.cache_creation_cost)} · 缓存读{" "}
            {usageMoney(item.cache_read_cost)} · 倍率{" "}
            {show(item.rate_multiplier)}
          </p>
        </section>
        <section className="osir-detail-section" aria-label="请求信息">
          <h4>请求信息</h4>
          <dl className="osir-detail-values">
            <dt>请求类型</dt>
            <dd>
              {{
                sync: "非流式",
                stream: "流式",
                ws_v2: "WebSocket",
                cyber: "Cyber",
                unknown: "类型未知",
              }[usageRequest(item)] ?? usageRequest(item)}
            </dd>
          </dl>
          <p>
            请求 ID {show(item.request_id)} · 订阅 ID{" "}
            {show(item.subscription_id)} · 缓存 TTL 覆盖{" "}
            {item.cache_ttl_overridden == null
              ? "—"
              : item.cache_ttl_overridden
                ? "是"
                : "否"}
          </p>
        </section>
        <section className="osir-detail-section" aria-label="图片与媒体">
          <h4>图片与媒体</h4>
          <p>图片数量 {show(item.image_count)}</p>
          <p>
            图片尺寸 {show(item.image_size)} · 输入{" "}
            {show(item.image_input_size)} · 输出 {show(item.image_output_size)}{" "}
            · 来源 {show(item.image_size_source)}
          </p>
          <p>
            图片输出 Token {show(item.image_output_tokens)} · 图片费用{" "}
            {usageMoney(item.image_output_cost)}
          </p>
          <p>
            图片计费分布：
            {item.image_size_breakdown == null
              ? "—"
              : ["1K", "2K", "4K"]
                  .filter((k) => item.image_size_breakdown?.[k] != null)
                  .map((k) => k + ": " + item.image_size_breakdown![k])
                  .join(" / ")}
          </p>
          <p>
            媒体 {show(item.media_type)} · 服务档位{" "}
            {usageTier(item.service_tier) ?? "未提供"} · 推理{" "}
            {usageReasoning(item.reasoning_effort) ?? "未提供"}
          </p>
        </section>
      </details>
    </ResourceDrawer>
  );
}
