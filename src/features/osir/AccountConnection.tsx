import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQueryClient } from "@tanstack/react-query";
import type { Provider } from "@/types";
import { useAccountResource, useAccountSession } from "./accountApi";
import {
  isCodexConversationModel,
  type ConnectionToolId,
} from "./providerConfig";
import { AccountKeyCreation } from "./AccountKeyCreation";
import { KeyRound } from "lucide-react";
import { OsirTabs } from "./OsirTabs";
import { connectionError } from "./errors";
import { defaultConnectionModel } from "./modelDefaults";

type AccountKey = {
  id: number;
  name: string;
  masked_key: string;
  group_id: number | null;
  group?: { name?: string } | null;
  status: string;
  expires_at?: string | null;
};
type KeyPage = { items: AccountKey[]; total: number };

function keyAvailability(key: AccountKey, currentKeyId?: number) {
  if (key.id === currentKeyId) return "当前连接正在使用";
  const expiry = key.expires_at ? Date.parse(key.expires_at) : null;
  if (expiry !== null && !Number.isFinite(expiry)) return "有效期无法确认";
  if (expiry !== null && expiry <= Date.now()) return "已过期";
  if (key.status !== "active") return "未启用";
  return null;
}

function keyGroup(key: AccountKey) {
  return key.group_id == null
    ? "未绑定分组"
    : key.group?.name || `分组 #${key.group_id}`;
}

type AccountConnectionProps = {
  active?: boolean;
  reconnectName?: string;
  replacementProvider?: Provider;
  onCancel?: () => void;
  tool: ConnectionToolId;
  preview: boolean;
  onSaved: (provider: Provider) => void;
  onBusy: (busy: boolean) => void;
  onConnectAccount?: () => void;
  onDirtyChange?: (dirty: boolean) => void;
};

export function AccountConnection(props: AccountConnectionProps) {
  const session = useAccountSession();
  // Remount before rendering another account so no draft or selected key survives.
  return (
    <AccountConnectionForm
      key={`${session.data?.generation}:${session.data?.connected}`}
      {...props}
      session={session}
    />
  );
}

