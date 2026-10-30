import { useCallback, useEffect, useState } from "react";
import { useAccountResource, useAccountSession } from "./accountApi";
import {
  activityDate,
  validActivityTokens,
  type OverviewTokenActivityData,
  type OverviewTokenActivityFilters,
} from "./overviewTokenActivityData";

interface UsageActivitySnapshot {
  start_date?: string;
  end_date?: string;
  granularity?: string;
  trend?: { date: string; total_tokens?: number | null }[] | null;
  models?: { model: string; total_tokens?: number | null }[] | null;
}

interface BucketRange {
  date: string;
  endDate: string;
}
interface BucketFocus extends BucketRange {
  generation: number | undefined;
  model: string;
  timeZone: string;
}

function activityRange() {
  const timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(new Date());
  const part = (name: string) =>
    parts.find((item) => item.type === name)?.value;
  const endDate = `${part("year")}-${part("month")}-${part("day")}`;
  const start = new Date(`${endDate}T00:00:00Z`);
  // Calendar dates, independent of 23/25-hour daylight-saving transitions.
  start.setUTCDate(start.getUTCDate() - 363);
  return { startDate: start.toISOString().slice(0, 10), endDate, timeZone };
}

/** The server returns sparse daily buckets. Missing/invalid days are unknown. */
export function useOverviewTokenActivity(enabled = true) {
  const session = useAccountSession();
  const generation = session.data?.generation;
  const defaultFilters: OverviewTokenActivityFilters = {
    source: "all",
    model: "all",
    scale: "day",
  };
  const [selection, setSelection] = useState({
    generation,
    filters: defaultFilters,
  });
  const filters =
    selection.generation === generation ? selection.filters : defaultFilters;
  const { startDate, endDate, timeZone } = activityRange();
  const [focusedBucket, setFocusedBucket] = useState<BucketFocus | null>(null);
  const [settledBucket, setSettledBucket] = useState<BucketFocus | null>(null);
  const focus =
    enabled &&
    session.data?.connected &&
    focusedBucket?.generation === generation &&
    focusedBucket?.model === filters.model &&
    focusedBucket?.timeZone === timeZone
      ? focusedBucket
      : null;
  useEffect(() => {
    if (!focus) return;
    const timer = setTimeout(() => setSettledBucket(focus), 300);
    return () => clearTimeout(timer);
  }, [focus]);
  const onBucketFocus = useCallback(
    (bucket: BucketRange | null) => {
      if (
        !enabled ||
        !bucket ||
        activityDate(bucket.date) === null ||
        activityDate(bucket.endDate) === null ||
        bucket.date < startDate ||
        bucket.endDate > endDate ||
        bucket.date > bucket.endDate
      ) {
        setFocusedBucket(null);
        setSettledBucket(null);
        return;
      }
      setFocusedBucket((previous) =>
        previous?.date === bucket.date &&
        previous.endDate === bucket.endDate &&
        previous.generation === generation &&
        previous.model === filters.model &&
        previous.timeZone === timeZone
          ? previous
          : { ...bucket, generation, model: filters.model, timeZone },
      );
    },
    [enabled, startDate, endDate, generation, filters.model, timeZone],
  );
  const range = {
    start_date: startDate,
    end_date: endDate,
    timezone: timeZone,
    granularity: "day",
    include_trend: true,
    include_group_stats: false,
  };
  // Retain the unfiltered model catalog when a selected model has no activity.
  const all = useAccountResource<UsageActivitySnapshot>(
    "usage_snapshot",
    { ...range, include_model_stats: true },
    enabled,
  );
  const selected = useAccountResource<UsageActivitySnapshot>(
    "usage_snapshot",
    { ...range, include_model_stats: false, model: filters.model },
    enabled && filters.model !== "all",
  );
  const active = filters.model === "all" ? all : selected;
  // A single observer serves the stable focused bucket. The existing native
  // cache/single-flight and generation-aware query key apply to these reads too.
  const bucketQuery = useAccountResource<UsageActivitySnapshot>(
    "usage_snapshot",
    {
      start_date: settledBucket?.date ?? startDate,
      end_date: settledBucket?.endDate ?? endDate,
      timezone: settledBucket?.timeZone ?? timeZone,
      granularity: "day",
      include_trend: false,
      include_model_stats: true,
      include_group_stats: false,
      ...(settledBucket && settledBucket.model !== "all"
        ? { model: settledBucket.model }
        : {}),
    },
    !!focus && settledBucket === focus,
  );
  const bucketSnapshot =
    focus &&
    settledBucket === focus &&
    bucketQuery.data &&
    bucketQuery.data.generation === generation
      ? bucketQuery.data.data
      : undefined;
  const validBucket =
    !!bucketSnapshot &&
    bucketSnapshot.start_date === focus?.date &&
    bucketSnapshot.end_date === focus?.endDate &&
    Array.isArray(bucketSnapshot.models);
  const current =
    active.data?.generation === generation ? active.data : undefined;
  const baseline =
    all.data && all.data.generation === generation ? all.data.data : undefined;
  const models = [
    ...new Set((baseline?.models ?? []).map((item) => item.model)),
  ]
    .filter((model) => typeof model === "string" && model.length > 0)
    .map((model) => ({ id: model, label: model }));
  const snapshot = current?.data;
  const valid =
    snapshot?.granularity === "day" &&
    snapshot.start_date === startDate &&
    snapshot.end_date === endDate &&
    Array.isArray(snapshot.trend);
  const data: OverviewTokenActivityData | undefined = valid
    ? {
        startDate,
        endDate,
        timeZone,
        points: snapshot
          .trend!.filter((point) => activityDate(point.date) !== null)
          .map((point) => ({
            date: point.date,
            totalTokens: validActivityTokens(point.total_tokens)
              ? point.total_tokens
              : null,
          })),
        models,
        sources: [],
        sourceFilterAvailable: false,
        modelFilterAvailable: Array.isArray(baseline?.models),
      }
    : undefined;
  return {
    data,
    filters,
    onBucketFocus,
    bucketDetails:
      focus && validBucket
        ? {
            date: focus.date,
            endDate: focus.endDate,
            models: bucketSnapshot!
              .models!.filter(
                (item) =>
                  typeof item.model === "string" && item.model.length > 0,
              )
              .map((item) => ({
                model: item.model,
                totalTokens: validActivityTokens(item.total_tokens)
                  ? item.total_tokens
                  : null,
              })),
          }
        : undefined,
    bucketDetailsLoading:
      !!focus && (settledBucket !== focus || bucketQuery.isFetching),
    bucketDetailsError:
      !!focus &&
      settledBucket === focus &&
      (bucketQuery.isError || (!!bucketSnapshot && !validBucket)),
    onFiltersChange: (next: OverviewTokenActivityFilters) => {
      if (!["day", "week", "month"].includes(next.scale)) return;
      if (
        next.model !== "all" &&
        !models.some((model) => model.id === next.model)
      )
        return;
      // Usage has no trusted client/tool/provider dimension. Do not infer one.
      setFocusedBucket(null);
      setSettledBucket(null);
      setSelection({ generation, filters: { ...next, source: "all" } });
    },
    loading: enabled && active.isFetching,
    error:
      enabled && (active.isError || (!!current && !valid))
        ? data
          ? "Token 活动暂时无法更新，已保留最近成功的数据。请重试。"
          : "Token 活动暂时不可用，请重试。"
        : undefined,
    onRetry: () => {
      if (!enabled || active.isFetching) return;
      void active.refetch();
    },
  };
}
