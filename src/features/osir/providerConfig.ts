import { parse, stringify } from "smol-toml";
import type { Provider } from "@/types";
import {
  readClaudeContextLimit,
  readClaudeAutoCompactWindow,
  updateClaudeContextLimit,
  updateClaudeAutoCompactWindow,
} from "@/components/providers/forms/claudeContextLimit";
import {
  readGeminiContextManagementEnabled,
  readGeminiHistoryTokenLimit,
  updateGeminiContextManagementEnabled,
  updateGeminiHistoryTokenLimit,
} from "@/components/providers/forms/geminiContextLimit";

export const TOOL_IDS = ["codex", "claude", "opencode", "gemini"] as const;
export type ToolId = (typeof TOOL_IDS)[number];
export type ConnectionToolId = ToolId | "claude-desktop";
export const TOOL_NAMES: Record<ConnectionToolId, string> = {
  codex: "Codex",
  claude: "Claude Code",
  "claude-desktop": "Claude 桌面客户端",
  opencode: "OpenCode",
  gemini: "Gemini CLI",
};

/** Output-image models are separate from Codex conversation/image-input models. */
export function isCodexConversationModel(
  modelId: string,
  metadata?: unknown,
): boolean {
  const entry =
    metadata && typeof metadata === "object"
      ? (metadata as Record<string, unknown>)
      : undefined;
  const modalities =
    entry?.modalities && typeof entry.modalities === "object"
      ? (entry.modalities as Record<string, unknown>)
      : undefined;
  const output = entry?.outputModalities ?? modalities?.output;
  if (
    Array.isArray(output) &&
    output.some(
      (value) =>
        typeof value === "string" && value.trim().toLowerCase() === "image",
    )
  )
    return false;
  // The gateway currently returns only IDs. Match its image families rather
  // than dropping vision/text models whose names happen to contain "image".
  const id = modelId.trim().toLowerCase().split("/").pop()?.split(":")[0] ?? "";
  return !(
    id.startsWith("gpt-image-") ||
    /^dall-e-[23](?:$|-)/.test(id) ||
    id === "grok-imagine" ||
    id === "grok-imagine-edit" ||
    id.startsWith("grok-imagine-image") ||
    id === "gemini-2.5-flash-image" ||
    id === "gemini-3.1-flash-image"
  );
}

export interface ConnectionDraft {
  name: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  hideAggregationName?: boolean;
  protocol: "openai_responses" | "openai_chat" | "anthropic" | "gemini_native";
}

export interface ModelContextDraft {
  window: string;
  output: string;
}
export interface ConnectionContextDraft {
  window: string;
  compact: string;
  enabled: boolean;
  models: Record<string, ModelContextDraft>;
}

/** Reads only context metadata; managed credentials must never enter the form. */
export function readConnectionContext(
  provider: Provider | undefined,
  app: ConnectionToolId,
): ConnectionContextDraft {
  const result: ConnectionContextDraft = {
    window: "",
    compact: "",
    enabled: true,
    models: {},
  };
  if (!provider || app === "claude-desktop") return result;
  const c = provider.settingsConfig;
  const text = (value: unknown) =>
    typeof value === "number" || typeof value === "string" ? String(value) : "";
  if (app === "codex") {
    const config = parse(c.config || "");
    result.window = text(config.model_context_window);
    result.compact = text(config.model_auto_compact_token_limit);
    for (const row of c.modelCatalog?.models || []) {
      if (isCodexConversationModel(row.model, row))
        result.models[row.model] = {
          window: text(row.contextWindow ?? row.context_window),
          output: "",
        };
    }
  } else if (app === "opencode") {
    for (const [id, row] of Object.entries(c.models || {})) {
      const model = row as { limit?: { context?: unknown; output?: unknown } };
      result.models[id] = {
        window: text(model.limit?.context),
        output: text(model.limit?.output),
      };
    }
  } else if (app === "claude") {
    // Do not stringify env: it can contain renderer-inaccessible credentials.
    const context = JSON.stringify({
      env: {
        CLAUDE_CODE_MAX_CONTEXT_TOKENS: c.env?.CLAUDE_CODE_MAX_CONTEXT_TOKENS,
        CLAUDE_CODE_AUTO_COMPACT_WINDOW: c.env?.CLAUDE_CODE_AUTO_COMPACT_WINDOW,
      },
    });
    result.window = readClaudeContextLimit(context);
    result.compact = readClaudeAutoCompactWindow(context);
  } else {
    const config = JSON.stringify(c.config || {});
    result.window = readGeminiHistoryTokenLimit(config);
    result.enabled = readGeminiContextManagementEnabled(config);
  }
  return result;
}

