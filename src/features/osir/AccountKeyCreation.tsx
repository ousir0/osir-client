import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useAccountResource } from "./accountApi";

export type CreatedKeyMetadata = {
  id: number;
  name: string;
  masked_key: string;
  group_id: number | null;
  status: string;
  quota: number;
  quota_used: number;
  expires_at: string | null;
};
export type AccountKeyCreationProps = {
  generation: number;
  canCreateKeys: boolean;
  onCreated: (key: CreatedKeyMetadata) => void;
  onBusy: (busy: boolean) => void;
  onDirtyChange?: (dirty: boolean) => void;
  actionContainer?: HTMLElement | null;
};
type Group = { id: number; name?: string; status?: string };
type Attempt = { pending: boolean; uncertain: boolean; revision: number };
const idle: Attempt = { pending: false, uncertain: false, revision: 0 };
// Component timeouts/unmounts cannot cancel native mutations. Keep admission
// and reconciliation state across remounts for this renderer's auth generation.
const attempts = new Map<number, Attempt>();
const listeners = new Set<() => void>();
function updateAttempt(generation: number, patch: Partial<Attempt>) {
  const previous = attempts.get(generation) ?? idle;
  attempts.set(generation, {
    ...previous,
    ...patch,
    revision: previous.revision + 1,
  });
  listeners.forEach((notify) => notify());
}
function subscribe(notify: () => void) {
  listeners.add(notify);
  return () => {
    listeners.delete(notify);
  };
}

