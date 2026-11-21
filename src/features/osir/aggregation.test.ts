import { beforeEach, describe, expect, it, vi } from "vitest";
import { proxyApi } from "@/lib/api";
import type { Provider } from "@/types";
import type { AppModeView, ProxyStack } from "@/types/proxy";
import { applyAggregation, hasActiveAggregationMembers } from "./aggregation";

vi.mock("@/lib/api", () => ({
  proxyApi: {
    getAppMode: vi.fn(),
    getProxyStack: vi.fn(),
    setProxyStackMember: vi.fn(),
    setProxyTakeoverForApp: vi.fn(),
  },
}));

const provider: Provider = {
  id: "route",
  name: "Test route",
  settingsConfig: { modelCatalog: { models: [{ model: "gpt-test" }] } },
};
const attached: AppModeView = {
  mode: "stack",
  attached: true,
  routeProviderId: provider.id,
  directProviderId: null,
};
const stack: ProxyStack = {
  active: true,
  members: [
    {
      providerId: provider.id,
      route: true,
      modelIds: [],
      memberModels: 1,
    },
  ],
};

describe("Codex aggregation confirmation", () => {
  beforeEach(() => vi.resetAllMocks());

  it("requires the requested members and an available catalog", () => {
    expect(hasActiveAggregationMembers(stack, [provider])).toBe(true);
    expect(
      hasActiveAggregationMembers({ ...stack, active: false }, [provider]),
    ).toBe(false);
    expect(
      hasActiveAggregationMembers({ ...stack, members: [] }, [provider]),
    ).toBe(false);
    expect(
      hasActiveAggregationMembers(stack, [
        provider,
        { ...provider, id: "missing" },
      ]),
    ).toBe(false);
    expect(
      hasActiveAggregationMembers({ ...stack, notice: "routeOwnsCatalog" }, [
        provider,
      ]),
    ).toBe(false);
    expect(
      hasActiveAggregationMembers(
        { ...stack, notice: "officialModelsBundled" },
        [provider],
      ),
    ).toBe(true);
  });

  it("reattaches a saved but detached stack and verifies the readback", async () => {
    vi.mocked(proxyApi.getAppMode)
      .mockResolvedValueOnce({ ...attached, attached: false })
      .mockResolvedValueOnce(attached);
    vi.mocked(proxyApi.getProxyStack).mockResolvedValue(stack);

    await expect(applyAggregation([provider])).resolves.toBe(true);
    expect(proxyApi.setProxyStackMember).toHaveBeenCalledWith(
      "codex",
      provider.id,
      true,
    );
    expect(proxyApi.setProxyTakeoverForApp).toHaveBeenCalledWith(
      "codex",
      true,
      true,
      provider.id,
    );
  });

  it("does not report success when an attached stack omits the saved supplier", async () => {
    vi.mocked(proxyApi.getAppMode).mockResolvedValue(attached);
    vi.mocked(proxyApi.getProxyStack).mockResolvedValue({
      ...stack,
      members: [],
    });

    await expect(applyAggregation([provider])).rejects.toThrow("尚未确认");
    expect(proxyApi.setProxyTakeoverForApp).not.toHaveBeenCalled();
  });
});
