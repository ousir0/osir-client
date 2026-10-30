import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import type { Provider } from "@/types";
import {
  accountQueryRoot,
  accountSessionKey,
  useAccountSession,
  useAccountVisible,
  type AccountSession,
} from "./accountApi";
import type { ConnectionToolId } from "./providerConfig";

function boundId(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0
    ? value
    : undefined;
}

type ConnectionDetails = {
  generation: number;
  providerId: string;
  keyId: number;
  groupId: number | null;
  keyName: string | null;
  groupName: string | null;
  maskedKey: string;
  groupChanged: boolean;
};

type RecoveryStage =
  | "restored"
  | "missing"
  | "confirmation_required"
  | "unavailable"
  | "invalid_connection"
  | "changed";

const recoveryCopy: Record<RecoveryStage, string> = {
  restored: "已恢复原保存的连接凭据。可重新尝试读取模型或使用此连接。",
  missing: "未找到原保存的连接凭据。请更换账户密钥后重新保存连接。",
  confirmation_required: "系统尚未确认访问。请再次点击恢复，并完成系统确认。",
  unavailable: "连接凭据暂时无法恢复，请稍后再次点击恢复。",
  invalid_connection: "此连接无法恢复凭据。请关闭抽屉并刷新连接列表后核对。",
  changed: "连接已变化。请关闭抽屉并刷新连接列表后重试。",
};

function recoveryStage(value: unknown): RecoveryStage {
  const reply = value as { restored?: unknown; stage?: unknown } | null;
  if (
    !reply ||
    typeof reply.restored !== "boolean" ||
    typeof reply.stage !== "string" ||
    !Object.prototype.hasOwnProperty.call(recoveryCopy, reply.stage) ||
    reply.restored !== (reply.stage === "restored")
  )
    return "unavailable";
  return reply.stage as RecoveryStage;
}

function accountContext(session: AccountSession | undefined) {
  return JSON.stringify([
    session?.generation,
    session?.connected,
    session?.profile?.id,
  ]);
}

function details(
  value: unknown,
  generation: number,
  providerId: string,
  keyId: number,
  groupId: number | null,
): ConnectionDetails {
  const item = value as Partial<ConnectionDetails> | null;
  if (
    !item ||
    item.generation !== generation ||
    item.providerId !== providerId ||
    item.keyId !== keyId ||
    item.groupId !== groupId ||
    (item.keyName !== null && typeof item.keyName !== "string") ||
    (item.groupName !== null && typeof item.groupName !== "string") ||
    typeof item.maskedKey !== "string" ||
    !/^••••[\x21-\x7e]{0,4}$/.test(item.maskedKey) ||
    typeof item.groupChanged !== "boolean"
  )
    throw new Error("连接详情无法确认");
  return {
    generation,
    providerId,
    keyId,
    groupId,
    keyName: item.keyName,
    groupName: item.groupName,
    maskedKey: item.maskedKey,
    groupChanged: item.groupChanged,
  };
}