/** Merge only changed context leaves into the latest row. No credentials,
 * routing, model selection or capability metadata are rebuilt here. */
export function applyConnectionContext(
  previous: Provider,
  app: ConnectionToolId,
  input: ConnectionContextDraft,
  base?: ConnectionContextDraft,
): Provider {
  if (app === "claude-desktop") return previous;
  const changed = (value: unknown, old: unknown) => !base || value !== old;
  const contextChanged =
    changed(input.window, base?.window) ||
    changed(input.compact, base?.compact) ||
    (app === "gemini" && changed(input.enabled, base?.enabled)) ||
    Object.entries(input.models).some(
      ([id, row]) =>
        changed(row.window, base?.models[id]?.window ?? "") ||
        changed(row.output, base?.models[id]?.output ?? ""),
    );
  if (!contextChanged) return previous;
  const current = base ? readConnectionContext(previous, app) : undefined;
  const check = (value: unknown, old: unknown, saved: unknown) => {
    if (base && value !== old && saved !== old && saved !== value)
      throw new Error("上下文设置已变化，请关闭抽屉并刷新后重试。");
  };
  check(input.window, base?.window, current?.window);
  check(input.compact, base?.compact, current?.compact);
  if (app === "gemini") check(input.enabled, base?.enabled, current?.enabled);
  const p = structuredClone(previous);
  const c = p.settingsConfig;
  const positive = (value: string) => {
    const raw = value.trim();
    if (!raw) return undefined;
    if (
      !/^\d+$/.test(raw) ||
      !Number.isSafeInteger(Number(raw)) ||
      Number(raw) < 1
    )
      throw new Error("上下文和输出上限须为正整数，留空使用默认值。");
    return Number(raw);
  };
  const assign = (
    object: Record<string, unknown>,
    key: string,
    value: unknown,
  ) => {
    if (value === undefined) delete object[key];
    else object[key] = value;
  };
  if (app === "codex") {
    const config = parse(c.config || "");
    if (changed(input.window, base?.window))
      assign(config, "model_context_window", positive(input.window));
    if (changed(input.compact, base?.compact))
      assign(config, "model_auto_compact_token_limit", positive(input.compact));
    c.config = stringify(config);
  } else if (app === "claude") {
    let text = JSON.stringify(c);
    if (changed(input.window, base?.window)) {
      const next = updateClaudeContextLimit(text, input.window);
      if (next === null)
        throw new Error("上下文上限须为 1 至 2000000 的整数。");
      text = next;
    }
    if (changed(input.compact, base?.compact)) {
      const next = updateClaudeAutoCompactWindow(text, input.compact);
      if (next === null)
        throw new Error("自动压缩窗口须为 100000 至 1000000 的整数。");
      text = next;
    }
    p.settingsConfig = JSON.parse(text);
  } else if (app === "gemini") {
    let text = JSON.stringify(c.config || {});
    if (changed(input.window, base?.window)) {
      const next = updateGeminiHistoryTokenLimit(text, input.window);
      if (next === null)
        throw new Error("历史窗口上限须为 1 至 2000000 的整数。");
      text = next;
    }
    if (changed(input.enabled, base?.enabled)) {
      const next = updateGeminiContextManagementEnabled(text, input.enabled);
      if (next === null)
        throw new Error("历史压缩设置无法保存，请重新读取配置。");
      text = next;
    }
    c.config = JSON.parse(text);
  }
  if (app === "codex" || app === "opencode") {
    for (const [id, row] of Object.entries(input.models)) {
      const old = base?.models[id] ?? { window: "", output: "" };
      if (!changed(row.window, old.window) && !changed(row.output, old.output))
        continue;
      check(row.window, old.window, current?.models[id]?.window ?? "");
      if (app === "opencode")
        check(row.output, old.output, current?.models[id]?.output ?? "");
      // Do not re-add a model removed while this drawer was open.
      if (base?.models[id] && !current?.models[id])
        throw new Error("模型列表已变化，请关闭抽屉并刷新后重试。");
      if (app === "codex") {
        const models = c.modelCatalog?.models || [];
        const entry = models.find(
          (model: { model: string }) => model.model === id,
        );
        if (!entry) throw new Error("请先保存此模型，再调整上下文上限。");
        if (changed(row.window, old.window)) {
          assign(entry, "contextWindow", positive(row.window));
          delete entry.context_window;
        }
      } else {
        const entry = c.models?.[id];
        if (!entry) throw new Error("请先保存此模型，再调整上下文上限。");
        const limit = { ...entry.limit };
        if (changed(row.window, old.window))
          assign(limit, "context", positive(row.window));
        if (changed(row.output, old.output))
          assign(limit, "output", positive(row.output));
        if (Object.keys(limit).length) entry.limit = limit;
        else delete entry.limit;
      }
    }
  }
  return p;
}
function claudeKeyField(
  p: Provider,
): "ANTHROPIC_API_KEY" | "ANTHROPIC_AUTH_TOKEN" {
  if (
    p.meta?.apiKeyField === "ANTHROPIC_API_KEY" ||
    p.meta?.apiKeyField === "ANTHROPIC_AUTH_TOKEN"
  )
    return p.meta.apiKeyField;
  return p.settingsConfig.env?.ANTHROPIC_AUTH_TOKEN
    ? "ANTHROPIC_AUTH_TOKEN"
    : p.settingsConfig.env?.ANTHROPIC_API_KEY
      ? "ANTHROPIC_API_KEY"
      : "ANTHROPIC_AUTH_TOKEN";
}
export function readConnection(
  p: Provider,
  app: ConnectionToolId,
): ConnectionDraft {
  const c = p.settingsConfig;
  const d = app === "codex" ? parse(c.config || "") : {};
  const table = (
    d.model_providers as Record<string, Record<string, unknown>> | undefined
  )?.[String(d.model_provider || "")];
  return {
    name: p.name,
    hideAggregationName: p.meta?.hideAggregationName ?? true,
    baseUrl: String(
      app === "codex"
        ? table?.base_url || d.base_url || ""
        : app === "opencode"
          ? c.options?.baseURL || ""
          : c.env?.[
              app === "claude" || app === "claude-desktop"
                ? "ANTHROPIC_BASE_URL"
                : "GOOGLE_GEMINI_BASE_URL"
            ] || "",
    ),
    apiKey: String(
      app === "codex"
        ? c.auth?.OPENAI_API_KEY || ""
        : app === "opencode"
          ? c.options?.apiKey || ""
          : c.env?.[
              app === "claude" || app === "claude-desktop"
                ? claudeKeyField(p)
                : "GEMINI_API_KEY"
            ] || "",
    ),
    model: String(
      app === "codex"
        ? d.model || ""
        : app === "opencode"
          ? Object.keys(c.models || {})[0] || ""
          : c.env?.[
              app === "claude" || app === "claude-desktop"
                ? "ANTHROPIC_MODEL"
                : "GEMINI_MODEL"
            ] || "",
    ),
    protocol:
      (p.meta?.apiFormat as ConnectionDraft["protocol"]) ||
      (app === "claude" || app === "claude-desktop"
        ? "anthropic"
        : app === "gemini"
          ? "gemini_native"
          : app === "codex"
            ? "openai_responses"
            : "openai_chat"),
  };
}
export function simpleEditable(p: Provider, app: ConnectionToolId) {
  if (
    p.category === "official" ||
    p.meta?.osirBinding ||
    p.meta?.providerType ||
    p.meta?.authBinding?.source === "managed_account"
  )
    return false;
  if (
    app === "opencode" &&
    (!p.settingsConfig.npm || p.settingsConfig.providers)
  )
    return false;
  if (app === "codex") {
    try {
      const d = parse(p.settingsConfig.config || "");
      const tables = d.model_providers as
        | Record<string, Record<string, unknown>>
        | undefined;
      const active = tables?.[String(d.model_provider || "")];
      if (
        active?.env_key ||
        active?.experimental_bearer_token ||
        active?.http_headers ||
        active?.env_http_headers ||
        (active?.wire_api && active.wire_api !== "responses")
      )
        return false;
      if (p.settingsConfig.auth?.tokens) return false;
    } catch {
      return false;
    }
  }
  return true;
}
export function buildConnection(
  app: ConnectionToolId,
  input: ConnectionDraft,
  previous?: Provider,
): Provider {
  const name = input.name.trim(),
    model = input.model.trim(),
    apiKey = input.apiKey.trim();
  if (!name || !apiKey || !model)
    throw new Error("请填写供应商名称、密钥和模型。");
  if (
    (app === "codex" || app === "claude-desktop") &&
    !isCodexConversationModel(model)
  )
    throw new Error("图片生成模型不能用作对话模型，请选择对话模型。");
  const url = new URL(input.baseUrl.trim());
  if (
    !["https:", "http:"].includes(url.protocol) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash
  )
    throw new Error(
      "请输入有效的 HTTP(S) 服务地址，不含密码、查询参数或片段。",
    );
  if (previous && !simpleEditable(previous, app))
    throw new Error(
      "此供应商使用原生登录或自定义配置，当前连接表单无法编辑。请在对应工具中调整原生配置。",
    );
  const p: Provider = previous
    ? structuredClone(previous)
    : { id: crypto.randomUUID(), name, settingsConfig: {}, category: "custom" };
  const c = p.settingsConfig;
  p.name = name;
  if (app === "codex")
    p.meta = {
      ...p.meta,
      hideAggregationName: input.hideAggregationName ?? true,
    };
  const normalized = url.toString();
  const baseUrl = normalized.endsWith("/")
    ? normalized.slice(0, -1)
    : normalized;
  if (app === "codex") {
    const d = parse(c.config || "");
    const id = String(d.model_provider || "osir_custom");
    const tables = (d.model_providers || {}) as Record<
      string,
      Record<string, unknown>
    >;
    tables[id] = {
      ...(tables[id] || {}),
      name,
      base_url: baseUrl,
      wire_api: "responses",
      requires_openai_auth: true,
    };
    d.model_provider = id;
    d.model = model;
    d.model_providers = tables as never;
    c.config = stringify(d);
    c.auth = { ...c.auth, OPENAI_API_KEY: apiKey };
    p.meta = { ...p.meta, apiFormat: input.protocol };
    const models = c.modelCatalog?.models || [];
    if (!models.some((x: { model: string }) => x.model === model))
      c.modelCatalog = { ...c.modelCatalog, models: [...models, { model }] };
  } else if (app === "opencode") {
    c.npm = c.npm || "@ai-sdk/openai-compatible";
    c.options = { ...c.options, baseURL: baseUrl, apiKey };
    c.models = { ...c.models, [model]: c.models?.[model] || { name: model } };
  } else {
    c.env = { ...c.env };
    if (app === "claude" || app === "claude-desktop") {
      c.env.ANTHROPIC_BASE_URL = baseUrl;
      const field =
        app === "claude-desktop" ? "ANTHROPIC_AUTH_TOKEN" : claudeKeyField(p);
      delete c.env.ANTHROPIC_API_KEY;
      delete c.env.ANTHROPIC_AUTH_TOKEN;
      c.env[field] = apiKey;
      p.meta = { ...p.meta, apiKeyField: field };
      c.env.ANTHROPIC_MODEL = model;
      if (app === "claude-desktop") {
        const routes = { ...p.meta?.claudeDesktopModelRoutes };
        const route =
          Object.keys(routes).find(
            (id) =>
              routes[id].model ===
              previous?.settingsConfig.env?.ANTHROPIC_MODEL,
          ) ??
          Object.keys(routes)[0] ??
          "claude-sonnet-osir";
        p.meta = {
          ...p.meta,
          claudeDesktopMode: "proxy",
          claudeDesktopModelRoutes: {
            ...routes,
            [route]: { ...routes[route], model },
          },
        };
      }
    } else {
      c.env.GOOGLE_GEMINI_BASE_URL = baseUrl;
      c.env.GEMINI_API_KEY = apiKey;
      c.env.GEMINI_MODEL = model;
    }
  }
  return p;
}
export function modelNames(p: Provider, app: ConnectionToolId): string[] {
  const list =
    app === "codex"
      ? p.settingsConfig.modelCatalog?.models?.map(
          (x: { model: string }) => x.model,
        )
      : app === "opencode"
        ? Object.keys(p.settingsConfig.models || {})
        : app === "claude-desktop"
          ? Object.values(p.meta?.claudeDesktopModelRoutes ?? {}).map(
              (route) => route.model,
            )
          : [];
  try {
    // Match Rust codex_published_specs: an explicit nonempty catalog is the
    // publication source; a stale top-level default must not add a model to it.
    if (app === "codex" && list?.length)
      return [...new Set<string>(list.filter(Boolean))];
    return [
      ...new Set<string>(
        [...(list || []), readConnection(p, app).model].filter(Boolean),
      ),
    ];
  } catch {
    return list || [];
  }
}

