import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  ChartNoAxesCombined,
  Cpu,
  Filter,
  Layers,
  RefreshCw,
} from "lucide-react";
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  activityLevel,
  activityDate,
  buildActivityBuckets,
  type OverviewTokenActivityData,
  type OverviewTokenActivityFilters,
} from "./overviewTokenActivityData";
import "./overview-token-activity.css";
import { usageNumber } from "./OsirUsagePresentation";
import { OsirLoading } from "@/components/ui/osir-loading";

export interface OverviewTokenActivityProps {
  data?: OverviewTokenActivityData;
  filters: OverviewTokenActivityFilters;
  onFiltersChange: (filters: OverviewTokenActivityFilters) => void;
  loading?: boolean;
  error?: string | boolean | null;
  onRetry?: () => void;
  onViewUsage?: () => void;
  onBucketFocus?: (range: { date: string; endDate: string } | null) => void;
  bucketDetails?: {
    date: string;
    endDate: string;
    models: { model: string; totalTokens: number | null }[];
  };
  bucketDetailsLoading?: boolean;
  bucketDetailsError?: boolean;
}
const tokens = (value: number) => usageNumber(value, true);
const scaleName = { day: "日", week: "周", month: "月" } as const;

export function OverviewTokenActivity({
  data,
  filters,
  onFiltersChange,
  loading = false,
  error,
  onRetry,
  onViewUsage,
  onBucketFocus,
  bucketDetails,
  bucketDetailsLoading = false,
  bucketDetailsError = false,
}: OverviewTokenActivityProps) {
  const [filterOpen, setFilterOpen] = useState(false);
  const [activeCell, setActiveCell] = useState(0);
  const cells = useRef<(HTMLButtonElement | null)[]>([]);
  const hovered = useRef<{ date: string; endDate: string } | null>(null);
  const focused = useRef<{ date: string; endDate: string } | null>(null);
  const [activeBucket, setActiveBucket] = useState<string | null>(null);
  const [tooltipBucket, setTooltipBucket] = useState<string | null>(null);
  const [pinnedBucket, setPinnedBucket] = useState<string | null>(null);
  const pinned = useRef<{ date: string; endDate: string } | null>(null);
  const detailId = useId();
  const bucketFocus = useRef(onBucketFocus);
  bucketFocus.current = onBucketFocus;
  function dismissDetails() {
    pinned.current = null;
    hovered.current = null;
    focused.current = null;
    setPinnedBucket(null);
    setTooltipBucket(null);
    setActiveBucket(null);
    bucketFocus.current?.(null);
  }
  function togglePinned(range: { date: string; endDate: string }) {
    const key = `${range.date}/${range.endDate}`;
    if (
      pinned.current?.date === range.date &&
      pinned.current.endDate === range.endDate
    ) {
      dismissDetails();
      return;
    }
    pinned.current = range;
    setPinnedBucket(key);
    setTooltipBucket(null);
    setActiveBucket(key);
    bucketFocus.current?.(range);
  }
  function updateActiveBucket(preferPointer = false) {
    if (pinned.current) return;
    const range = preferPointer
      ? (hovered.current ?? focused.current)
      : (focused.current ?? hovered.current);
    setActiveBucket(range ? `${range.date}/${range.endDate}` : null);
    onBucketFocus?.(range);
  }
  const contextKey = `${filters.source}/${filters.model}/${filters.scale}/${data?.startDate}/${data?.endDate}/${data?.timeZone}`;
  const previousContext = useRef(contextKey);
  useEffect(() => {
    if (previousContext.current === contextKey) return;
    previousContext.current = contextKey;
    dismissDetails();
  }, [contextKey]);
  useEffect(
    () => () => {
      if (pinned.current || hovered.current || focused.current)
        bucketFocus.current?.(null);
    },
    [],
  );
  const buckets = useMemo(
    () => (data ? buildActivityBuckets(data, filters.scale) : []),
    [data, filters.scale],
  );
  const maximum = Math.max(
    0,
    ...buckets.map((bucket) => bucket.totalTokens ?? 0),
  );
  const selectedModel = data?.models?.find(
    (model) => model.id === filters.model,
  )?.label;
  const selectedSource = data?.sources?.find(
    (source) => source.id === filters.source,
  )?.label;
  const daily = filters.scale === "day";
  const firstDate = buckets[0] ? activityDate(buckets[0].date) : null;
  const firstWeekday =
    firstDate === null ? 0 : (new Date(firstDate).getUTCDay() + 6) % 7;
  const chartColumns = daily
    ? Math.max(26, Math.ceil((buckets.length + firstWeekday) / 7))
    : Math.max(1, buckets.length);
  const axisCandidates = buckets.flatMap((bucket, index) => {
    const parsed = activityDate(bucket.date);
    if (parsed === null) return [];
    const date = new Date(parsed);
    const month = date.getUTCMonth() + 1;
    const previous = index ? buckets[index - 1].date.slice(0, 7) : "";
    if (daily && previous === bucket.date.slice(0, 7)) return [];
    if (
      filters.scale === "week" &&
      index % Math.max(1, Math.ceil(buckets.length / 8)) !== 0
    )
      return [];
    return [
      {
        column: daily ? Math.floor((index + firstWeekday) / 7) + 1 : index + 1,
        label:
          filters.scale === "week"
            ? `${month}/${date.getUTCDate()}`
            : `${month}月`,
        title: bucket.date,
      },
    ];
  });
  const axisLabels = axisCandidates.reduce<typeof axisCandidates>(
    (labels, next) => {
      const previous = labels.at(-1);
      // A short date range can cross months within the same calendar week.
      if (previous?.column === next.column) {
        previous.label += ` / ${next.label}`;
        previous.title += ` — ${next.title}`;
      } else labels.push({ ...next });
      return labels;
    },
    [],
  );
  return (
    <section
      className="osir-token-activity"
      aria-label="Token 活跃度"
      aria-busy={loading || undefined}
    >
      <header>
        <div>
          <h2>Token 活跃度</h2>
          {data && (
            <span title="按服务端返回的日期展示">
              {data.startDate} — {data.endDate}
            </span>
          )}
        </div>
        <Popover open={filterOpen} onOpenChange={setFilterOpen}>
          <PopoverTrigger asChild>
            <button
              type="button"
              className="osir-token-icon-button"
              aria-label="筛选 Token 活跃度"
              title="筛选 Token 活跃度"
            >
              <Filter size={16} aria-hidden="true" />
            </button>
          </PopoverTrigger>
          <PopoverContent
            className="osir-token-filter"
            align="end"
            aria-label="Token 活跃度筛选"
          >
            <label>
              供应商 / 工具
              <select
                value={filters.source}
                disabled={!data?.sourceFilterAvailable}
                onChange={(event) =>
                  onFiltersChange({ ...filters, source: event.target.value })
                }
              >
                <option value="all">全部</option>
                {data?.sources?.map((source) => (
                  <option key={source.id} value={source.id}>
                    {source.label}
                  </option>
                ))}
              </select>
            </label>
            {!data?.sourceFilterAvailable && (
              <p>当前记录未提供供应商或工具来源。</p>
            )}
            <label>
              模型
              <select
                value={filters.model}
                disabled={!data?.modelFilterAvailable || loading}
                onChange={(event) =>
                  onFiltersChange({ ...filters, model: event.target.value })
                }
              >
                <option value="all">全部</option>
                {data?.models?.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.label}
                  </option>
                ))}
              </select>
            </label>
            <fieldset>
              <legend>时间尺度</legend>
              <div>
                {(["day", "week", "month"] as const).map((scale) => (
                  <button
                    key={scale}
                    type="button"
                    aria-pressed={filters.scale === scale}
                    onClick={() => onFiltersChange({ ...filters, scale })}
                  >
                    {scaleName[scale]}
                  </button>
                ))}
              </div>
            </fieldset>
          </PopoverContent>
        </Popover>
      </header>
      {error && (
        <div className="osir-token-error" role="alert">
          {buckets.length
            ? "用量更新失败，已保留最近可用数据。"
            : "用量暂不可用，请重试。"}
          {onRetry && (
            <button
              type="button"
              className="osir-token-icon-button"
              aria-label="重试 Token 活跃度"
              title="重试 Token 活跃度"
              disabled={loading}
              onClick={onRetry}
            >
              <RefreshCw size={15} aria-hidden="true" />
            </button>
          )}
        </div>
      )}
      {!buckets.length ? (
        !error &&
        (loading ? (
          <OsirLoading label="正在读取用量…" />
        ) : (
          <p className="osir-token-state">暂无可用的 Token 数据</p>
        ))
      ) : (
        <TooltipProvider delayDuration={100}>
          <div className="osir-token-chart" data-scale={filters.scale}>
            <span className="osir-token-axis-unit">
              {daily ? "星期" : scaleName[filters.scale]}
            </span>
            <div
              className="osir-token-axis-x"
              aria-label={filters.scale === "week" ? "日期（月/日）" : "月份"}
              style={{
                gridTemplateColumns: `repeat(${chartColumns}, minmax(0, 1fr))`,
              }}
            >
              {axisLabels.map((label) => (
                <span
                  key={label.title}
                  title={label.title}
                  style={{ gridColumn: label.column }}
                >
                  {label.label}
                </span>
              ))}
            </div>
            <div className="osir-token-axis-y" aria-hidden="true">
              {daily ? (
                ["一", "二", "三", "四", "五", "六", "日"].map((day) => (
                  <span key={day}>{day}</span>
                ))
              ) : (
                <span>{filters.scale === "week" ? "每周" : "每月"}</span>
              )}
            </div>
            <div
              className="osir-token-calendar"
              data-scale={filters.scale}
              role="group"
              aria-label={`按${scaleName[filters.scale]}显示 Token 用量`}
              style={{
                gridTemplateColumns: `repeat(${chartColumns}, minmax(0, 1fr))`,
                aspectRatio: daily ? `${chartColumns} / 7` : undefined,
              }}
            >
              {buckets.map((bucket, index) => {
                const range =
                  bucket.date === bucket.endDate
                    ? bucket.date
                    : `${bucket.date} — ${bucket.endDate}`;
                const value =
                  bucket.totalTokens === null
                    ? bucket.reportedDays === 0
                      ? "暂无用量记录"
                      : "数据未完整提供"
                    : `${tokens(bucket.totalTokens)} Token`;
                const key = `${bucket.date}/${bucket.endDate}`;
                const isPinned = pinnedBucket === key;
                const detail = (
                  <>
                    <strong>{range}</strong>
                    <span>{value}</span>
                    {bucket.totalTokens === null &&
                      bucket.recordedTokens !== null && (
                        <span>
                          已返回 {bucket.reportedDays}/{bucket.expectedDays} 天
                          · {tokens(bucket.recordedTokens)} Token
                        </span>
                      )}
                    {selectedSource && (
                      <span>
                        <Layers size={13} aria-hidden="true" />
                        {selectedSource}
                      </span>
                    )}
                    {selectedModel && (
                      <span>
                        <Cpu size={13} aria-hidden="true" />
                        {selectedModel}
                      </span>
                    )}
                    {activeBucket === `${bucket.date}/${bucket.endDate}` &&
                      bucketDetailsLoading && <span>正在读取模型明细…</span>}
                    {activeBucket === `${bucket.date}/${bucket.endDate}` &&
                      bucketDetailsError && <span>模型明细暂不可用</span>}
                    {bucketDetails?.date === bucket.date &&
                      bucketDetails.endDate === bucket.endDate && (
                        <div
                          className="osir-token-model-list"
                          role="region"
                          aria-label="模型 Token 明细"
                          tabIndex={isPinned ? 0 : undefined}
                        >
                          {bucketDetails.models.length ? (
                            bucketDetails.models.map((row) => (
                              <span key={row.model}>
                                <Cpu size={13} aria-hidden="true" />
                                {row.model} ·{" "}
                                {row.totalTokens === null
                                  ? "Token 数未提供"
                                  : `${tokens(row.totalTokens)} Token`}
                              </span>
                            ))
                          ) : (
                            <span>当前时段无模型明细</span>
                          )}
                        </div>
                      )}
                    {bucket.breakdown.map((row, rowIndex) => (
                      <span key={rowIndex}>
                        {row.sourceLabel && (
                          <>
                            <Layers size={13} aria-hidden="true" />
                            {row.sourceLabel}
                          </>
                        )}
                        {row.model && (
                          <>
                            <Cpu size={13} aria-hidden="true" />
                            {row.model}
                          </>
                        )}
                        {row.totalTokens === null
                          ? "—"
                          : tokens(row.totalTokens)}
                      </span>
                    ))}
                  </>
                );
                return (
                  <Tooltip
                    key={bucket.date}
                    open={!pinnedBucket && tooltipBucket === key}
                    onOpenChange={(open) => {
                      if (pinned.current) return;
                      setTooltipBucket((current) =>
                        open ? key : current === key ? null : current,
                      );
                    }}
                  >
                    <Popover
                      open={isPinned}
                      onOpenChange={(open) => {
                        if (
                          !open &&
                          pinned.current?.date === bucket.date &&
                          pinned.current.endDate === bucket.endDate
                        )
                          dismissDetails();
                      }}
                    >
                      <PopoverAnchor asChild>
                        <TooltipTrigger asChild>
                          <button
                            type="button"
                            ref={(element) => {
                              cells.current[index] = element;
                            }}
                            className="osir-token-cell"
                            style={
                              daily
                                ? {
                                    gridColumn:
                                      Math.floor((index + firstWeekday) / 7) +
                                      1,
                                    gridRow: ((index + firstWeekday) % 7) + 1,
                                  }
                                : undefined
                            }
                            data-level={activityLevel(
                              bucket.totalTokens,
                              maximum,
                            )}
                            aria-label={`${range}，${value}`}
                            aria-pressed={isPinned}
                            aria-haspopup="dialog"
                            aria-controls={isPinned ? detailId : undefined}
                            aria-expanded={isPinned}
                            onClick={() =>
                              togglePinned({
                                date: bucket.date,
                                endDate: bucket.endDate,
                              })
                            }
                            tabIndex={
                              index === Math.min(activeCell, buckets.length - 1)
                                ? 0
                                : -1
                            }
                            onPointerEnter={() => {
                              hovered.current = {
                                date: bucket.date,
                                endDate: bucket.endDate,
                              };
                              updateActiveBucket(true);
                            }}
                            onPointerLeave={() => {
                              hovered.current = null;
                              updateActiveBucket();
                            }}
                            onFocus={() => {
                              setActiveCell(index);
                              focused.current = {
                                date: bucket.date,
                                endDate: bucket.endDate,
                              };
                              updateActiveBucket();
                            }}
                            onBlur={() => {
                              focused.current = null;
                              updateActiveBucket();
                            }}
                            onKeyDown={(event) => {
                              if (event.key === "Enter" || event.key === " ") {
                                event.preventDefault();
                                togglePinned({
                                  date: bucket.date,
                                  endDate: bucket.endDate,
                                });
                                return;
                              }
                              if (event.key === "Escape") {
                                event.preventDefault();
                                dismissDetails();
                                event.currentTarget.blur();
                                return;
                              }
                              const columns = chartColumns;
                              const offset =
                                event.key === "ArrowRight"
                                  ? daily
                                    ? 7
                                    : 1
                                  : event.key === "ArrowLeft"
                                    ? daily
                                      ? -7
                                      : -1
                                    : event.key === "ArrowDown"
                                      ? daily
                                        ? 1
                                        : columns
                                      : event.key === "ArrowUp"
                                        ? daily
                                          ? -1
                                          : -columns
                                        : null;
                              const next =
                                event.key === "Home"
                                  ? 0
                                  : event.key === "End"
                                    ? buckets.length - 1
                                    : offset !== null
                                      ? Math.max(
                                          0,
                                          Math.min(
                                            buckets.length - 1,
                                            index + offset,
                                          ),
                                        )
                                      : null;
                              if (next !== null) {
                                event.preventDefault();
                                cells.current[next]?.focus();
                              }
                            }}
                          />
                        </TooltipTrigger>
                      </PopoverAnchor>
                      <TooltipContent className="osir-token-detail" side="top">
                        {detail}
                      </TooltipContent>
                      <PopoverContent
                        id={detailId}
                        className="osir-token-detail osir-token-detail-pinned"
                        side="top"
                        align="center"
                        collisionPadding={12}
                        aria-label={`${range} Token 用量明细`}
                        onOpenAutoFocus={(event) => event.preventDefault()}
                        onCloseAutoFocus={(event) => event.preventDefault()}
                        onFocusOutside={(event) => event.preventDefault()}
                        onPointerDownOutside={(event) => {
                          const target = event.detail.originalEvent.target;
                          if (
                            target instanceof Node &&
                            cells.current.some((cell) => cell?.contains(target))
                          )
                            event.preventDefault();
                        }}
                      >
                        {detail}
                      </PopoverContent>
                    </Popover>
                  </Tooltip>
                );
              })}
            </div>
          </div>
        </TooltipProvider>
      )}
      <footer>
        <div
          className="osir-token-legend"
          aria-label="用量由少到多；浅灰表示暂无记录或零用量"
        >
          <span>少</span>
          {["0", "1", "2", "3", "4"].map((level) => (
            <i key={level} data-level={level} aria-hidden="true" />
          ))}
          <span>多</span>
        </div>
        {onViewUsage && (
          <button
            type="button"
            className="osir-token-icon-button"
            aria-label="查看使用记录"
            title="查看使用记录"
            onClick={onViewUsage}
          >
            <ChartNoAxesCombined size={17} aria-hidden="true" />
          </button>
        )}
      </footer>
    </section>
  );
}