/** Read metadata only while this connection belongs to the current account. */
export function ManagedConnectionDetails({
  provider,
  tool,
  preview,
  busy = false,
  onBusy,
}: {
  provider: Provider;
  tool: ConnectionToolId;
  preview: boolean;
  busy?: boolean;
  onBusy?: (value: boolean) => void;
}) {
  const client = useQueryClient();
  const session = useAccountSession();
  const visible = useAccountVisible();
  const binding = provider.meta?.osirBinding?.binding;
  const keyId = boundId(binding?.key_id);
  const groupId =
    binding?.group_id === null ? null : boundId(binding?.group_id);
  const generation = session.data?.generation;
  const sameAccount =
    session.data?.connected === true &&
    !!session.data.profile &&
    String(session.data.profile.id) === binding?.account_id;
  const native =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  const readable =
    native &&
    !preview &&
    sameAccount &&
    keyId !== undefined &&
    groupId !== undefined &&
    generation !== undefined;
  const [recovering, setRecovering] = useState(false);
  const [recovery, setRecovery] = useState<{
    context: string;
    stage: RecoveryStage;
  } | null>(null);
  const pending = useRef(false);
  const mounted = useRef(false);
  const busyCallback = useRef(onBusy);
  busyCallback.current = onBusy;
  const context = JSON.stringify([
    accountContext(session.data),
    tool,
    provider.id,
    keyId,
    groupId,
    binding?.account_id,
    binding?.account_origin,
    provider.meta?.osirBinding?.reference,
  ]);
  const liveContext = useRef(context);
  liveContext.current = context;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (pending.current) busyCallback.current?.(false);
    };
  }, []);
  const queryKey = [
    ...accountQueryRoot,
    generation,
    "connection-details",
    tool,
    provider.id,
    keyId,
    groupId,
    binding?.account_id,
    binding?.account_origin,
    provider.meta?.osirBinding?.reference,
  ] as const;
  const query = useQuery({
    queryKey,
    queryFn: async ({ signal }) => {
      const current = () => {
        const active = client.getQueryData<AccountSession>(accountSessionKey);
        return (
          !signal.aborted &&
          active?.connected === true &&
          active.generation === generation &&
          String(active.profile?.id) === binding?.account_id
        );
      };
      if (!readable || !current()) throw new Error("账户已切换");
      const reply = await invoke<unknown>("osir_connection_details", {
        tool,
        providerId: provider.id,
        generation,
      });
      if (!current()) throw new Error("账户已切换");
      return details(reply, generation, provider.id, keyId, groupId);
    },
    enabled: readable && visible,
    retry: false,
    retryOnMount: false,
    staleTime: 180_000,
    gcTime: 300_000,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  const metadata = readable && !query.isError ? query.data : undefined;
  async function recoverCredential() {
    if (!native || preview || !visible || busy || pending.current) return;
    pending.current = true;
    setRecovering(true);
    setRecovery(null);
    busyCallback.current?.(true);
    const started = context;
    const startedAccount = accountContext(
      client.getQueryData<AccountSession>(accountSessionKey) ?? session.data,
    );
    const current = () =>
      mounted.current &&
      liveContext.current === started &&
      accountContext(
        client.getQueryData<AccountSession>(accountSessionKey) ?? session.data,
      ) === startedAccount;
    try {
      const reply = await invoke<unknown>(
        "osir_recover_connection_credential",
        { appType: tool, providerId: provider.id },
      );
      if (!current()) return;
      const stage = recoveryStage(reply);
      setRecovery({ context: started, stage });
      if (stage === "restored")
        void client.invalidateQueries({ queryKey, exact: true });
    } catch {
      if (current()) setRecovery({ context: started, stage: "unavailable" });
    } finally {
      pending.current = false;
      if (mounted.current) {
        setRecovering(false);
        busyCallback.current?.(false);
      }
    }
  }
  const group =
    groupId === null
      ? "未绑定分组"
      : groupId !== undefined
        ? metadata?.groupName?.trim() || `分组 #${groupId}（名称暂不可用）`
        : "分组信息暂不可用";
  const key =
    keyId !== undefined
      ? metadata?.keyName?.trim() || `密钥 #${keyId}（名称暂不可用）`
      : "密钥信息暂不可用";
  const savedMask = provider.meta?.osirBinding?.masked_key?.trim();

  return (
    <div className="osir-key-selection" aria-label="账户连接详情">
      <strong>所属分组：{group}</strong>
      <p>账户密钥：{key}</p>
      <p>
        密钥标识：
        {metadata?.maskedKey ||
          (savedMask && /^••••[\x21-\x7e]{0,4}$/.test(savedMask)
            ? savedMask
            : null) ||
          "密钥标识暂不可用"}
      </p>
      <p>已有密钥的分组只读。更换账户密钥后，所属分组随所选密钥更新。</p>
      {!preview && native && (
        <div>
          <button
            type="button"
            disabled={busy || recovering || !visible}
            onClick={() => void recoverCredential()}
          >
            {recovering ? "正在恢复连接凭据…" : "恢复连接凭据"}
          </button>
          <p className="osir-muted">
            仅恢复原保存的凭据。若系统要求钥匙串密码，请在系统窗口输入；确认是
            OSIR Client 后，可选“始终允许”记住许可，“允许”仅对本次有效。
          </p>
          {recovery?.context === context && (
            <p
              role={recovery.stage === "restored" ? "status" : "alert"}
              className={
                recovery.stage === "restored" ? "osir-muted" : "osir-error"
              }
            >
              {recoveryCopy[recovery.stage]}
            </p>
          )}
        </div>
      )}
      {metadata?.groupChanged && (
        <p role="alert" className="osir-error">
          此密钥在网站上的分组已变化，本地连接仍保留原分组。请核对网站分组后重新添加连接。
        </p>
      )}
      {readable && query.isFetching ? (
        <p role="status">正在读取密钥与分组名称…</p>
      ) : readable && query.isError ? (
        <p role="alert" className="osir-error">
          账户连接详情读取失败。
          <button type="button" onClick={() => void query.refetch()}>
            重试读取详情
          </button>
        </p>
      ) : !preview && native && session.data?.connected && !sameAccount ? (
        <p className="osir-muted">请登录此连接所属的账户后同步名称。</p>
      ) : !metadata ? (
        <p className="osir-muted">
          账户名称详情需在桌面客户端登录原账户后读取。
        </p>
      ) : null}
    </div>
  );
}