export function AccountKeyCreation({
  generation,
  canCreateKeys,
  onCreated,
  onBusy,
  onDirtyChange,
  actionContainer,
}: AccountKeyCreationProps) {
  // Native groups maps to the authenticated groups/available endpoint.
  const groups = useAccountResource<Group[]>("groups", {}, canCreateKeys);
  const keys = useAccountResource<unknown>(
    "keys",
    { page: 1, page_size: 20 },
    false,
  );
  const [name, setName] = useState("");
  const [group, setGroup] = useState("");
  const [quota, setQuota] = useState("0");
  const [expiry, setExpiry] = useState("");
  const attempt = useSyncExternalStore(
    subscribe,
    () => attempts.get(generation) ?? idle,
  );
  const busy = attempt.pending;
  const uncertain = attempt.uncertain;
  const [checked, setChecked] = useState(false);
  const [message, setMessage] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const refreshPending = useRef(false);
  const context = useRef({
    generation,
    active: true,
    operation: 0,
  });
  const callbacks = useRef({ onBusy, onCreated });
  callbacks.current = { onBusy, onCreated };
  context.current.generation = generation;
  useEffect(() => {
    context.current.active = true;
    return () => {
      context.current.active = false;
      context.current.operation++;
      callbacks.current.onBusy(false);
    };
  }, []);
  useEffect(() => {
    callbacks.current.onBusy(busy || refreshing);
  }, [busy, refreshing, generation]);
  useEffect(() => {
    setChecked(false);
  }, [attempt.revision]);
  useEffect(() => {
    context.current.operation++;
    setChecked(false);
    setMessage("");
    setName("");
    setGroup("");
    setQuota("0");
    setExpiry("");
    refreshPending.current = false;
    setRefreshing(false);
  }, [generation]);
  const available =
    groups.data?.generation === generation
      ? groups.data.data.filter(
          (g) =>
            Number.isSafeInteger(g.id) &&
            g.id > 0 &&
            (!g.status || g.status === "active"),
        )
      : [];
  const cleanName = name.trim();
  useEffect(() => {
    onDirtyChange?.(
      name !== "" || group !== "" || quota !== "0" || expiry !== "",
    );
  }, [name, group, quota, expiry, onDirtyChange]);
  const valid =
    cleanName.length > 0 &&
    new TextEncoder().encode(cleanName).length <= 100 &&
    !Array.from(cleanName).some(
      (c) => c.charCodeAt(0) < 32 || c.charCodeAt(0) === 127,
    ) &&
    available.some((g) => String(g.id) === group) &&
    quota.trim() !== "" &&
    Number.isFinite(Number(quota)) &&
    Number(quota) >= 0 &&
    Number(quota) <= 1_000_000_000 &&
    (expiry === "" ||
      (Number.isInteger(Number(expiry)) &&
        Number(expiry) >= 1 &&
        Number(expiry) <= 3650));
  async function create() {
    const existing = attempts.get(generation) ?? idle;
    if (
      !valid ||
      !canCreateKeys ||
      groups.isFetching ||
      groups.isError ||
      refreshPending.current ||
      existing.uncertain ||
      existing.pending
    )
      return;
    const started = generation;
    const operation = ++context.current.operation;
    const current = () =>
      context.current.active &&
      context.current.generation === started &&
      context.current.operation === operation;
    updateAttempt(started, { pending: true, uncertain: false });
    setMessage("");
    let timer: ReturnType<typeof setTimeout> | undefined;
    const execute = async () => {
      try {
        return await invoke<CreatedKeyMetadata>("osir_create_key", {
          request: {
            name: cleanName,
            group_id: Number(group),
            quota: Number(quota),
            expires_in_days: expiry === "" ? null : Number(expiry),
          },
          generation: started,
        });
      } finally {
        // Also runs for synchronous bridge failures. Timeout never releases
        // this lock; only completion of the actual native invocation does.
        updateAttempt(started, { pending: false, uncertain: true });
      }
    };
    try {
      const result = await Promise.race([
        execute(),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error("timeout")), 30_000);
        }),
      ]);
      if (!current()) return;
      updateAttempt(started, { uncertain: false });
      // Explicit projection prevents extra IPC fields from reaching callbacks.
      const {
        id,
        name,
        masked_key,
        group_id,
        status,
        quota: createdQuota,
        quota_used,
        expires_at,
      } = result;
      setMessage("密钥已创建，请另行选择密钥保存连接。工具配置尚未应用。");
      setName("");
      callbacks.current.onCreated({
        id,
        name,
        masked_key,
        group_id,
        status,
        quota: createdQuota,
        quota_used,
        expires_at,
      });
    } catch {
      if (current()) {
        updateAttempt(started, { uncertain: true });
        setChecked(false);
        setMessage("创建结果未确认，请先刷新密钥列表并核对，避免重复创建。");
      }
    } finally {
      clearTimeout(timer);
    }
  }
  async function refreshKeys() {
    if ((attempts.get(generation) ?? idle).pending || refreshPending.current)
      return;
    refreshPending.current = true;
    setRefreshing(true);
    const started = generation;
    const operation = context.current.operation;
    const revision = (attempts.get(started) ?? idle).revision;
    const current = () =>
      context.current.active &&
      context.current.generation === started &&
      context.current.operation === operation &&
      (attempts.get(started) ?? idle).revision === revision &&
      !(attempts.get(started) ?? idle).pending;
    setChecked(false);
    try {
      const result = await keys.refetch();
      if (!current()) return;
      if (result.isError || result.data?.generation !== started)
        throw new Error("unavailable");
      setChecked(true);
      setMessage("密钥列表已刷新，请在密钥列表核对是否已经创建。");
    } catch {
      if (current()) setMessage("密钥列表刷新失败，请稍后重新刷新核对。");
    } finally {
      if (context.current.active && context.current.generation === started) {
        refreshPending.current = false;
        setRefreshing(false);
      }
    }
  }
  if (!canCreateKeys)
    return (
      <p className="osir-muted">
        创建密钥需要写入权限。请退出账户后重新授权，并勾选密钥写入权限。
      </p>
    );
  const action = (
    <button
      type="button"
      className="primary"
      disabled={
        busy || uncertain || !valid || groups.isFetching || groups.isError
      }
      onClick={() => void create()}
    >
      {busy ? "正在创建…" : "创建密钥"}
    </button>
  );
  return (
    <div className="osir-form" role="group" aria-label="创建账户密钥">
      <label>
        密钥名称
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          disabled={busy || uncertain}
          maxLength={100}
          required
        />
      </label>
      <label>
        可用分组
        <select
          value={group}
          onChange={(e) => setGroup(e.target.value)}
          disabled={busy || uncertain || groups.isFetching}
          required
        >
          <option value="">选择可用分组</option>
          {available.map((g) => (
            <option key={g.id} value={g.id}>
              {g.name || "分组 #" + g.id}
            </option>
          ))}
        </select>
      </label>
      {groups.isError && (
        <p role="alert">
          可用分组读取失败。
          <button
            type="button"
            disabled={busy || groups.isFetching}
            onClick={() => void groups.refetch()}
          >
            刷新分组
          </button>
        </p>
      )}
      {!groups.isFetching && !groups.isError && available.length === 0 && (
        <p>暂无可用分组，暂不能创建密钥。</p>
      )}
      {groups.isFetching && <p role="status">正在读取可用分组…</p>}
      <label>
        额度（USD，0 表示不限额）
        <input
          type="number"
          min="0"
          max="1000000000"
          step="any"
          value={quota}
          onChange={(e) => setQuota(e.target.value)}
          disabled={busy || uncertain}
          required
        />
      </label>
      <label>
        有效天数（可选）
        <input
          type="number"
          min="1"
          max="3650"
          step="1"
          value={expiry}
          onChange={(e) => setExpiry(e.target.value)}
          disabled={busy || uncertain}
          placeholder="留空表示不设置到期时间"
        />
      </label>
      {!valid && !busy && !uncertain && (
        <p className="osir-muted">
          {new TextEncoder().encode(cleanName).length > 100
            ? "名称过长，请缩短后重试。"
            : Number(quota) < 0 || Number(quota) > 1_000_000_000
              ? "额度须为 0–1000000000 USD。"
              : expiry !== "" &&
                  (!Number.isInteger(Number(expiry)) ||
                    Number(expiry) < 1 ||
                    Number(expiry) > 3650)
                ? "有效天数须为 1–3650 的整数。"
                : "填写名称并选择所属分组，额度与有效期可按需设置。"}
        </p>
      )}
      {message && (
        <p
          className={uncertain ? "osir-error" : undefined}
          role={uncertain ? "alert" : "status"}
        >
          {message}
        </p>
      )}
      {uncertain && busy && (
        <p role="status">密钥仍在创建，请稍后刷新列表，避免重复创建。</p>
      )}
      {uncertain && !message && !busy && (
        <p role="status">上次创建结果需要核对，请先刷新密钥列表。</p>
      )}
      {uncertain && (
        <div>
          <button
            type="button"
            disabled={busy || refreshing || keys.isFetching}
            onClick={() => void refreshKeys()}
          >
            先刷新密钥列表
          </button>
          <button
            type="button"
            disabled={busy || refreshing || !checked || keys.isFetching}
            onClick={() => {
              if (
                (attempts.get(generation) ?? idle).pending ||
                refreshPending.current ||
                !checked
              )
                return;
              updateAttempt(generation, { uncertain: false });
              setMessage("");
              setChecked(false);
            }}
          >
            已核对未创建，重新创建
          </button>
        </div>
      )}
      {actionContainer ? createPortal(action, actionContainer) : action}
    </div>
  );
}
