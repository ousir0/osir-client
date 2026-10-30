import { useCallback, useEffect, useId, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Loader2, Play, RotateCw, Save } from "lucide-react";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { useAccountResource, useAccountSession } from "./accountApi";
import { connectionError } from "./errors";
import { workBuddyLaunchNotice, type WorkBuddyLaunch } from "./workbuddyLaunch";
import "./workbuddy-models.css";

type Config = {
  path: string;
  revision: string;
  isolated: boolean;
  models: { id: string; managed: boolean }[];
};
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
type Props = {
  desktop: boolean;
  onBusyChange: (busy: boolean) => void;
  onDirtyChange: (dirty: boolean) => void;
};
function unavailable(key: AccountKey) {
  const expiry = key.expires_at ? Date.parse(key.expires_at) : null;
  return (
    key.status !== "active" ||
    (expiry !== null && (!Number.isFinite(expiry) || expiry <= Date.now()))
  );
}

export function WorkBuddyModelSetup(props: Props) {
  const session = useAccountSession();
  return (
    <WorkBuddyModelForm
      key={`${session.data?.generation}:${session.data?.connected}`}
      {...props}
      connected={!!session.data?.connected}
      generation={session.data?.generation}
    />
  );
}

function WorkBuddyModelForm({
  desktop,
  onBusyChange,
  onDirtyChange,
  connected,
  generation,
}: Props & {
  connected: boolean;
  generation?: number;
}) {
  const labelId = useId();
  const [config, setConfig] = useState<Config>();
  const [reading, setReading] = useState(false);
  const [configError, setConfigError] = useState("");
  const [page, setPage] = useState(1);
  const keys = useAccountResource<KeyPage>(
    "keys",
    { page, page_size: 20 },
    desktop && connected,
  );
  const [keyId, setKeyId] = useState<number>();
  const selectedKey = keys.data?.data.items.find((key) => key.id === keyId);
  const [models, setModels] = useState<string[]>([]);
  const [selectedModels, setSelectedModels] = useState<string[]>([]);
  const [modelState, setModelState] = useState<
    "idle" | "loading" | "loaded" | "failed"
  >("idle");
  const [catalogRevision, setCatalogRevision] = useState(0);
  const [toolCall, setToolCall] = useState(true);
  const [images, setImages] = useState(false);
  const [reasoning, setReasoning] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [reloadAvailable, setReloadAvailable] = useState(false);
  const [reloadConfirm, setReloadConfirm] = useState(false);
  const alive = useRef(true);
  const lock = useRef(false);
  const readEpoch = useRef(0);
  const callbacks = useRef({ onBusyChange, onDirtyChange });
  callbacks.current = { onBusyChange, onDirtyChange };
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      callbacks.current.onDirtyChange(false);
      callbacks.current.onBusyChange(false);
    };
  }, []);
  useEffect(
    () => callbacks.current.onDirtyChange(keyId !== undefined),
    [keyId],
  );
  useEffect(
    () => callbacks.current.onBusyChange(busy || reloadConfirm),
    [busy, reloadConfirm],
  );

  const readConfig = useCallback(async () => {
    if (!desktop) return;
    const epoch = ++readEpoch.current;
    setReading(true);
    setConfigError("");
    try {
      const value = await invoke<Config>("osir_workbuddy_config");
      if (alive.current && epoch === readEpoch.current) setConfig(value);
    } catch (e) {
      if (alive.current && epoch === readEpoch.current) {
        setConfig(undefined);
        setConfigError(connectionError(e));
      }
    } finally {
      if (alive.current && epoch === readEpoch.current) setReading(false);
    }
  }, [desktop]);
  useEffect(() => {
    void readConfig();
  }, [readConfig]);

  useEffect(() => {
    let active = true;
    setModels([]);
    setSelectedModels([]);
    setError("");
    if (!desktop || !connected || generation === undefined || !keyId) {
      setModelState("idle");
      return;
    }
    setModelState("loading");
    void invoke<{ generation: number; models: { id: string }[] }>(
      "osir_account_key_models",
      { keyId, generation },
    )
      .then((reply) => {
        if (!active) return;
        if (reply.generation !== generation)
          throw new Error("账户已切换，请重新选择密钥");
        setModels(reply.models.map((model) => model.id));
        setModelState("loaded");
      })
      .catch((e) => {
        if (!active) return;
        setModelState("failed");
        setError(connectionError(e));
      });
    return () => {
      active = false;
    };
  }, [desktop, connected, generation, keyId, catalogRevision]);

  function resetDraft() {
    setKeyId(undefined);
    setToolCall(true);
    setImages(false);
    setReasoning(false);
    setError("");
  }
  async function save() {
    if (
      lock.current ||
      !desktop ||
      !connected ||
      generation === undefined ||
      !config ||
      !selectedKey ||
      unavailable(selectedKey) ||
      !selectedModels.length ||
      modelState !== "loaded"
    )
      return;
    lock.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await invoke<Config>("osir_workbuddy_save_models", {
        request: {
          keyId,
          generation,
          modelIds: selectedModels,
          expectedRevision: config.revision,
          supportsToolCall: toolCall,
          supportsImages: images,
          supportsReasoning: reasoning,
        },
      });
      if (!alive.current) return;
      setConfig(result);
      resetDraft();
      setReloadAvailable(false);
      setNotice("模型已保存，正在打开 WorkBuddy…");
      try {
        const launched = await invoke<WorkBuddyLaunch>("osir_launch_workbuddy");
        if (alive.current) {
          setNotice(workBuddyLaunchNotice(launched, true));
          if (launched.alreadyRunning) {
            setReloadAvailable(true);
          }
        }
      } catch (e) {
        if (alive.current) {
          setNotice("模型已保存，可点击打开 WorkBuddy 重试。");
          setError(connectionError(e));
        }
      }
    } catch (e) {
      if (alive.current) setError(connectionError(e));
    } finally {
      lock.current = false;
      if (alive.current) {
        setBusy(false);
      }
    }
  }

  async function open() {
    if (lock.current || !desktop) return;
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      const result = await invoke<WorkBuddyLaunch>("osir_launch_workbuddy");
      if (alive.current) {
        setNotice(workBuddyLaunchNotice(result, reloadAvailable));
        if (
          !result.alreadyRunning &&
          result.foreground === true &&
          result.runtimeStatus !== "unavailable"
        )
          setReloadAvailable(false);
      }
    } catch (e) {
      if (alive.current) setError(connectionError(e));
    } finally {
      lock.current = false;
      if (alive.current) {
        setBusy(false);
      }
    }
  }

  async function reload() {
    if (lock.current || !desktop || !reloadConfirm || !reloadAvailable) return;
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      const result = await invoke<WorkBuddyLaunch>("osir_launch_workbuddy", {
        restart: true,
      });
      if (!alive.current) return;
      setNotice(workBuddyLaunchNotice(result, true, true));
      if (
        result.restarted === true &&
        result.foreground === true &&
        result.runtimeStatus !== "unavailable"
      )
        setReloadAvailable(false);
      setReloadConfirm(false);
    } catch (e) {
      if (alive.current) {
        setNotice(
          "模型已保存，重新载入未完成。请先保存 WorkBuddy 中的工作，再重试。",
        );
        setError(connectionError(e));
        setReloadConfirm(false);
      }
    } finally {
      lock.current = false;
      if (alive.current) setBusy(false);
    }
  }

  return (
    <section className="osir-workbuddy-models" aria-labelledby={labelId}>
      <header>
        <div>
          <h3 id={labelId}>自定义模型</h3>
          <p>选择账户密钥和模型，即可在 WorkBuddy 中使用。</p>
        </div>
      </header>
      {!desktop && <p role="status">请在桌面客户端配置 WorkBuddy 模型。</p>}
      {reading && <p role="status">正在读取本地模型配置…</p>}
      {configError && (
        <div>
          <p role="alert">{configError}</p>
          <button disabled={reading || busy} onClick={() => void readConfig()}>
            重新读取
          </button>
        </div>
      )}
      {config?.isolated && (
        <p className="osir-workbuddy-environment">
          开发隔离环境 · 保存和打开使用同一份测试配置
        </p>
      )}
      {!connected ? (
        <>
          <p role="status">请先登录 OSIRAPI 账户，再选择账户密钥。</p>
          <button disabled={!desktop || busy} onClick={() => void open()}>
            <Play size={14} />
            打开 WorkBuddy
          </button>
        </>
      ) : (
        <>
          <label
            className="osir-workbuddy-key-label"
            htmlFor={`${labelId}-key`}
          >
            账户密钥
          </label>
          {keys.isPending && <p role="status">正在读取账户密钥…</p>}
          {keys.isError && (
            <p role="alert">
              账户密钥读取失败。
              <button
                disabled={busy || keys.isFetching}
                onClick={() => void keys.refetch()}
              >
                重试
              </button>
            </p>
          )}
          {keys.data && (
            <>
              <select
                id={`${labelId}-key`}
                value={keyId ?? ""}
                disabled={busy || keys.isFetching}
                onChange={(event) => {
                  setNotice("");
                  setKeyId(
                    event.target.value ? Number(event.target.value) : undefined,
                  );
                }}
              >
                <option value="">请选择已有密钥</option>
                {keys.data.data.items.map((key) => (
                  <option
                    key={key.id}
                    value={key.id}
                    disabled={unavailable(key)}
                  >
                    {key.name} · {key.masked_key}
                    {unavailable(key) ? "（不可用）" : ""}
                  </option>
                ))}
              </select>
              {!keys.data.data.total && (
                <p>当前没有可用密钥，请先在密钥管理中创建。</p>
              )}
              {keys.data.data.total > 20 && (
                <div className="osir-workbuddy-pages">
                  <button
                    disabled={busy || keyId !== undefined || page <= 1}
                    onClick={() => setPage((value) => value - 1)}
                  >
                    上一页
                  </button>
                  <span>第 {page} 页</span>
                  <button
                    disabled={
                      busy ||
                      keyId !== undefined ||
                      page * 20 >= keys.data.data.total
                    }
                    onClick={() => setPage((value) => value + 1)}
                  >
                    下一页
                  </button>
                </div>
              )}
            </>
          )}
          {selectedKey && (
            <div className="osir-workbuddy-catalog-status">
              <p className="osir-muted">
                分组：
                {selectedKey.group_id == null
                  ? "未绑定分组"
                  : selectedKey.group?.name || `分组 #${selectedKey.group_id}`}
              </p>
            </div>
          )}
          {modelState === "loading" && (
            <p role="status">正在读取此密钥可用的模型…</p>
          )}
          {modelState === "failed" && (
            <button
              disabled={busy}
              onClick={() => setCatalogRevision((value) => value + 1)}
            >
              重试模型目录
            </button>
          )}
          {modelState === "loaded" && (
            <>
              <fieldset disabled={busy}>
                <legend>选择模型</legend>
                {models.length ? (
                  <div className="osir-workbuddy-model-options">
                    {models.map((id) => (
                      <label key={id}>
                        <input
                          type="checkbox"
                          checked={selectedModels.includes(id)}
                          onChange={(event) =>
                            setSelectedModels((values) =>
                              event.target.checked
                                ? [...values, id]
                                : values.filter((value) => value !== id),
                            )
                          }
                        />
                        <span>{id}</span>
                      </label>
                    ))}
                  </div>
                ) : (
                  <p>此密钥当前没有可用模型，请换一个密钥或稍后重试。</p>
                )}
              </fieldset>
            </>
          )}
          <div className="osir-workbuddy-actions">
            <button
              className="osir-primary"
              disabled={
                !desktop ||
                busy ||
                reading ||
                !config ||
                !selectedKey ||
                unavailable(selectedKey) ||
                !selectedModels.length ||
                modelState !== "loaded"
              }
              onClick={() => void save()}
            >
              {busy ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <Save size={14} />
              )}
              保存并打开
            </button>
            <button
              disabled={!desktop || busy || keyId !== undefined}
              onClick={() => void open()}
            >
              <Play size={14} />
              打开 WorkBuddy
            </button>
            {reloadAvailable && (
              <button
                className="osir-workbuddy-reload"
                aria-label="重新载入模型"
                title="重新载入模型"
                style={{ width: 34, minWidth: 34, padding: 0 }}
                disabled={busy || keyId !== undefined}
                onClick={() => setReloadConfirm(true)}
              >
                <RotateCw size={15} aria-hidden="true" />
              </button>
            )}
            {keyId !== undefined && (
              <>
                <button disabled={busy} onClick={resetDraft}>
                  取消编辑
                </button>
              </>
            )}
          </div>
        </>
      )}
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      <ConfirmDialog
        isOpen={reloadConfirm}
        title="重启 WorkBuddy？"
        message="模型配置已保存。重启会中断正在进行的对话，未保存的内容可能丢失。重启后请在模型菜单中选择新模型。"
        confirmText="确认重启"
        cancelText="取消"
        variant="info"
        className="osir-tool-update-dialog"
        pending={busy}
        onConfirm={() => void reload()}
        onCancel={() => {
          if (!busy) setReloadConfirm(false);
        }}
      />
      <details className="osir-workbuddy-config-details">
        <summary>配置详情</summary>
        {config && (
          <>
            <p className="osir-tools-path">配置位置：{config.path}</p>
            <p className="osir-muted">
              {config.models.length
                ? `已有 ${config.models.length} 个自定义模型：${config.models.map((model) => model.id).join("、")}`
                : "尚未配置自定义模型。"}
            </p>
          </>
        )}
        {modelState === "loaded" && !!models.length && (
          <fieldset disabled={busy}>
            <legend>模型能力</legend>
            <p className="osir-muted">请按所选模型实际支持的能力勾选。</p>
            <div className="osir-workbuddy-capabilities">
              <label>
                <input
                  type="checkbox"
                  checked={toolCall}
                  onChange={(event) => setToolCall(event.target.checked)}
                />
                工具调用
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={images}
                  onChange={(event) => setImages(event.target.checked)}
                />
                图片输入
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={reasoning}
                  onChange={(event) => setReasoning(event.target.checked)}
                />
                推理
              </label>
            </div>
          </fieldset>
        )}
        <p className="osir-muted">
          保存会备份本地配置，并将密钥写入 WorkBuddy 的本地模型文件。
        </p>
        <div className="osir-workbuddy-actions">
          <button
            disabled={!desktop || reading || busy}
            onClick={() => void readConfig()}
          >
            重新读取
          </button>
          {connected && (
            <button
              disabled={busy || keys.isFetching}
              onClick={() => void keys.refetch()}
            >
              刷新密钥
            </button>
          )}
          {modelState === "loaded" && (
            <button
              disabled={busy}
              onClick={() => setCatalogRevision((value) => value + 1)}
            >
              刷新模型
            </button>
          )}
        </div>
      </details>
    </section>
  );
}