function AccountConnectionForm({
  active = true,
  tool,
  preview,
  onSaved,
  onBusy,
  onConnectAccount,
  onDirtyChange,
  reconnectName,
  replacementProvider,
  onCancel,
  session,
}: AccountConnectionProps & {
  session: ReturnType<typeof useAccountSession>;
}) {
  const query = useQueryClient();
  const [page, setPage] = useState(1);
  const [createdKey, setCreatedKey] = useState<AccountKey>();
  const [keySearch, setKeySearch] = useState("");
  const [search, setSearch] = useState("");
  const [keyTab, setKeyTab] = useState("existing");
  const keyListId = useId();
  const connectionRef = useRef<HTMLDivElement>(null);
  const keyListRef = useRef<HTMLDivElement>(null);
  // Let short lists determine the form height. Only constrain a longer list
  // after reserving the actual space used by the fields and actions below it.
  useLayoutEffect(() => {
    const root = connectionRef.current;
    const list = keyListRef.current;
    const body = root?.closest<HTMLElement>(".osir-connection-body");
    if (!root || !list || !body || !active) return;
    const resize = () => {
      if (!list.getClientRects().length) return;
      const listBox = list.getBoundingClientRect();
      const bodyBox = body.getBoundingClientRect();
      const tail = root.getBoundingClientRect().bottom - listBox.bottom;
      const top = listBox.top - bodyBox.top + body.scrollTop;
      const padding = parseFloat(getComputedStyle(body).paddingBottom) || 0;
      const height = Math.max(
        96,
        Math.floor(body.clientHeight - top - tail - padding),
      );
      list.style.setProperty("--osir-key-list-height", `${height}px`);
    };
    resize();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(resize);
    observer.observe(root);
    observer.observe(body);
    return () => observer.disconnect();
  });
  const keys = useAccountResource<KeyPage>(
    "keys",
    { page, page_size: 20, ...(search ? { search } : {}) },
    !preview,
  );
  const [keyId, setKeyId] = useState<number>();
  const initialName = replacementProvider?.name || reconnectName || "OSIR API";
  const [name, setName] = useState(initialName);
  const [hideAggregationName, setHideAggregationName] = useState(true);
  const [acknowledged, setAcknowledged] = useState(false);
  const [model, setModel] = useState("");
  const [models, setModels] = useState<string[]>([]);
  const [modelCatalogState, setModelCatalogState] = useState<
    "idle" | "loading" | "loaded" | "failed"
  >("idle");
  const [modelRevision, setModelRevision] = useState(0);
  const dirtyCallback = useRef(onDirtyChange);
  dirtyCallback.current = onDirtyChange;
  useEffect(() => {
    dirtyCallback.current?.(
      keyId !== undefined ||
        name !== initialName ||
        model !== "" ||
        (tool === "codex" && !hideAggregationName),
    );
  }, [keyId, name, model, initialName, tool, hideAggregationName]);
  useEffect(() => () => dirtyCallback.current?.(false), []);
  const [busy, setBusy] = useState(false);
  const [creating, setCreating] = useState(false);
  const [createdNotice, setCreatedNotice] = useState("");
  const creationBusy = useCallback(
    (value: boolean) => {
      setCreating(value);
      onBusy(value);
    },
    [onBusy],
  );
  const [error, setError] = useState("");
  const pending = useRef(false);
  const generation = session.data?.generation;
  const currentKeyId = Number(
    replacementProvider?.meta?.osirBinding?.binding?.key_id,
  );
  const previousKeyId =
    Number.isSafeInteger(currentKeyId) && currentKeyId > 0
      ? currentKeyId
      : undefined;
  const context = useRef({ generation, active: true });
  context.current.generation = generation;
  useEffect(() => {
    context.current.active = true;
    return () => {
      context.current.active = false;
      onBusy(false);
    };
  }, [onBusy]);
  function resetSelection() {
    setKeyId(undefined);
    setModel("");
    setModels([]);
    setModelCatalogState("idle");
    setError("");
  }
  function searchKeys(value: string) {
    if (busy || creating || pending.current) return;
    const submitted = value.trim();
    setKeySearch(submitted);
    if (submitted === search && page === 1) return;
    setSearch(submitted);
    setPage(1);
    resetSelection();
    setCreatedNotice("");
    setCreatedKey(undefined);
  }
  function changePage(next: number) {
    setPage(next);
    resetSelection();
  }
  const keyPage =
    keys.data?.generation === generation ? keys.data?.data : undefined;
  const pageKeys = keyPage?.items ?? [];
  const allKeys =
    createdKey && !pageKeys.some((key) => key.id === createdKey.id)
      ? [createdKey, ...pageKeys]
      : pageKeys;
  const selected = allKeys.find((key) => key.id === keyId);
  const usable =
    selected !== undefined && keyAvailability(selected, previousKeyId) === null;
  useEffect(() => {
    if (
      !active ||
      preview ||
      keyTab !== "existing" ||
      creating ||
      keys.isFetching ||
      keys.isError ||
      session.isError ||
      !session.data?.connected ||
      generation === undefined ||
      keyId === undefined ||
      !usable
    )
      return;
    let cancelled = false;
    const current = () =>
      !cancelled &&
      context.current.active &&
      context.current.generation === generation;
    setModelCatalogState("loading");
    setError("");
    void (async () => {
      try {
        const result = await invoke<{
          generation: number;
          models: { id: string }[];
        }>("osir_account_key_models", { keyId, generation });
        if (!current()) return;
        if (result.generation !== generation) throw new Error("账户已变化");
        const ids = [
          ...new Set(
            result.models
              .filter(
                (entry) =>
                  (tool !== "codex" && tool !== "claude-desktop") ||
                  isCodexConversationModel(entry.id, entry),
              )
              .map((entry) => entry.id)
              .filter(Boolean),
          ),
        ];
        setModels(ids);
        setModelCatalogState("loaded");
        setModel((previous) => defaultConnectionModel(tool, ids, previous));
        if (!ids.length)
          setError(
            "此密钥所属分组没有返回可用模型，请检查密钥状态和分组权限后重试。",
          );
      } catch {
        if (!current()) return;
        setModelCatalogState("failed");
        setError("模型目录读取失败，请检查网络、密钥状态和分组权限后重试。");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [
    active,
    preview,
    keyTab,
    creating,
    keys.isFetching,
    keys.isError,
    session.isError,
    session.data?.connected,
    generation,
    keyId,
    usable,
    modelRevision,
    tool,
  ]);
  async function saveConnection() {
    if (
      !active ||
      keyTab !== "existing" ||
      preview ||
      creating ||
      pending.current ||
      keys.isFetching ||
      keys.isError ||
      session.isError ||
      !name.trim() ||
      modelCatalogState !== "loaded" ||
      !models.length ||
      !models.includes(model.trim()) ||
      (reconnectName && !acknowledged) ||
      !keyId ||
      generation === undefined ||
      !usable ||
      !selected ||
      keyAvailability(selected) !== null ||
      !session.data?.connected
    )
      return;
    pending.current = true;
    setBusy(true);
    onBusy(true);
    setError("");
    const started = generation;
    const current = () =>
      context.current.active && context.current.generation === started;
    try {
      const provider = await invoke<Provider>(
        replacementProvider
          ? "osir_replace_connection_key"
          : "osir_connect_key",
        {
          request: {
            tool,
            key_id: keyId,
            name: name.trim(),
            model: model.trim(),
            generation: started,
            ...(replacementProvider
              ? {
                  provider_id: replacementProvider.id,
                  expected_reference:
                    replacementProvider.meta?.osirBinding?.reference,
                }
              : {}),
            ...(models.length ? { models } : {}),
            ...(!replacementProvider && tool === "codex" && !hideAggregationName
              ? { hideAggregationName: false }
              : {}),
          },
        },
      );
      if (current()) {
        setKeyId(undefined);
        setName("OSIR API");
        setHideAggregationName(true);
        setModel("");
        setModels([]);
        dirtyCallback.current?.(false);
        onSaved(provider);
      }
    } catch (cause) {
      if (current()) {
        setError(
          replacementProvider
            ? "密钥更换未完成：" +
                connectionError(cause) +
                " 请刷新供应商列表核对后重试。"
            : "连接保存未确认完成，请先检查供应商列表，避免重复添加。",
        );
      }
    } finally {
      pending.current = false;
      if (context.current.active) {
        setBusy(false);
        onBusy(false);
      }
    }
  }
  if (preview)
    return (
      <div>
        <p className="osir-muted">账户连接需要在桌面客户端中完成浏览器授权。</p>
        {onConnectAccount && (
          <button type="button" onClick={onConnectAccount}>
            前往账户登录
          </button>
        )}
      </div>
    );
  if (session.isPending) return <p role="status">正在读取账户状态…</p>;
  if (session.isError)
    return (
      <p role="alert">
        账户状态暂不可用。
        <button
          type="button"
          disabled={session.isFetching}
          onClick={() => void session.refetch()}
        >
          重试账户状态
        </button>
      </p>
    );
  if (!session.data?.connected)
    return (
      <div>
        <p className="osir-muted">请先授权登录 OSIR 账户，再选择密钥。</p>
        {onConnectAccount && (
          <button type="button" onClick={onConnectAccount}>
            前往账户登录
          </button>
        )}
      </div>
    );
  return (
    <div className="osir-account-connection" ref={connectionRef}>
      {replacementProvider && (
        <p className="osir-muted" role="note">
          当前密钥：
          {replacementProvider.meta?.osirBinding?.masked_key || "标识暂不可用"}
          。更换密钥后保留原连接。
        </p>
      )}
      {reconnectName && (
        <div className="osir-reconnect-note" role="note">
          <strong>“{reconnectName}”将另存为新连接</strong>
          <p>当前暂不支持更换原连接的密钥；原连接会保留。</p>
          <label>
            <input
              type="checkbox"
              checked={acknowledged}
              disabled={busy || creating}
              onChange={(event) => setAcknowledged(event.target.checked)}
            />
            我确认添加新连接
          </label>
        </div>
      )}
      <fieldset disabled={busy || creating} className="osir-key-tabs">
        <OsirTabs
          label="账户密钥连接方式"
          value={keyTab}
          items={[
            { value: "existing", label: "已有密钥" },
            { value: "create", label: "创建账户密钥" },
          ]}
          onChange={setKeyTab}
        />
      </fieldset>
      {generation !== undefined && (
        <div hidden={keyTab !== "create"}>
          <fieldset disabled={busy}>
            <AccountKeyCreation
              generation={generation}
              canCreateKeys={session.data?.canCreateKeys === true}
              onBusy={creationBusy}
              onCreated={(key) => {
                setKeySearch("");
                setSearch("");
                setPage(1);
                setCreatedNotice(`已创建 ${key.name} · ${key.masked_key}。`);
                setCreatedKey(key);
                setKeyId(key.id);
                setModel("");
                setModels([]);
                setModelCatalogState("idle");
                setError("");
                setKeyTab("existing");
                void query.invalidateQueries({
                  queryKey: ["osir-account", generation, "keys"],
                });
              }}
            />
          </fieldset>
        </div>
      )}
      {createdNotice && <p role="status">{createdNotice}</p>}
      {error && (
        <p role="alert" className="osir-error">
          {error}
        </p>
      )}
      <form
        hidden={keyTab !== "existing"}
        className="osir-form"
        onSubmit={(event) => {
          event.preventDefault();
          void saveConnection();
        }}
      >
        <fieldset disabled={creating} className="osir-form">
          <div className="osir-row">
            <label className="osir-key-search">
              <span className="sr-only">搜索账户密钥</span>
              <input
                type="search"
                value={keySearch}
                onChange={(event) => setKeySearch(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    if (!event.nativeEvent.isComposing) searchKeys(keySearch);
                  }
                }}
                disabled={busy}
                placeholder="搜索密钥名称"
                aria-describedby={`${keyListId}-hint`}
                aria-controls={`${keyListId}-list`}
              />
            </label>
            <button
              type="button"
              disabled={busy}
              onClick={() => searchKeys(keySearch)}
            >
              搜索
            </button>
            <button
              type="button"
              aria-label="清除搜索"
              disabled={busy || (!keySearch && !search)}
              onClick={() => searchKeys("")}
            >
              清除
            </button>
          </div>
          <p id={`${keyListId}-hint`} className="sr-only">
            输入密钥名称，点击搜索或按 Enter 查询全部账户密钥。
          </p>
          <fieldset
            className="osir-key-picker"
            disabled={busy || keys.isFetching || keys.isError}
            aria-describedby={`${keyListId}-hint`}
            aria-busy={keys.isFetching}
          >
            <legend>已有账户密钥</legend>
            <div
              id={`${keyListId}-list`}
              className="osir-key-list"
              ref={keyListRef}
            >
              {allKeys.map((key) => {
                const unavailable = keyAvailability(key, previousKeyId);
                return (
                  <label
                    key={key.id}
                    className="osir-key-card"
                    data-selected={keyId === key.id}
                    data-unavailable={unavailable !== null}
                  >
                    <input
                      type="radio"
                      name={keyListId}
                      value={key.id}
                      checked={keyId === key.id}
                      disabled={unavailable !== null}
                      onChange={() => {
                        setKeyId(key.id);
                        setModels([]);
                        setModel("");
                        setModelCatalogState("idle");
                        setError("");
                      }}
                    />
                    <span className="osir-key-brand">
                      <KeyRound size={19} />
                    </span>
                    <span className="osir-key-content">
                      <span className="osir-key-title">
                        <strong>{key.name}</strong>
                        <span className="osir-key-mask">{key.masked_key}</span>
                      </span>
                      <span className="osir-key-group">
                        所属分组：{keyGroup(key)} · 只读
                      </span>
                    </span>
                    <span className="osir-key-status">
                      {unavailable || "可用"}
                    </span>
                  </label>
                );
              })}
            </div>
            {search &&
              keyPage &&
              !allKeys.length &&
              !keys.isFetching &&
              !keys.isError && (
                <p className="osir-key-hint" role="status">
                  没有匹配的密钥，请调整搜索。
                </p>
              )}
          </fieldset>
          {keys.isLoading && <p role="status">正在同步密钥…</p>}
          {keys.isError && (
            <p role="alert">
              密钥同步失败。
              <button
                type="button"
                disabled={busy || keys.isFetching}
                onClick={() => void keys.refetch()}
              >
                重新同步
              </button>
            </p>
          )}
          {!keys.isError &&
            !keys.isFetching &&
            !search &&
            keyPage &&
            !allKeys.length && (
              <p>
                暂无已有密钥。可在上方创建账户密钥，或在 OSIR 网站创建后同步。
              </p>
            )}
          <div
            className="osir-row"
            hidden={page === 1 && (keyPage?.total ?? 0) <= 20}
          >
            <button
              type="button"
              disabled={busy || keys.isFetching || page <= 1}
              onClick={() => changePage(page - 1)}
            >
              上一页
            </button>
            <span>第 {page} 页</span>
            <button
              type="button"
              disabled={
                busy ||
                keys.isFetching ||
                keys.isError ||
                !keyPage ||
                page * 20 >= keyPage.total
              }
              onClick={() => changePage(page + 1)}
            >
              下一页
            </button>
          </div>
          <label>
            连接名称
            <input
              value={name}
              onChange={(event) => setName(event.target.value)}
              disabled={busy}
              maxLength={160}
              required
            />
          </label>
          {tool === "codex" && !replacementProvider && (
            <label className="osir-checkbox-label">
              <input
                type="checkbox"
                checked={hideAggregationName}
                disabled={busy}
                onChange={(event) =>
                  setHideAggregationName(event.target.checked)
                }
              />
              不显示聚合名称
            </label>
          )}
          {selected && !usable && (
            <p role="alert">此密钥未启用或已过期，请在网站处理后重新同步。</p>
          )}
          <div hidden={!selected} className="osir-form">
            <div className="osir-row">
              <strong>默认模型</strong>
              {modelCatalogState === "loading" ? (
                <span className="osir-muted" role="status">
                  正在读取模型…
                </span>
              ) : (
                <button
                  type="button"
                  disabled={busy || keys.isFetching || keys.isError || !usable}
                  onClick={() => setModelRevision((revision) => revision + 1)}
                >
                  {modelCatalogState === "failed" ||
                  (modelCatalogState === "loaded" && models.length === 0)
                    ? "重试读取模型"
                    : "刷新模型目录"}
                </button>
              )}
            </div>
            <label>
              <span className="sr-only">账户连接模型 ID</span>
              <select
                aria-label="账户连接模型 ID"
                value={model}
                onChange={(event) => setModel(event.target.value)}
                disabled={
                  busy || modelCatalogState !== "loaded" || models.length === 0
                }
                required={!!selected}
              >
                <option value="" disabled>
                  {modelCatalogState === "loaded" && models.length === 0
                    ? "当前分组暂无可用模型"
                    : modelCatalogState === "loading"
                      ? "正在读取模型…"
                      : "模型暂不可用"}
                </option>
                {models.map((id) => (
                  <option key={id} value={id}>
                    {id}
                  </option>
                ))}
              </select>
            </label>
          </div>
          {models.length > 0 && (
            <p className="osir-muted">{models.length} 个可用模型</p>
          )}
          {models.length > 0 && !models.includes(model.trim()) && (
            <p role="alert" className="osir-error">
              请选择已读取目录中的模型，或重新读取目录。
            </p>
          )}
          <div className="osir-form-footer">
            {onCancel && (
              <button
                type="button"
                disabled={busy || creating}
                onClick={onCancel}
              >
                取消
              </button>
            )}
            <button
              type="submit"
              className="primary"
              disabled={
                busy ||
                keys.isFetching ||
                keys.isError ||
                modelCatalogState !== "loaded" ||
                !models.length ||
                !usable ||
                !model.trim() ||
                !name.trim() ||
                !!(reconnectName && !acknowledged) ||
                !models.includes(model.trim())
              }
            >
              {busy
                ? "正在处理连接…"
                : replacementProvider
                  ? "保存密钥更换"
                  : reconnectName
                    ? "另存为新连接"
                    : "保存连接"}
            </button>
          </div>
        </fieldset>
      </form>
    </div>
  );
}
