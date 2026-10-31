import { Trash2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { connectionError } from "./errors";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
  SheetBody,
} from "@/components/ui/sheet";
import { fetchModelsForConfig } from "@/lib/api/model-fetch";
import { defaultConnectionModel } from "./modelDefaults";
import { providersApi } from "@/lib/api";
import type { Provider } from "@/types";
import { AccountConnection } from "./AccountConnection";
import { ManagedConnectionDetails } from "./ManagedConnectionDetails";
import { GptContextWindowHint } from "@/components/providers/forms/shared/GptContextWindowHint";
import "./drawer.css";
import {
  buildConnection,
  applyConnectionContext,
  readConnectionContext,
  readConnection,
  isCodexConversationModel,
  TOOL_NAMES,
  type ConnectionToolId,
  type ConnectionDraft,
  type ConnectionContextDraft,
} from "./providerConfig";

export function ConnectionDrawer({
  tool,
  provider,
  onClose,
  onSaved,
  preview,
  initialSource = "account",
  reconnectName,
  onConnectAccount,
  onDelete,
}: {
  tool: ConnectionToolId;
  provider?: Provider;
  onClose: () => void;
  onSaved: (p: Provider) => void;
  preview: boolean;
  initialSource?: "manual" | "account";
  reconnectName?: string;
  onConnectAccount?: () => void;
  onDelete?: () => void;
}) {
  const managed = !!provider?.meta?.osirBinding;
  const reconnect = managed ? undefined : reconnectName;
  const [draft, setDraft] = useState<ConnectionDraft>(() =>
    provider && !managed
      ? readConnection(provider, tool)
      : {
          name: managed && provider ? provider.name : "OSIR API",
          hideAggregationName: provider?.meta?.hideAggregationName ?? true,
          baseUrl:
            tool === "codex" || tool === "opencode"
              ? "https://api.osirclaw.com/v1"
              : "https://api.osirclaw.com",
          apiKey: "",
          model: "",
          protocol:
            tool === "codex"
              ? "openai_responses"
              : tool === "claude" || tool === "claude-desktop"
                ? "anthropic"
                : tool === "gemini"
                  ? "gemini_native"
                  : "openai_chat",
        },
  );
  const [models, setModels] = useState<string[]>([]);
  const [context, setContext] = useState(() =>
    readConnectionContext(provider, tool),
  );
  const contextBase = useRef(context);
  const [contextModel, setContextModel] = useState("");
  const contextDirty =
    JSON.stringify(context) !== JSON.stringify(contextBase.current);
  const [busy, setBusy] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState("");
  const [source, setSource] = useState<"manual" | "account">(
    managed ? "account" : provider ? "manual" : initialSource,
  );
  const pending = useRef(false);
  const accountPanel = useRef<HTMLDivElement>(null);
  const reportBusy = useCallback((value: boolean) => {
    pending.current = value;
    setBusy(value);
  }, []);
  const [accountDirty, setAccountDirty] = useState(false);
  const [keyCreationDirty, setKeyCreationDirty] = useState(false);
  const [accountMounted, setAccountMounted] = useState(
    !provider && initialSource === "account",
  );
  const [confirmClose, setConfirmClose] = useState(false);
  const [replacingKey, setReplacingKey] = useState(false);
  const leaveAction = useRef(onClose);
  const [loadRevision, setLoadRevision] = useState(0);
  const [baseError, setBaseError] = useState("");
  const [editBase, setEditBase] = useState<Record<string, unknown>>();
  const [loadingBase, setLoadingBase] = useState(
    !preview && !!provider && !managed && tool !== "opencode",
  );
  useEffect(() => {
    if (preview || !provider || managed || tool === "opencode") return;
    let cancelled = false;
    setLoadingBase(true);
    setBaseError("");
    void providersApi
      .getEditorView(
        tool,
        provider.settingsConfig,
        provider.category,
        provider.id,
        provider.meta,
      )
      .then((view) => {
        if (!cancelled) {
          setEditBase(view.settings);
          const loadedContext = readConnectionContext(
            { ...provider, settingsConfig: view.settings },
            tool,
          );
          contextBase.current = loadedContext;
          setContext(loadedContext);
          setDraft(
            readConnection(
              { ...provider, settingsConfig: view.settings },
              tool,
            ),
          );
          setLoadingBase(false);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setBaseError("当前配置读取失败，请重试后再编辑和保存。");
          setLoadingBase(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [provider, tool, preview, loadRevision, managed]);
  const requestLeave = (action: () => void) => {
    if (pending.current) return;
    leaveAction.current = action;
    if (dirty || contextDirty || accountDirty || keyCreationDirty)
      setConfirmClose(true);
    else action();
  };
  const close = () => requestLeave(onClose);
  const aggregationChanged = (value: ConnectionDraft) =>
    tool === "codex" &&
    (value.hideAggregationName ?? true) !==
      (provider?.meta?.hideAggregationName ?? true);
  const set = (key: keyof ConnectionDraft, value: string) => {
    const next = { ...draft, [key]: value };
    setDraft(next);
    setDirty(
      managed
        ? next.name.trim() !== provider?.name || aggregationChanged(next)
        : true,
    );
    if (key === "baseUrl" || key === "apiKey" || key === "protocol")
      setModels([]);
  };
  async function fetchModels() {
    if (pending.current || loadingBase || baseError) return;
    reportBusy(true);
    setError("");
    if (tool === "gemini") {
      setModels([]);
      setDraft((current) => ({ ...current, model: "" }));
    }
    try {
      if (preview)
        throw new Error("浏览器预览不连接真实服务。请在桌面客户端获取模型。");
      if (!draft.apiKey.trim()) throw new Error("请先填写密钥。");
      validateEndpoint();
      const apiFormat =
        draft.protocol === "gemini_native"
          ? "google-generative-ai"
          : draft.protocol === "anthropic"
            ? "anthropic-messages"
            : draft.protocol;
      const list = await fetchModelsForConfig(
        draft.baseUrl.trim(),
        draft.apiKey.trim(),
        false,
        undefined,
        undefined,
        { apiFormat },
      );
      const modelIds = [
        ...new Set(
          list
            .filter(
              (model) =>
                (tool !== "codex" && tool !== "claude-desktop") ||
                isCodexConversationModel(model.id, model),
            )
            .map((model) => model.id)
            .filter(Boolean),
        ),
      ];
      setModels(modelIds);
      if (modelIds.length)
        setDraft((current) => ({
          ...current,
          model: defaultConnectionModel(tool, modelIds, current.model),
        }));
      if (!modelIds.length)
        setError(
          tool === "gemini"
            ? "服务未返回可用模型，请检查密钥、分组权限和服务地址后重试。"
            : "服务未返回可用模型，请检查分组权限或填写准确的模型 ID。",
        );
    } catch (e) {
      setError(connectionError(e, [draft.apiKey, draft.apiKey.trim()]));
    } finally {
      reportBusy(false);
    }
  }
  async function saveManagedSettings() {
    if (pending.current || !managed || !provider || preview) return;
    reportBusy(true);
    setError("");
    try {
      const name = draft.name.trim();
      if (
        !name ||
        new TextEncoder().encode(name).length > 160 ||
        /[\u0000-\u001f\u007f]/.test(name)
      )
        throw new Error("请输入有效的连接显示名称（最多 160 字节）。");
      // Re-read to preserve changes made since this drawer opened. Never rebuild
      // managed settings or credential bindings through the manual editor.
      const latest = (await providersApi.getAll(tool))[provider.id];
      if (!latest?.meta?.osirBinding)
        throw new Error("此账户连接已变化，请关闭抽屉并刷新后重试。");
      const configured = applyConnectionContext(
        {
          ...latest,
          name,
          ...(tool === "codex" && aggregationChanged(draft)
            ? {
                meta: {
                  ...latest.meta,
                  hideAggregationName: draft.hideAggregationName ?? true,
                },
              }
            : {}),
        },
        tool,
        context,
        contextBase.current,
      );
      const confirmed = await providersApi.update(
        configured,
        tool,
        undefined,
        undefined,
        latest,
      );
      if (!confirmed)
        throw new Error("保存未确认，请刷新供应商列表核对后重试。");
      setDirty(false);
      onSaved(configured);
      onClose();
    } catch (e) {
      setError(connectionError(e));
    } finally {
      reportBusy(false);
    }
  }
  const aggregationOption = (disabled: boolean) =>
    tool === "codex" ? (
      <label className="osir-checkbox-label">
        <input
          type="checkbox"
          checked={draft.hideAggregationName ?? true}
          disabled={disabled}
          onChange={(event) => {
            const next = {
              ...draft,
              hideAggregationName: event.target.checked,
            };
            setDraft(next);
            setDirty(
              managed
                ? next.name.trim() !== provider?.name ||
                    aggregationChanged(next)
                : true,
            );
          }}
        />
        不显示聚合名称
      </label>
    ) : null;
  async function save() {
    if (managed || loadingBase || baseError || pending.current) return;
    if (
      tool === "gemini" &&
      (!models.length || !models.includes(draft.model.trim()))
    ) {
      setError("请先成功获取模型目录，并选择其中一个模型后再保存。");
      return;
    }
    reportBusy(true);
    setError("");
    try {
      if (preview)
        throw new Error("浏览器预览不能保存真实连接，请在桌面客户端操作。");
      validateEndpoint();
      const result = buildConnection(
        tool,
        draft,
        provider && editBase
          ? { ...provider, settingsConfig: editBase }
          : provider,
      );
      const configured = applyConnectionContext(
        result,
        tool,
        context,
        contextBase.current,
      );
      const saved = provider
        ? await providersApi.update(
            configured,
            tool,
            undefined,
            editBase ? { base: editBase, onConflict: "refuse" } : undefined,
          )
        : await providersApi.add(configured, tool, false);
      if (!saved) throw new Error("保存未确认完成，请先检查供应商列表再重试。");
      onSaved(configured);
      onClose();
    } catch (e) {
      setError(connectionError(e, [draft.apiKey, draft.apiKey.trim()]));
    } finally {
      reportBusy(false);
    }
  }
  const blocked = busy || loadingBase || !!baseError;
  const contextFields = (disabled: boolean) => {
    if (tool === "claude-desktop")
      return (
        <p className="osir-muted" role="note">
          Claude
          桌面客户端暂不支持在此设置自定义上下文上限或自动压缩阈值。Claude Code
          的设置不适用于桌面客户端。
        </p>
      );
    const update = (next: ConnectionContextDraft) => setContext(next);
    const numberField = (
      label: string,
      value: string,
      onChange: (value: string) => void,
      maximum = Number.MAX_SAFE_INTEGER,
      minimum = 1,
      off = false,
    ) => (
      <label>
        {label}
        <input
          type="number"
          inputMode="numeric"
          min={minimum}
          max={maximum}
          step={1}
          value={value}
          disabled={disabled || off}
          placeholder="使用默认值"
          onChange={(event) => onChange(event.target.value)}
        />
      </label>
    );
    const ids = [
      ...new Set([
        ...Object.keys(context.models),
        ...(!managed && draft.model.trim() ? [draft.model.trim()] : []),
      ]),
    ].filter((id) => tool !== "codex" || isCodexConversationModel(id));
    const selected = ids.includes(contextModel) ? contextModel : ids[0] || "";
    const row = context.models[selected] ?? { window: "", output: "" };
    const setModel = (key: "window" | "output", value: string) =>
      update({
        ...context,
        models: { ...context.models, [selected]: { ...row, [key]: value } },
      });
    return (
      <section
        className="osir-form osir-drawer-section"
        aria-label="上下文设置"
      >
        <strong className="osir-drawer-section-title">上下文设置</strong>
        {tool === "codex" && (
          <>
            <p className="osir-muted">
              适用于 Codex CLI
              和桌面客户端。供应商窗口上限优先于下方的单个模型设置；留空不指定上限，由客户端决定。
            </p>
            {numberField(
              "供应商上下文上限（Token）",
              context.window,
              (window) => update({ ...context, window }),
            )}
            {numberField("自动压缩阈值（Token）", context.compact, (compact) =>
              update({ ...context, compact }),
            )}
          </>
        )}
        {tool === "claude" && (
          <>
            <p className="osir-muted">
              适用于此供应商的所有 Claude Code 模型，不适用于 Claude
              桌面客户端。
            </p>
            {numberField(
              "上下文上限（Token）",
              context.window,
              (window) => update({ ...context, window }),
              2_000_000,
            )}
            <small>关闭自动压缩时使用此上限；留空使用模型默认值。</small>
            {numberField(
              "自动压缩窗口（Token）",
              context.compact,
              (compact) => update({ ...context, compact }),
              1_000_000,
              100_000,
            )}
            <small>留空使用默认值。可用窗口与费用取决于供应商和模型。</small>
          </>
        )}
        {tool === "gemini" && (
          <>
            <p className="osir-muted">
              适用于此供应商的所有 Gemini CLI 模型。保存后需重启 Gemini CLI。
            </p>
            <label className="osir-checkbox-label">
              <input
                type="checkbox"
                checked={context.enabled}
                disabled={disabled}
                onChange={(event) =>
                  update({ ...context, enabled: event.target.checked })
                }
              />
              自动压缩历史记录
            </label>
            {numberField(
              "历史窗口上限（Token）",
              context.window,
              (window) => update({ ...context, window }),
              2_000_000,
              1,
              !context.enabled,
            )}
            <small>开启后达到上限时压缩较早的对话；留空使用 150000。</small>
          </>
        )}
        {(tool === "codex" || tool === "opencode") && ids.length > 0 && (
          <>
            <label>
              上下文设置模型
              <select
                value={selected}
                disabled={disabled}
                onChange={(event) => setContextModel(event.target.value)}
              >
                {ids.map((id) => (
                  <option key={id} value={id}>
                    {id}
                  </option>
                ))}
              </select>
            </label>
            {numberField("模型上下文上限（Token）", row.window, (value) =>
              setModel("window", value),
            )}
            <GptContextWindowHint
              modelId={selected}
              contextWindow={
                tool === "codex" ? context.window || row.window : row.window
              }
            />
            {tool === "opencode" &&
              numberField("模型输出上限（Token）", row.output, (value) =>
                setModel("output", value),
              )}
            <small>
              仅影响所选模型；留空不指定上限，不会提高上游模型的实际能力。
            </small>
          </>
        )}
      </section>
    );
  };
  function validateEndpoint() {
    let url: URL;
    try {
      url = new URL(draft.baseUrl.trim());
    } catch {
      throw new Error("请输入有效的 HTTP(S) 服务地址。");
    }
    if (
      !["https:", "http:"].includes(url.protocol) ||
      url.username ||
      url.password ||
      url.search ||
      url.hash
    )
      throw new Error(
        "服务地址须使用 HTTP(S)，且不能包含密码、查询参数或片段。",
      );
  }
  return (
    <Sheet
      open
      onOpenChange={(open) => {
        if (!open) close();
      }}
    >
      <SheetContent
        width={420}
        closeLabel="关闭连接配置"
        className="osir-drawer osir-connection-drawer"
      >
        <SheetHeader className="osir-connection-header">
          <SheetTitle>
            {managed || initialSource === "account"
              ? "配置连接"
              : provider
                ? "配置供应商"
                : "添加供应商"}
          </SheetTitle>
          <SheetDescription className="sr-only">
            {TOOL_NAMES[tool]} ·{" "}
            {managed && provider ? provider.name : "连接配置"}
          </SheetDescription>
        </SheetHeader>
        <div className="px-5" aria-live="polite">
          {error && (
            <p className="osir-error" role="alert">
              {error}
            </p>
          )}
          {baseError && (
            <p className="osir-error" role="alert">
              {baseError}
              <button
                type="button"
                disabled={busy || loadingBase}
                onClick={() => setLoadRevision((v) => v + 1)}
              >
                重新读取配置
              </button>
            </p>
          )}
        </div>
        {provider && onDelete && (
          <button
            type="button"
            className="osir-drawer-delete"
            aria-label={"删除供应商 " + provider.name}
            title="删除本地供应商"
            disabled={busy}
            onClick={onDelete}
          >
            <Trash2 size={17} />
          </button>
        )}
        <SheetBody className="osir-connection-body">
          {busy && (
            <p role="status">正在处理，请等待完成后再切换来源或关闭。</p>
          )}
          {preview && (
            <p className="osir-muted">浏览器预览不能获取模型或保存真实连接。</p>
          )}
          {loadingBase && (
            <p role="status">正在读取当前配置。读取完成前暂不能保存。</p>
          )}
          {!reconnect && !provider && (
            <>
              <div className="osir-segment">
                <button
                  aria-pressed={source === "account"}
                  disabled={blocked || !!provider}
                  onClick={() => {
                    if (!pending.current) {
                      setAccountMounted(true);
                      setSource("account");
                    }
                  }}
                >
                  OSIRAPI 账户密钥
                </button>
                <button
                  aria-pressed={source === "manual"}
                  disabled={blocked || !!reconnectName}
                  onClick={() => {
                    if (!pending.current) {
                      if (accountPanel.current?.querySelector('[role="alert"]'))
                        setError(
                          "账户连接仍有未解决的问题，请返回 OSIRAPI 账户密钥查看并重试。",
                        );
                      setSource("manual");
                    }
                  }}
                >
                  手动添加
                </button>
              </div>
            </>
          )}
          {managed && provider && !replacingKey && (
            <form
              className="osir-form"
              aria-label="编辑账户连接"
              onSubmit={(event) => {
                event.preventDefault();
                void saveManagedSettings();
              }}
            >
              <label>
                连接显示名称
                <input
                  value={draft.name}
                  onChange={(event) => set("name", event.target.value)}
                  required
                  maxLength={160}
                  disabled={busy || preview}
                />
              </label>
              {aggregationOption(busy || preview)}
              <ManagedConnectionDetails
                provider={provider}
                tool={tool}
                preview={preview}
                busy={busy}
                onBusy={reportBusy}
              />
              {contextFields(busy || preview)}
              {tool === "opencode" &&
              provider.meta?.liveConfigManaged !== false ? (
                <p className="osir-muted" role="note">
                  OpenCode 正在使用此连接时暂不支持原位换钥。请先从 OpenCode
                  配置中移除此连接，再刷新本页。
                </p>
              ) : (
                <button
                  type="button"
                  disabled={busy || preview}
                  onClick={() =>
                    requestLeave(() => {
                      setDirty(false);
                      setContext(contextBase.current);
                      setAccountDirty(false);
                      setDraft((value) => ({ ...value, name: provider.name }));
                      setError("");
                      setReplacingKey(true);
                    })
                  }
                >
                  更换账户密钥
                </button>
              )}
              <div className="osir-form-footer">
                <button type="button" disabled={busy} onClick={close}>
                  取消
                </button>
                <button
                  type="submit"
                  className="primary"
                  disabled={
                    busy ||
                    preview ||
                    !draft.name.trim() ||
                    (draft.name.trim() === provider.name &&
                      !aggregationChanged(draft) &&
                      !contextDirty)
                  }
                >
                  {busy ? "正在保存…" : "保存设置"}
                </button>
              </div>
            </form>
          )}
          {managed && provider && replacingKey && (
            <div
              onChangeCapture={(event) => {
                if (
                  (event.target as HTMLElement).closest(
                    '[aria-label="创建账户密钥"]',
                  )
                )
                  setKeyCreationDirty(true);
              }}
            >
              <button
                type="button"
                disabled={busy}
                onClick={() =>
                  requestLeave(() => {
                    setAccountDirty(false);
                    setKeyCreationDirty(false);
                    setReplacingKey(false);
                  })
                }
              >
                返回连接信息
              </button>
              <AccountConnection
                tool={tool}
                replacementProvider={provider}
                preview={preview}
                onBusy={reportBusy}
                onConnectAccount={onConnectAccount}
                onDirtyChange={setAccountDirty}
                onCancel={close}
                onSaved={(saved) => {
                  onSaved(saved);
                  onClose();
                }}
              />
            </div>
          )}
          {!managed && accountMounted && (
            <div
              ref={accountPanel}
              hidden={source !== "account"}
              onChangeCapture={(event) => {
                // The connection callback excludes the nested creation draft.
                if (
                  (event.target as HTMLElement).closest(
                    '[aria-label="创建账户密钥"]',
                  )
                )
                  setKeyCreationDirty(true);
              }}
            >
              <AccountConnection
                tool={tool}
                reconnectName={reconnect}
                active={source === "account"}
                preview={preview}
                onBusy={reportBusy}
                onConnectAccount={onConnectAccount}
                onDirtyChange={setAccountDirty}
                onCancel={close}
                onSaved={(saved) => {
                  onSaved(saved);
                  onClose();
                }}
              />
            </div>
          )}
          <div hidden={source !== "manual"}>
            <form
              onInvalidCapture={() =>
                setError("请检查必填项和服务地址格式后再保存。")
              }
              onSubmit={(e) => {
                e.preventDefault();
                void save();
              }}
              className="osir-form"
            >
              <label>
                连接显示名称
                <input
                  value={draft.name}
                  onChange={(e) => set("name", e.target.value)}
                  required
                  disabled={blocked}
                />
              </label>
              {aggregationOption(blocked)}
              <label>
                服务地址
                <input
                  type="url"
                  value={draft.baseUrl}
                  onChange={(e) => set("baseUrl", e.target.value)}
                  required
                  disabled={blocked}
                />
              </label>
              <label>
                API 密钥
                <input
                  type="password"
                  autoComplete="off"
                  value={draft.apiKey}
                  onChange={(e) => set("apiKey", e.target.value)}
                  required
                  disabled={blocked}
                  placeholder={
                    preview ? "预览请填写演示值" : "粘贴该分组绑定的密钥"
                  }
                />
              </label>
              {tool === "codex" && (
                <label>
                  上游协议
                  <select
                    value={draft.protocol}
                    onChange={(e) => set("protocol", e.target.value)}
                    disabled={blocked}
                  >
                    <option value="openai_responses">OpenAI Responses</option>
                    <option value="openai_chat">OpenAI Chat Completions</option>
                    <option value="anthropic">Anthropic Messages</option>
                  </select>
                  <small>非 Responses 协议通过 Codex 多模型聚合使用。</small>
                </label>
              )}
              <div className="osir-row">
                <strong>{tool === "opencode" ? "连接模型" : "默认模型"}</strong>
                <button
                  type="button"
                  disabled={
                    blocked ||
                    preview ||
                    !draft.apiKey.trim() ||
                    !draft.baseUrl.trim()
                  }
                  onClick={() => void fetchModels()}
                >
                  获取可用模型
                </button>
              </div>
              <label>
                <span className="sr-only">模型 ID</span>
                {tool === "gemini" ? (
                  <select
                    aria-label="模型 ID"
                    value={draft.model}
                    onChange={(e) => set("model", e.target.value)}
                    required
                    disabled={blocked || !models.length}
                  >
                    <option value="">
                      {models.length ? "选择模型" : "先获取可用模型"}
                    </option>
                    {models.map((model) => (
                      <option key={model} value={model}>
                        {model}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    list="osir-model-options"
                    value={draft.model}
                    onChange={(e) => set("model", e.target.value)}
                    required
                    disabled={blocked}
                    placeholder="从列表选择或填写模型 ID"
                  />
                )}
                {tool !== "gemini" && (
                  <datalist id="osir-model-options">
                    {models.map((model) => (
                      <option key={model} value={model} />
                    ))}
                  </datalist>
                )}
              </label>
              <p className="osir-muted">
                密钥的分组由 OSIR 网站绑定。此处不会修改云端分组。
              </p>
              {!managed && contextFields(blocked)}
              {provider && (
                <p className="osir-muted">
                  编辑当前已生效的供应商时，保存后将同步工具配置。
                </p>
              )}
              {(!draft.name.trim() ||
                !draft.baseUrl.trim() ||
                !draft.apiKey.trim() ||
                !draft.model.trim()) && (
                <p className="osir-muted">
                  请填写供应商名称、服务地址、密钥和模型后保存。
                </p>
              )}
              <div className="osir-form-footer">
                <button type="button" disabled={busy} onClick={close}>
                  取消
                </button>
                <button
                  type="submit"
                  className="primary"
                  disabled={
                    blocked ||
                    preview ||
                    !draft.name.trim() ||
                    !draft.apiKey.trim() ||
                    !draft.model.trim() ||
                    (tool === "gemini" &&
                      (!models.length ||
                        !models.includes(draft.model.trim()))) ||
                    !draft.baseUrl.trim()
                  }
                >
                  {busy ? "处理中…" : "保存连接"}
                </button>
              </div>
            </form>
          </div>
        </SheetBody>
        <Dialog open={confirmClose} onOpenChange={setConfirmClose}>
          <DialogContent zIndex="alert" role="alertdialog">
            <DialogHeader>
              <DialogTitle>放弃尚未保存的连接修改？</DialogTitle>
              <DialogDescription>
                {managed && !replacingKey
                  ? "关闭后将丢弃尚未保存的连接设置。"
                  : "关闭后丢弃两种来源的本地草稿。已在账户中创建的密钥会保留。"}
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button
                autoFocus
                variant="outline"
                onClick={() => setConfirmClose(false)}
              >
                继续编辑
              </Button>
              <Button
                variant="destructive"
                onClick={() => {
                  if (!pending.current) {
                    setConfirmClose(false);
                    leaveAction.current();
                  }
                }}
              >
                放弃修改
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </SheetContent>
    </Sheet>
  );
}
