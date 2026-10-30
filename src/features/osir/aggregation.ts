import { proxyApi } from "@/lib/api";
import type { Provider } from "@/types";
import { modelNames, simpleEditable } from "./providerConfig";

const preferenceKey = "osir.codex.aggregation.enabled";
export function aggregationIntent(): boolean {
  try {
    return localStorage.getItem(preferenceKey) !== "false";
  } catch {
    return true;
  }
}
export function saveAggregationIntent(enabled: boolean) {
  try {
    localStorage.setItem(preferenceKey, String(enabled));
  } catch {
    throw new Error("聚合偏好保存失败，请重试。");
  }
}
export function aggregationCandidates(providers: Provider[]) {
  return providers.filter(
    (p) =>
      modelNames(p, "codex").length > 0 &&
      (!!p.meta?.osirBinding || simpleEditable(p, "codex")),
  );
}

// Only call after an explicit save or toggle. Native operations own their
// recovery journal; do not overwrite it with speculative compensating writes.
export async function applyAggregation(providers: Provider[]) {
  const candidates = aggregationCandidates(providers);
  if (!candidates.length) return false;
  const before = await proxyApi.getAppMode("codex");
  for (const provider of candidates) {
    await proxyApi.setProxyStackMember("codex", provider.id, true);
  }
  if (before.mode !== "stack") {
    // The native enable transaction commits the explicit default route too.
    await proxyApi.setProxyTakeoverForApp(
      "codex",
      true,
      true,
      candidates.find((provider) => provider.id === before.routeProviderId)
        ?.id ?? candidates[0].id,
    );
  }
  const after = await proxyApi.getAppMode("codex");
  if (after.mode !== "stack" || !after.attached) {
    throw new Error("聚合尚未确认生效，请刷新状态后重试。");
  }
  return true;
}

export async function syncSavedAggregation(provider: Provider) {
  if (!aggregationIntent()) return false;
  return applyAggregation([provider]);
}

export async function disableAggregation() {
  const before = await proxyApi.getAppMode("codex");
  if (before.mode !== "stack") return;
  await proxyApi.setProxyTakeoverForApp(
    "codex",
    true,
    false,
    before.routeProviderId,
  );
  const after = await proxyApi.getAppMode("codex");
  if (after.mode === "stack")
    throw new Error("聚合仍在生效，请刷新状态后重试关闭。");
  if (after.mode !== "route" || !after.attached)
    throw new Error("单供应商连接尚未确认生效，请刷新后重试。");
}