/** Model selection is the published Codex catalog, not a second routing engine. */
export function setCodexModels(
  previous: Provider,
  selected: string[],
  defaultModel: string,
  availableModels?: readonly string[],
): Provider {
  if (!previous.meta?.osirBinding && !simpleEditable(previous, "codex"))
    throw new Error("此供应商的模型由原生配置管理，请在对应工具中调整模型。");
  const models = [...new Set(selected.map((m) => m.trim()).filter(Boolean))];
  if (!models.length || !models.includes(defaultModel))
    throw new Error("至少选择一个模型，且默认模型必须在已选模型中。");
  if (
    models.some(
      (model) =>
        !isCodexConversationModel(
          model,
          previous.settingsConfig.modelCatalog?.models?.find(
            (entry: { model: string }) => entry.model === model,
          ),
        ),
    )
  )
    throw new Error("图片生成模型不用于 Codex 对话，请选择对话模型。");
  if (
    availableModels &&
    models.some((model) => !availableModels.includes(model))
  )
    throw new Error("所选模型已不可用，请重新选择模型。");
  const p = structuredClone(previous);
  const catalog = p.settingsConfig.modelCatalog || {};
  const old = catalog.models || [];
  if (catalog.autoReviewModel && !models.includes(catalog.autoReviewModel))
    throw new Error(
      "该模型用于自动审批，暂不能移除。请保留该模型，或先在 Codex 配置中更改审批模型。",
    );
  p.settingsConfig.modelCatalog = {
    ...catalog,
    models: models.map(
      (model) =>
        old.find((m: { model: string }) => m.model === model) || { model },
    ),
  };
  const d = parse(p.settingsConfig.config || "");
  d.model = defaultModel;
  p.settingsConfig.config = stringify(d);
  return p;
}
