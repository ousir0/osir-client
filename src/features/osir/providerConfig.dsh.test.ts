import { describe, expect, it } from "vitest";
import { buildConnection, readConnection, modelNames } from "./providerConfig";
const draft = {
  name: "Fixture",
  apiKey: "synthetic-key",
  baseUrl: "http://127.0.0.1:9/v1",
  model: "conversation",
  protocol: "openai_chat" as const,
};
describe("DeepSeek Harness connections", () => {
  it("uses native protocols and valid IDs, and keeps the chosen default model", () => {
    const row = buildConnection("dsh", draft);
    expect(row.id).toMatch(/^osir-[a-f0-9]+$/);
    expect(row.settingsConfig.api).toBe("openai-completions");
    expect(readConnection(row, "dsh").model).toBe("conversation");
    expect(modelNames(row, "dsh")).toEqual(["conversation"]);
  });
  it("preserves opaque saved credentials and existing models on manual edits", () => {
    const old = buildConnection("dsh", draft);
    delete old.settingsConfig.options.apiKey;
    old.settingsConfig._osirDshKey = "local-ref";
    const next = buildConnection(
      "dsh",
      { ...draft, apiKey: "", model: "second", protocol: "anthropic" },
      old,
    );
    expect(next.settingsConfig.options.apiKey).toBeUndefined();
    expect(next.settingsConfig._osirDshKey).toBe("local-ref");
    expect(next.settingsConfig.api).toBe("anthropic-messages");
    expect(modelNames(next, "dsh")).toEqual(["conversation", "second"]);
    expect(readConnection(next, "dsh").model).toBe("second");
  });
  it("refuses image generation defaults and unsupported protocols", () => {
    expect(() =>
      buildConnection("dsh", { ...draft, model: "gpt-image-2" }),
    ).toThrow("对话模型");
    expect(() =>
      buildConnection("dsh", { ...draft, protocol: "gemini_native" }),
    ).toThrow("协议");
  });
});
