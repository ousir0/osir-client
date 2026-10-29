import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQueryClient } from "@tanstack/react-query";
import { settingsApi } from "@/lib/api";
import { ArrowUpRight, LoaderCircle } from "lucide-react";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import osirClientIcon from "@/assets/brand/osir-client.png";
import "./welcome.css";
import {
  applyAccountSession,
  accountSessionKey,
  useAccountResource,
  useAccountSession,
  type AccountProfile,
  type AccountSession,
  type LogoutReport,
} from "./accountApi";

const failedRecovery = (recovery: AccountSession["recovery"]) =>
  recovery === "unlock_required" ||
  recovery === "retryable" ||
  recovery === "blocked";

export function OsirAccountPanel({
  available,
  reason,
  welcome = false,
  compact = false,
}: {
  available: boolean;
  reason: string;
  welcome?: boolean;
  compact?: boolean;
}) {
  const session = useAccountSession();
  const profile = useAccountResource<AccountProfile>("profile");
  const query = useQueryClient();
  const [waiting, setWaiting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [allowKeyCreation, setAllowKeyCreation] = useState(welcome);
  const attempt = useRef(0);
  const desktop = "__TAURI_INTERNALS__" in window;
  const mounted = useRef(false);
  const authorizing = useRef(false);
  const cancelling = useRef(false);
  const recovering = useRef(false);
  const replacing = useRef(false);
  const [reauthorizeGeneration, setReauthorizeGeneration] = useState<
    number | null
  >(null);
  const recovery = session.data?.recovery;
  const restoring =
    (!session.isError &&
      (recovery === "pending" || recovery === "restoring")) ||
    recovering.current;
  const savedSession =
    recovery === "retryable" ||
    recovery === "unlock_required" ||
    recovery === "blocked" ||
    recovery === "pending" ||
    recovery === "restoring" ||
    recovering.current;
  const [failed, setFailed] = useState(false);
  const canAuthorize =
    desktop &&
    (welcome || available) &&
    !session.isPending &&
    !session.isError &&
    !savedSession &&
    !busy;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      ++attempt.current;
      if (authorizing.current) {
        authorizing.current = false;
        void invoke<AccountSession>("osir_account_cancel_authorization")
          .then((status) => {
            applyAccountSession(query, status);
          })
          .catch(() => {});
      }
    };
  }, [query]);
  async function authorize(expectedGeneration?: number) {
    const current = query.getQueryData<AccountSession>(accountSessionKey);
    const expectedIdle =
      expectedGeneration !== undefined &&
      current?.generation === expectedGeneration &&
      !current.connected &&
      current.recovery === "idle";
    if (
      !mounted.current ||
      !desktop ||
      (expectedGeneration === undefined
        ? !canAuthorize || replacing.current
        : !expectedIdle) ||
      authorizing.current ||
      cancelling.current ||
      recovering.current
    )
      return;
    const id = ++attempt.current;
    authorizing.current = true;
    setWaiting(true);
    setFailed(false);
    setMessage("");
    try {
      const status = await invoke<AccountSession>("osir_account_authorize", {
        allowKeyCreation,
        ...(expectedGeneration !== undefined
          ? { generation: expectedGeneration }
          : {}),
      });
      if (mounted.current && attempt.current === id) {
        const accepted = applyAccountSession(query, status);
        setFailed(!accepted.connected);
        setMessage(
          accepted.connected
            ? "已登录 OSIR 账户。连接工具请另行选择密钥并保存供应商。"
            : "本次登录未完成，可重新发起浏览器授权。",
        );
      }
    } catch (error) {
      if (mounted.current && attempt.current === id) {
        setFailed(true);
        const safeReasons: Record<string, string> = {
          账户服务暂时不可用:
            "账户服务暂时不可用，本次登录未完成，请稍后重试。",
          安全凭据存储失败: "无法读取或保存安全凭据，请检查系统钥匙串后重试。",
          请先退出当前账户或完成本地凭据清理:
            "已有账户状态尚未恢复，请稍后重试登录。",
          无法打开系统浏览器: "无法打开系统浏览器，请检查默认浏览器后重试。",
        };
        setMessage(
          (typeof error === "string" && safeReasons[error]) ||
            "账户登录未完成。请检查网络和浏览器中的授权结果，然后重试。",
        );
      }
    } finally {
      if (attempt.current === id) {
        authorizing.current = false;
        setWaiting(false);
      }
    }
  }
  async function cancel() {
    if (cancelling.current || !authorizing.current) return;
    cancelling.current = true;
    setBusy(true);
    setMessage("");
    try {
      const status = await invoke<AccountSession>(
        "osir_account_cancel_authorization",
      );
      ++attempt.current;
      const accepted = applyAccountSession(query, status);
      authorizing.current = false;
      if (mounted.current) {
        setWaiting(false);
        setFailed(false);
        setMessage(
          accepted.connected
            ? "账户已登录，请使用退出账户结束登录。"
            : "已取消本次账户登录。需要时可重新授权。",
        );
      }
    } catch {
      if (mounted.current)
        setMessage(
          "未能确认取消。可重试取消，或在浏览器拒绝授权并等待本次登录结束。",
        );
    } finally {
      cancelling.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  async function retrySaved() {
    if (
      busy ||
      recovering.current ||
      !["retryable", "unlock_required"].includes(recovery || "")
    )
      return;
    recovering.current = true;
    const id = ++attempt.current;
    setBusy(true);
    setFailed(false);
    setMessage("");
    try {
      await session.retrySaved();
    } catch {
      if (mounted.current && attempt.current === id) {
        setFailed(true);
        setMessage("登录恢复未完成，请重试恢复或选择重新授权。");
      }
    } finally {
      recovering.current = false;
      if (mounted.current && attempt.current === id) setBusy(false);
    }
  }
  async function reauthorize() {
    if (
      reauthorizeGeneration === null ||
      replacing.current ||
      recovering.current ||
      authorizing.current ||
      cancelling.current ||
      busy
    )
      return;
    const expected = reauthorizeGeneration;
    const current = query.getQueryData<AccountSession>(accountSessionKey);
    if (
      current?.generation !== expected ||
      current.connected ||
      !failedRecovery(current.recovery)
    ) {
      setReauthorizeGeneration(null);
      return;
    }
    replacing.current = true;
    const id = ++attempt.current;
    setBusy(true);
    setFailed(false);
    setMessage("");
    try {
      const status = await invoke<AccountSession>(
        "osir_account_abandon_saved_login",
        { generation: expected },
      );
      if (!mounted.current || attempt.current !== id) return;
      const accepted = applyAccountSession(query, status);
      setReauthorizeGeneration(null);
      setBusy(false);
      if (
        status.generation > expected &&
        status.recovery === "idle" &&
        !status.connected &&
        accepted.generation === status.generation &&
        accepted.recovery === "idle" &&
        !accepted.connected
      ) {
        // The browser attempt owns its own cancellation lifecycle. Do not keep
        // the replacement guard alive while an old IPC reply may still linger.
        replacing.current = false;
        void authorize(status.generation);
      } else {
        setFailed(true);
        setMessage("登录状态已改变，请重新选择登录操作。");
      }
    } catch {
      if (mounted.current && attempt.current === id) {
        setFailed(true);
        setMessage("未能开始重新授权，请重试。");
      }
    } finally {
      replacing.current = false;
      if (mounted.current && attempt.current === id) {
        setBusy(false);
        setReauthorizeGeneration(null);
      }
    }
  }
  async function logout() {
    setBusy(true);
    setMessage("");
    try {
      const result = await invoke<LogoutReport>("osir_account_logout");
      if (result.localCleared)
        applyAccountSession(query, {
          connected: false,
          recovery: "idle",
          generation: result.generation,
        });
      if (mounted.current)
        setMessage(
          !result.localCleared
            ? "本机账户未能清除，请重试退出。"
            : result.remoteRevoked
              ? "已退出 OSIR 账户。"
              : "已清除本机账户；远端撤销未确认。",
        );
    } catch {
      if (mounted.current) setMessage("退出账户未完成，请重试退出。");
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  async function website() {
    try {
      if (desktop) await settingsApi.openExternal("https://osirclaw.com");
      else window.open("https://osirclaw.com", "_blank", "noopener,noreferrer");
    } catch {
      setMessage("无法打开 OSIR 网站");
    }
  }
  const account =
    session.data?.connected &&
    profile.data?.generation === session.data.generation
      ? profile.data.data
      : session.data?.connected
        ? session.data.profile
        : undefined;
  const canReauthorize =
    desktop &&
    !session.data?.connected &&
    failedRecovery(recovery) &&
    !session.isPending &&
    !session.isError &&
    !restoring &&
    !waiting &&
    !busy;
  const reauthorizeButton = canReauthorize && (
    <button
      type="button"
      className="osir-welcome-reauthorize"
      disabled={reauthorizeGeneration !== null}
      onClick={() => setReauthorizeGeneration(session.data?.generation ?? null)}
    >
      重新授权
    </button>
  );
  const reauthorizeDialog = (
    <ConfirmDialog
      isOpen={reauthorizeGeneration !== null}
      title="重新授权？"
      message="将在浏览器中重新登录，现有工具配置会保留。"
      confirmText={busy ? "正在准备…" : "重新授权"}
      cancelText="取消"
      variant="info"
      className="osir-welcome-reauthorize-dialog"
      pending={busy}
      onConfirm={() => void reauthorize()}
      onCancel={() => {
        if (!replacing.current) setReauthorizeGeneration(null);
      }}
    />
  );
  if (welcome && !session.data?.connected) {
    return (
      <main className="osir-welcome">
        <div
          className="osir-welcome-drag"
          data-tauri-drag-region
          aria-hidden="true"
        />
        <section
          className="osir-welcome-card"
          aria-labelledby="osir-welcome-title"
          aria-busy={waiting || busy || restoring || session.isPending}
        >
          <div className="osir-welcome-logo" aria-hidden="true">
            <img src={osirClientIcon} alt="" width={64} height={64} />
          </div>
          <h1 id="osir-welcome-title">欢迎使用 OSIRAPI</h1>
          <p className="osir-welcome-subtitle">连接账户，开启你的 AI 工作台</p>
          <button
            type="button"
            className="osir-welcome-login"
            disabled={
              reauthorizeGeneration !== null ||
              (waiting
                ? busy
                : desktop
                  ? session.isPending || restoring || busy
                  : false)
            }
            onClick={() => {
              if (!desktop) {
                setMessage("请在桌面客户端中授权登录 OSIRAPI。");
              } else if (waiting) {
                void cancel();
              } else if (session.isError) {
                void session.refetch();
              } else if (
                recovery === "retryable" ||
                recovery === "unlock_required"
              ) {
                void retrySaved();
              } else if (recovery === "blocked") {
                void logout();
              } else {
                void authorize();
              }
            }}
          >
            {waiting || session.isPending || restoring || busy ? (
              <LoaderCircle
                className="osir-welcome-spinner"
                aria-hidden="true"
              />
            ) : (
              <ArrowUpRight aria-hidden="true" />
            )}
            {waiting
              ? busy
                ? "正在取消…"
                : "等待浏览器授权 · 取消"
              : session.isPending || restoring
                ? "正在恢复登录…"
                : session.isError
                  ? "重试读取登录状态"
                  : recovery === "retryable" || recovery === "unlock_required"
                    ? "恢复登录"
                    : recovery === "blocked"
                      ? busy
                        ? "正在退出…"
                        : "退出已保存账户"
                      : "授权登录OSIRAPI"}
          </button>
          <p className="osir-welcome-note" role="status">
            {waiting
              ? "请在浏览器中确认授权，完成后将自动返回客户端。"
              : restoring || session.isPending
                ? "正在恢复已保存的登录，请稍候。"
                : session.isError
                  ? "账户状态暂不可用，请重试读取后继续。"
                  : recovery === "retryable"
                    ? "账户服务暂时不可用，已保留登录信息。请检查网络后重试恢复。"
                    : recovery === "unlock_required"
                      ? "已保存的登录暂未恢复。可重试恢复或重新授权。"
                      : recovery === "blocked"
                        ? "已保存的登录无法恢复，请重新授权。"
                        : "在浏览器中安全授权，完成后自动返回客户端"}
          </p>
          {reauthorizeButton}
          {message && (
            <p
              className="osir-welcome-feedback"
              role={failed ? "alert" : "status"}
            >
              {message}
            </p>
          )}
        </section>
        <span className="osir-welcome-wordmark" aria-hidden="true">
          OSIRAPI · 让 AI 触手可及
        </span>
        {reauthorizeDialog}
      </main>
    );
  }
  if (compact && session.data?.connected) {
    return (
      <section
        className="osir-account-session-actions"
        aria-label="账户登录管理"
        aria-busy={busy}
      >
        <div>
          <h3>登录管理</h3>
          <p className="osir-muted">
            退出后停止账户同步，已连接工具的密钥仍可能产生费用。
          </p>
          {message && <p role="status">{message}</p>}
        </div>
        <button type="button" disabled={busy} onClick={() => void logout()}>
          {busy ? "正在退出…" : "退出账户"}
        </button>
      </section>
    );
  }
  return (
    <section
      className="osir-auth-card"
      aria-busy={waiting || busy || restoring}
    >
      <h2>
        {waiting
          ? "等待浏览器授权"
          : session.data?.connected
            ? "OSIR 账户"
            : savedSession
              ? "恢复 OSIR 账户"
              : "登录 OSIR 账户"}
      </h2>
      {waiting ? (
        <p role="status">
          请在系统浏览器完成登录并确认权限，客户端会自动更新登录结果。也可取消本次登录。
        </p>
      ) : session.data?.connected ? (
        <>
          <p>
            {account
              ? `${account.username || account.email} · ${account.email}`
              : profile.isFetching
                ? "正在读取账户资料"
                : "账户资料暂不可用"}
          </p>
          <div className="osir-stats">
            <article>
              <span>账户余额</span>
              <strong>
                {account && Number.isFinite(account.balance)
                  ? account.balance.toLocaleString("en-US", {
                      minimumFractionDigits: 2,
                      maximumFractionDigits: 2,
                    })
                  : "—"}
              </strong>
              <small>以 OSIR 服务端返回值为准</small>
            </article>
          </div>
          {profile.isError && (
            <p role="alert">
              账户资料刷新失败，请重试。
              {account && " · 当前显示最近成功读取的资料，余额可能已变化。"}
            </p>
          )}
          {profile.data?.generation === session.data.generation && (
            <p className="osir-muted">最近同步：{profile.data.fetched_at}</p>
          )}
          <button
            type="button"
            disabled={profile.isFetching || busy}
            onClick={() => void profile.refetch()}
          >
            {profile.isFetching ? "正在刷新资料…" : "刷新资料与余额"}
          </button>
          <p>退出账户会停止同步；已连接工具的配置仍可能继续产生调用费用。</p>
        </>
      ) : savedSession ? (
        <p role="status">
          {restoring
            ? "正在恢复已保存的登录，请稍候。"
            : recovery === "blocked"
              ? "已保存的登录无法恢复，请重新授权。"
              : recovery === "unlock_required"
                ? "已保存的登录暂未恢复。可重试恢复或重新授权。"
                : "账户服务暂时不可用，已保留登录信息。请检查网络后重试恢复。"}
        </p>
      ) : (
        <p>
          通过 OSIR
          网站授权，查看余额、密钥、分组、订阅和用量。零余额或无订阅仍可登录；登录不会自动创建密钥或修改工具配置。
        </p>
      )}
      {!available && !session.data?.connected && !waiting && !savedSession && (
        <p className="osir-auth-warning">
          {reason || "账户授权服务暂不可用，请稍后重试。"}
          <button
            type="button"
            onClick={() =>
              void query.invalidateQueries({
                queryKey: ["osir-auth-capability"],
              })
            }
          >
            重新检测授权服务
          </button>
        </p>
      )}
      {session.isError && (
        <p role="alert">
          账户状态读取失败，请重试后再登录。{" "}
          <button
            type="button"
            disabled={session.isFetching}
            onClick={() => void session.refetch()}
          >
            重试读取账户状态
          </button>
        </p>
      )}
      {!desktop && (
        <p className="osir-muted">当前为网页预览，请在桌面客户端登录账户。</p>
      )}
      {desktop && session.isPending && <p role="status">正在读取账户状态…</p>}
      {message && <p role={failed ? "alert" : "status"}>{message}</p>}
      {!session.data?.connected && !waiting && !savedSession && (
        <label>
          <input
            type="checkbox"
            checked={allowKeyCreation}
            disabled={!canAuthorize}
            onChange={(event) => setAllowKeyCreation(event.target.checked)}
          />
          允许在客户端创建密钥（可选，浏览器中再次确认）
        </label>
      )}
      <div className="osir-auth-actions">
        <button type="button" onClick={() => void website()}>
          打开网站 / 充值
        </button>
        {waiting ? (
          <button type="button" disabled={busy} onClick={() => void cancel()}>
            {busy ? "正在取消…" : "取消授权"}
          </button>
        ) : session.data?.connected || recovery === "blocked" ? (
          <button
            type="button"
            disabled={busy || reauthorizeGeneration !== null}
            onClick={() => void logout()}
          >
            {busy ? "正在退出…" : "退出账户"}
          </button>
        ) : savedSession ? (
          <button
            type="button"
            disabled={
              busy ||
              restoring ||
              session.isError ||
              reauthorizeGeneration !== null
            }
            onClick={() => void retrySaved()}
          >
            {restoring ? "正在恢复登录…" : "恢复登录"}
          </button>
        ) : (
          <button
            type="button"
            className="primary"
            disabled={!canAuthorize}
            onClick={() => void authorize()}
          >
            {failed ? "重试账户授权" : "使用浏览器授权"}
          </button>
        )}
        {reauthorizeButton}
      </div>
      {reauthorizeDialog}
    </section>
  );
}
