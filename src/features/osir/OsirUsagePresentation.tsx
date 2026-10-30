import { useState, type ReactNode } from "react";
import {
  Activity,
  ArrowDown,
  ArrowUp,
  Box,
  Brain,
  Clock3,
  Database,
  FileText,
  Gauge,
  Image,
  KeyRound,
  Layers,
  Receipt,
  Sparkles,
  Wallet,
  Zap,
} from "lucide-react";
import "./usage-presentation.css";

export type UsageRecord = {
  id: number;
  created_at?: string;
  model?: string;
  group_id?: number | null;
  api_key_id?: number;
  group?: { id: number; name?: string; platform?: string } | null;
  api_key?: { id: number; name?: string } | null;
  billing_type?: number;
  billing_mode?: string | null;
  request_type?: string;
  stream?: boolean;
  openai_ws_mode?: boolean;
  request_id?: string;
  subscription_id?: number | null;
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
export type UsageStats = {
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
export type UsageAggregate = {
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
const valid = (value: number | null | undefined): value is number =>
  typeof value === "number" && Number.isFinite(value);
export function usageNumber(value?: number | null, compact = false) {
  if (!valid(value)) return "—";
  if (compact && Math.abs(value) >= 1000) {
    const scale =
      Math.abs(value) >= 1e9 ? 1e9 : Math.abs(value) >= 1e6 ? 1e6 : 1e3;
    return (
      (value / scale).toFixed(2) +
      (scale === 1e9 ? "B" : scale === 1e6 ? "M" : "K")
    );
  }
  return value.toLocaleString("en-US", { maximumFractionDigits: 2 });
}
export function usageMoney(value?: number | null) {
  return valid(value) ? "$" + value.toFixed(6) : "—";
}
function usageSummaryMoney(value?: number | null) {
  return valid(value) ? "$" + value.toFixed(2) : "—";
}
export function usageDuration(value?: number | null) {
  return valid(value) && value >= 0
    ? value < 1000
      ? Math.round(value) + " ms"
      : (value / 1000).toFixed(2) + " s"
    : "—";
}
export function usageKeyName(item: UsageRecord) {
  return item.api_key_id != null &&
    item.api_key?.id === item.api_key_id &&
    item.api_key.name
    ? item.api_key.name
    : item.api_key_id == null
      ? "密钥未提供"
      : "密钥 #" + item.api_key_id;
}
export function usageGroupName(item: UsageRecord) {
  return item.group_id != null &&
    item.group?.id === item.group_id &&
    item.group.name
    ? item.group.name
    : item.group_id == null
      ? "分组未提供"
      : "分组 #" + item.group_id;
}
export function usageRequest(item: UsageRecord) {
  const type = item.request_type;
  if (["unknown", "sync", "stream", "ws_v2", "cyber"].includes(type ?? ""))
    return type!;
  if (type) return type;
  if (item.openai_ws_mode) return "ws_v2";
  return item.stream === true
    ? "stream"
    : item.stream === false
      ? "sync"
      : "unknown";
}
export function usageBillingMode(item: UsageRecord) {
  const mode =
    item.billing_mode || ((item.image_count ?? 0) > 0 ? "image" : undefined);
  return mode
    ? ({
        token: "Token 计费",
        per_request: "按次计费",
        image: "图片计费",
        video: "视频计费",
      }[mode] ?? mode)
    : "计费模式未提供";
}
export const usageBillingSource = (item: UsageRecord) =>
  item.billing_type === 0
    ? "余额扣费"
    : item.billing_type === 1
      ? "订阅消耗"
      : "计费来源未提供";
export function usageTier(value?: string | null) {
  if (!value?.trim()) return null;
  const tier = value.trim().toLowerCase();
  return (
    {
      fast: "优先",
      priority: "优先",
      default: "标准",
      standard: "标准",
      flex: "弹性",
    }[tier] ?? value
  );
}
export function usageReasoning(value?: string | null) {
  return value
    ? ({
        none: "无",
        minimal: "极低",
        low: "低",
        medium: "中",
        high: "高",
        xhigh: "极高",
      }[value] ?? value)
    : null;
}
function Badge({
  tone = "slate",
  icon,
  children,
}: {
  tone?: string;
  icon?: ReactNode;
  children: ReactNode;
}) {
  return (
    <span className={"osir-usage-badge osir-usage-tone-" + tone}>
      {icon}
      {children}
    </span>
  );
}
export function UsageBadges({
  item,
  source = true,
}: {
  item: UsageRecord;
  source?: boolean;
}) {
  const request = usageRequest(item);
  const requestLabel =
    {
      sync: "非流式",
      stream: "流式",
      ws_v2: "WebSocket",
      cyber: "Cyber",
      unknown: "类型未知",
    }[request] ?? request;
  return (
    <div className="osir-usage-badges">
      {source && (
        <Badge
          tone={
            item.billing_type === 0
              ? "green"
              : item.billing_type === 1
                ? "violet"
                : "slate"
          }
          icon={item.billing_type === 1 ? <Layers /> : <Wallet />}
        >
          {usageBillingSource(item)}
        </Badge>
      )}
      <Badge
        tone={request === "stream" || request === "ws_v2" ? "blue" : "slate"}
        icon={<Activity />}
      >
        {requestLabel}
      </Badge>
      <Badge
        tone={
          item.billing_mode === "image" ||
          (!item.billing_mode && (item.image_count ?? 0) > 0)
            ? "pink"
            : item.billing_mode === "per_request"
              ? "violet"
              : item.billing_mode === "video"
                ? "amber"
                : item.billing_mode === "token"
                  ? "blue"
                  : "slate"
        }
      >
        {usageBillingMode(item)}
      </Badge>
    </div>
  );
}
export function UsageModel({ item }: { item: UsageRecord }) {
  const tier = usageTier(item.service_tier);
  const reasoning = usageReasoning(item.reasoning_effort);
  return (
    <div className="osir-usage-model">
      <strong>{item.model || "模型未提供"}</strong>
      <div className="osir-usage-badges">
        {tier && (
          <Badge tone="cyan" icon={<Zap />}>
            {tier}档位
          </Badge>
        )}
        {reasoning && <Badge icon={<Brain />}>推理：{reasoning}</Badge>}
      </div>
      <span className="osir-usage-key">
        <KeyRound aria-hidden="true" />
        {usageKeyName(item)}
      </span>
      <Badge
        tone={item.group_id == null ? "slate" : "violet"}
        icon={<Layers />}
      >
        {usageGroupName(item)}
      </Badge>
    </div>
  );
}
export function UsageTokens({ item }: { item: UsageRecord }) {
  const imageBilling =
    (item.image_count ?? 0) > 0 &&
    item.billing_mode !== "token" &&
    item.billing_mode !== "video";
  return (
    <div className="osir-usage-token-lines">
      {imageBilling ? (
        <span className="osir-usage-metric osir-usage-tone-pink">
          <Image aria-hidden="true" />
          图片 {usageNumber(item.image_count)} 张
          {item.image_size ? " · " + item.image_size : ""}
        </span>
      ) : (
        <>
          <span className="osir-usage-metric osir-usage-tone-green">
            <ArrowDown aria-hidden="true" />
            输入 <b>{usageNumber(item.input_tokens, true)}</b>
          </span>
          <span className="osir-usage-metric osir-usage-tone-violet">
            <ArrowUp aria-hidden="true" />
            输出 <b>{usageNumber(item.output_tokens, true)}</b>
          </span>
        </>
      )}
      {(item.cache_read_tokens ?? 0) > 0 && (
        <span className="osir-usage-metric osir-usage-tone-cyan">
          <Database aria-hidden="true" />
          缓存读 <b>{usageNumber(item.cache_read_tokens, true)}</b>
        </span>
      )}
      {(item.cache_creation_tokens ?? 0) > 0 && (
        <span className="osir-usage-metric osir-usage-tone-amber">
          <Database aria-hidden="true" />
          缓存写 <b>{usageNumber(item.cache_creation_tokens, true)}</b>
          {(item.cache_creation_1h_tokens ?? 0) > 0 && (
            <Badge tone="amber">1h</Badge>
          )}
        </span>
      )}
      {item.cache_ttl_overridden && <Badge tone="pink">缓存 TTL 已调整</Badge>}
      {(item.image_output_tokens ?? 0) > 0 && (
        <span className="osir-usage-metric osir-usage-tone-pink">
          <Image aria-hidden="true" />
          图像 Token <b>{usageNumber(item.image_output_tokens, true)}</b>
        </span>
      )}
    </div>
  );
}
export function UsageCost({ item }: { item: UsageRecord }) {
  return (
    <div className="osir-usage-cost">
      <strong>{usageMoney(item.actual_cost)}</strong>
      <span>{usageBillingSource(item)}</span>
      <small>标准 {usageMoney(item.total_cost)}</small>
      {valid(item.rate_multiplier) && (
        <Badge>倍率 {usageNumber(item.rate_multiplier)}×</Badge>
      )}
    </div>
  );
}
export function UsageLatency({ item }: { item: UsageRecord }) {
  return (
    <dl className="osir-usage-latency">
      <dt>
        <Zap aria-hidden="true" />
        首字
      </dt>
      <dd>{usageDuration(item.first_token_ms)}</dd>
      <dt>
        <Clock3 aria-hidden="true" />
        总耗时
      </dt>
      <dd>{usageDuration(item.duration_ms)}</dd>
    </dl>
  );
}
export function UsageStatsCards({ stats }: { stats?: UsageStats }) {
  const cards = [
    {
      label: "总请求数",
      value: usageNumber(stats?.total_requests),
      icon: <FileText />,
      tone: "blue",
    },
    {
      label: "总 Token",
      value: usageNumber(stats?.total_tokens, true),
      icon: <Box />,
      tone: "amber",
    },
    {
      label: "实际费用（USD）",
      value: usageSummaryMoney(stats?.total_actual_cost),
      icon: <Receipt />,
      tone: "green",
    },
    {
      label: "平均耗时",
      value: usageDuration(stats?.average_duration_ms),
      icon: <Clock3 />,
      tone: "violet",
    },
  ];
  return (
    <div className="osir-usage-stats">
      {cards.map((card) => (
        <article key={card.label} aria-label={card.label}>
          <div className="osir-usage-stat-heading">
            <span
              className={"osir-usage-stat-icon osir-usage-tone-" + card.tone}
              aria-hidden="true"
            >
              {card.icon}
            </span>
            <span>{card.label}</span>
          </div>
          <strong>{card.value}</strong>
        </article>
      ))}
    </div>
  );
}
export function UsageBreakdown({
  rows,
  kind,
}: {
  rows?: UsageAggregate[] | null;
  kind: string;
}) {
  const [metric, setMetric] = useState<"total_tokens" | "actual_cost">(
    "total_tokens",
  );
  const validRows =
    rows?.filter((row) => valid(row[metric]) && row[metric]! >= 0) ?? [];
  const total = validRows.reduce((sum, row) => sum + row[metric]!, 0);
  const max = Math.max(0, ...validRows.map((row) => row[metric]!));
  return (
    <section
      className="osir-usage-breakdown"
      aria-label={
        kind === "trend"
          ? "用量时间趋势"
          : kind === "models"
            ? "模型用量分布"
            : "分组用量分布"
      }
    >
      <header>
        <h3>
          <Gauge aria-hidden="true" />
          {kind === "trend"
            ? "用量时间趋势"
            : kind === "models"
              ? "模型用量分布"
              : "分组用量分布"}
        </h3>
        <div
          className="osir-usage-metric-switch"
          role="group"
          aria-label="分布统计指标"
        >
          <button
            type="button"
            aria-pressed={metric === "total_tokens"}
            onClick={() => setMetric("total_tokens")}
          >
            Token
          </button>
          <button
            type="button"
            aria-pressed={metric === "actual_cost"}
            onClick={() => setMetric("actual_cost")}
          >
            实际费用
          </button>
        </div>
      </header>
      {validRows.length > 0 ? (
        <ul>
          {validRows.map((row, i) => (
            <li key={i}>
              <span className="osir-usage-breakdown-name">
                {kind === "trend"
                  ? row.date
                  : kind === "models"
                    ? row.model
                    : (row.group_name ??
                      (row.group_id == null ? "—" : "分组 #" + row.group_id))}
              </span>
              <span className="osir-usage-bar-track" aria-hidden="true">
                <span
                  className={"osir-usage-bar osir-usage-bar-" + (i % 5)}
                  style={{
                    width: (max ? (row[metric]! / max) * 100 : 0) + "%",
                  }}
                />
              </span>
              <strong>
                {metric === "actual_cost"
                  ? usageSummaryMoney(row.actual_cost)
                  : usageNumber(row.total_tokens, true)}
              </strong>
              <small>
                {total ? ((row[metric]! / total) * 100).toFixed(1) : "0.0"}%
              </small>
            </li>
          ))}
        </ul>
      ) : (
        <p className="osir-muted">
          {rows?.length ? "此指标的数据未提供" : "所选范围暂无分布数据"}
        </p>
      )}
      <p className="osir-muted">
        <Sparkles size={13} aria-hidden="true" />
        仅展示服务端汇总，不按本机记录估算费用。
      </p>
    </section>
  );
}
