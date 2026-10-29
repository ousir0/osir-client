import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { usageApi } from "@/lib/api/usage";

// Shared pricing, provider quota and reader summaries remain independent of reports.
export const usageKeys = {
  all: ["usage"] as const,
  pricing: () => [...usageKeys.all, "pricing"] as const,
  session: (appType: string, sessionId: string) =>
    [...usageKeys.all, "session", appType, sessionId] as const,
  limits: (providerId: string, appType: string) =>
    [...usageKeys.all, "limits", providerId, appType] as const,
  script: (providerId: string, appType: string) =>
    [...usageKeys.all, providerId, appType] as const,
};

const DEFAULT_REFETCH_INTERVAL_MS = 30000;

/** 单个会话的用量汇总（会话阅读页头部），只数会话日志导入的行。 */
export function useSessionUsageSummary(appType: string, sessionId: string) {
  return useQuery({
    queryKey: usageKeys.session(appType, sessionId),
    queryFn: () => usageApi.getSessionUsageSummary(appType, sessionId),
    refetchInterval: DEFAULT_REFETCH_INTERVAL_MS,
    refetchIntervalInBackground: false,
  });
}

export function useModelPricing() {
  return useQuery({
    queryKey: usageKeys.pricing(),
    queryFn: usageApi.getModelPricing,
  });
}

export function useProviderLimits(providerId: string, appType: string) {
  return useQuery({
    queryKey: usageKeys.limits(providerId, appType),
    queryFn: () => usageApi.checkProviderLimits(providerId, appType),
    enabled: !!providerId && !!appType,
  });
}

export function useUpdateModelPricing() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (params: {
      modelId: string;
      displayName: string;
      inputCost: string;
      outputCost: string;
      cacheReadCost: string;
      cacheCreationCost: string;
    }) =>
      usageApi.updateModelPricing(
        params.modelId,
        params.displayName,
        params.inputCost,
        params.outputCost,
        params.cacheReadCost,
        params.cacheCreationCost,
      ),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: usageKeys.all });
    },
  });
}

export function useDeleteModelPricing() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (modelId: string) => usageApi.deleteModelPricing(modelId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: usageKeys.all });
    },
  });
}
