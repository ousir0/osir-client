export type OverviewTokenActivityScale = "day" | "week" | "month";
export interface OverviewTokenActivityFilters {
  source: string;
  model: string;
  scale: OverviewTokenActivityScale;
}
export interface OverviewTokenActivityOption {
  id: string;
  label: string;
}
export interface OverviewTokenActivityBreakdown {
  sourceId?: string;
  sourceLabel?: string;
  model?: string;
  totalTokens: number | null;
}
export interface OverviewTokenActivityPoint {
  date: string;
  totalTokens?: number | null;
  breakdown?: OverviewTokenActivityBreakdown[];
}
export interface OverviewTokenActivityData {
  startDate: string;
  endDate: string;
  timeZone: string;
  points: OverviewTokenActivityPoint[];
  models?: OverviewTokenActivityOption[];
  sources?: OverviewTokenActivityOption[];
  sourceFilterAvailable?: boolean;
  modelFilterAvailable?: boolean;
}
export interface OverviewTokenActivityBucket {
  date: string;
  endDate: string;
  totalTokens: number | null;
  recordedTokens: number | null;
  reportedDays: number;
  expectedDays: number;
  breakdown: OverviewTokenActivityBreakdown[];
}
const DAY = 86_400_000;
const dateKey = (time: number) => new Date(time).toISOString().slice(0, 10);
export function activityDate(value: string) {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return null;
  const time = Date.parse(`${value}T00:00:00Z`);
  return Number.isFinite(time) && dateKey(time) === value ? time : null;
}
export function validActivityTokens(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

/** Missing days stay unknown. Only explicit API zeroes count as zero usage. */
export function buildActivityBuckets(
  data: OverviewTokenActivityData,
  scale: OverviewTokenActivityScale,
): OverviewTokenActivityBucket[] {
  const start = activityDate(data.startDate);
  const end = activityDate(data.endDate);
  if (start === null || end === null || end < start || end - start > DAY * 366)
    return [];
  const points = new Map<string, OverviewTokenActivityPoint | null>();
  for (const point of data.points) {
    const time = activityDate(point.date);
    if (time === null || time < start || time > end) continue;
    // Duplicate API dates are ambiguous; never sum or silently choose a row.
    points.set(point.date, points.has(point.date) ? null : point);
  }
  const buckets = new Map<string, OverviewTokenActivityBucket>();
  for (let time = start; time <= end; time += DAY) {
    const date = new Date(time);
    const key =
      scale === "month"
        ? dateKey(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), 1))
        : scale === "week"
          ? dateKey(time - ((date.getUTCDay() + 6) % 7) * DAY)
          : dateKey(time);
    const bucket = buckets.get(key) ?? {
      date: dateKey(time),
      endDate: dateKey(time),
      totalTokens: null,
      recordedTokens: null,
      reportedDays: 0,
      expectedDays: 0,
      breakdown: [],
    };
    bucket.endDate = dateKey(time);
    bucket.expectedDays += 1;
    const point = points.get(dateKey(time));
    if (validActivityTokens(point?.totalTokens)) {
      const sum = (bucket.recordedTokens ?? 0) + point.totalTokens;
      if (!Number.isSafeInteger(sum)) return [];
      bucket.recordedTokens = sum;
      bucket.reportedDays += 1;
    }
    for (const row of point?.breakdown ?? []) {
      if (
        !validActivityTokens(row.totalTokens) ||
        (!row.sourceLabel && !row.model)
      )
        continue;
      const existing = bucket.breakdown.find(
        (item) =>
          item.sourceId === row.sourceId &&
          item.sourceLabel === row.sourceLabel &&
          item.model === row.model,
      );
      if (existing) {
        const sum = (existing.totalTokens ?? 0) + row.totalTokens;
        existing.totalTokens = Number.isSafeInteger(sum) ? sum : null;
      } else bucket.breakdown.push({ ...row });
    }
    buckets.set(key, bucket);
  }
  return [...buckets.values()].map((bucket) => ({
    ...bucket,
    totalTokens:
      bucket.reportedDays === bucket.expectedDays
        ? bucket.recordedTokens
        : null,
  }));
}

export function activityLevel(tokens: number | null, maximum: number) {
  if (tokens === null) return "unknown";
  if (tokens === 0) return "0";
  return String(
    Math.max(1, Math.min(4, Math.ceil((tokens / Math.max(1, maximum)) * 4))),
  );
}
