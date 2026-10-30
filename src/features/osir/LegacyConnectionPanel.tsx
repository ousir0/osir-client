import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { settingsApi } from "@/lib/api";
import { legacyIdentity, useLegacySnapshot } from "./legacySnapshot";

export type LegacyConnectionPanelProps = {
  /** Native adapter must discard the DTO; keys/config bundles must never reach React. */
  authorize: () => Promise<void>;
  /** Supply only when native cancellation stops the pending flow and prevents import. */
  cancel?: () => Promise<void>;
  available?: boolean;
  reason?: string;
  onSuccess?: () => void | Promise<void>;
};

/** Subscription installer grant only. This never changes the account-session cache. */
export function LegacyConnectionPanel({
  authorize,
  cancel,
  available = true,
  onSuccess,
}: LegacyConnectionPanelProps) {
  const query = useQueryClient();
  const mounted = useRef(false);
  const running = useRef(false);
  const cancelling = useRef(false);
  const generation = useRef(0);
  const [pending, setPending] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [failed, setFailed] = useState(false);
  const desktop =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

  const [reviewed, setReviewed] = useState(false);
  const recovery = useQuery({
    queryKey: ["osir-legacy-recovery"],
    queryFn: () => invoke<string>("osir_legacy_recovery_status"),
    enabled: desktop && available,
    retry: false,
    retryOnMount: false,
    refetchOnWindowFocus: false,
  });
  const needsRecovery =
    recovery.data === "pending" || recovery.data === "unknown";
  const snapshot = useLegacySnapshot(desktop && available);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    setReviewed(false);
  }, [recovery.data, recovery.dataUpdatedAt]);

  async function connect(resume = false) {
    if (
      running.current ||
      cancelling.current ||
      busy ||
      !desktop ||
      !available ||
      recovery.isFetching ||
      recovery.isError ||
      (resume ? recovery.data !== "pending" : recovery.data !== "clear")
    )
      return;
    const id = ++generation.current;
    running.current = true;
    setPending(true);
    setFailed(false);
    setMessage("");
    try {
      if (resume) await invoke("osir_retry_legacy_connect");
      else await authorize();
      if (generation.current !== id) return;
      running.current = false;
      // Refresh even if the panel was unmounted while native import completed.
      const refreshed = await Promise.allSettled([
        query.invalidateQueries({ queryKey: ["osir-providers"] }),
        query.invalidateQueries({ queryKey: ["providers"] }),
        query.invalidateQueries({ queryKey: ["osir-legacy-snapshot"] }),
      ]);
      if (mounted.current) {
        setMessage(
          refreshed.some((result) => result.status === "rejected")
            ? "订阅连接已完成，供应商列表刷新失败，请重新打开工具页面。"
            : "订阅连接已完成，请到工具页面查看已保存的供应商。账户登录状态未改变；使用工具前请到工具页切换供应商。",
        );
      }
      if (onSuccess) {
        try {
          await onSuccess();
        } catch {
          if (mounted.current)
            setMessage("订阅连接已完成，页面刷新失败，请重新打开工具页面。");
        }
      }
    } catch (error) {
      // Only fixed native messages are permitted; never display arbitrary response data.
      const safeErrors: Record<string, string> = {
        已有连接正在进行: "已有订阅连接正在处理，请等待完成。",
        无法启动本机授权回调: "无法开始订阅连接，请稍后重试。",
        无法打开授权浏览器: "无法打开系统浏览器，请检查后重试。",
        "连接已取消或超时，请重新授权":
          "订阅授权已取消或超时。确认恢复状态后，可重新发起连接。",
        网站未批准此次连接: "网站未批准此次订阅连接。需要连接时可重新授权。",
        旧版连接服务不可用或授权已失效:
          "订阅连接暂不可用或授权已失效，请稍后重试。",
      };
      if (mounted.current && generation.current === id) {
        setFailed(true);
        setMessage(
          typeof error === "string" &&
            Object.prototype.hasOwnProperty.call(safeErrors, error)
            ? safeErrors[error]
            : "订阅连接未确认完成。请先刷新恢复状态，核对网站密钥和本机供应商；不要重复授权或创建密钥。",
        );
      }
    } finally {
      await recovery.refetch();
      if (generation.current === id) {
        running.current = false;
        if (mounted.current) setPending(false);
      }
    }
  }

  async function stop() {
    if (!cancel || !running.current || cancelling.current) return;
    cancelling.current = true;
    setBusy(true);
    const id = generation.current;
    try {
      await cancel();
      // A completed authorization must not be relabeled as cancelled.
      if (generation.current === id && running.current) {
        ++generation.current;
        running.current = false;
        if (mounted.current) {
          setPending(false);
          setFailed(false);
          setMessage("已取消订阅授权。请刷新恢复状态，确认后再发起新连接。");
        }
      }
    } catch {
      if (mounted.current && generation.current === id && running.current)
        setMessage("未能确认取消，请在浏览器拒绝授权，或等待本次授权结束。");
    } finally {
      await recovery.refetch();
      cancelling.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  return (
    <section
      className="osir-auth-card"
      aria-label="订阅接入"
      aria-busy={pending || busy}
    >
      <h2>连接已有订阅</h2>
      <p>
        在浏览器中授权后，会将订阅连接保存到本机，并按需创建或修复订阅密钥。
      </p>
      <p>
        此操作需要有效订阅。查看账户余额与用量，请使用账户登录；没有订阅或余额也可登录。
      </p>
      {snapshot.isError && <p role="status">上次订阅接入记录暂不可用。</p>}
      {snapshot.data && (
        <div aria-label="上次订阅接入记录">
          <h3>上次订阅接入记录</h3>
          <p>{legacyIdentity(snapshot.data)}</p>
          <p>记录时间：{snapshot.data.captured_at}</p>
          <p>
            这是上次接入时的记录，不代表订阅当前仍有效，也不表示账户已登录。
          </p>
          {snapshot.data.subscriptions.length === 0 ? (
            <p>上次接入记录未包含订阅明细。</p>
          ) : (
            <ul>
              {snapshot.data.subscriptions.map((subscription) => (
                <li key={subscription.id}>
                  {subscription.group_name || "未命名订阅"} ·{" "}
                  {subscription.status}
                  {subscription.expires_at && (
                    <span> · 到期记录：{subscription.expires_at}</span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {pending && (
        <p role="status">
          正在等待浏览器确认或保存工具供应商。完成后会更新结果，请勿重复提交或关闭客户端。
        </p>
      )}
      {!desktop && <p>当前为网页预览，请在桌面客户端连接订阅。</p>}
      {!available && <p>订阅接入暂不可用，请稍后重试。</p>}
      {message && <p role={failed ? "alert" : "status"}>{message}</p>}
      {desktop && available && recovery.isFetching && !pending && (
        <p role="status">正在检查上次连接状态…</p>
      )}
      {recovery.isError && (
        <p role="status">
          无法读取连接恢复状态，请刷新后再试。
          <button
            type="button"
            disabled={pending || busy || recovery.isFetching}
            onClick={() => void recovery.refetch()}
          >
            刷新状态
          </button>
        </p>
      )}
      {needsRecovery && (
        <div aria-label="连接恢复">
          <p>上次连接结果尚未确认。优先继续原连接，避免重复创建密钥。</p>
          <button
            type="button"
            disabled={pending || busy || recovery.isFetching}
            onClick={() => void recovery.refetch()}
          >
            刷新恢复状态
          </button>
          <button
            type="button"
            disabled={pending || busy || recovery.isFetching}
            onClick={() =>
              void settingsApi
                .openExternal("https://osirclaw.com")
                .catch(() => {
                  setMessage(
                    "无法打开网站，请手动访问 https://osirclaw.com 核对密钥和订阅。",
                  );
                })
            }
          >
            打开 OSIR 网站核对
          </button>
          {recovery.data === "unknown" && (
            <p>
              无法继续上次连接。请先核对网站密钥与本机供应商，再决定是否重新授权。
            </p>
          )}
          {recovery.data === "pending" && (
            <button
              type="button"
              disabled={pending || busy || recovery.isFetching}
              onClick={() => void connect(true)}
            >
              继续上次连接
            </button>
          )}
          <p>
            如果原连接已过期，请先到 OSIR
            网站核对专用密钥和订阅，再检查本机供应商列表。
          </p>
          <label>
            <input
              type="checkbox"
              checked={reviewed}
              disabled={pending || busy || recovery.isFetching}
              onChange={(e) => setReviewed(e.target.checked)}
            />
            我已核对网站密钥和本机供应商，确认需要重新授权
          </label>
          <button
            type="button"
            disabled={!reviewed || pending || busy || recovery.isFetching}
            onClick={() =>
              void (async () => {
                if (running.current || cancelling.current) return;
                cancelling.current = true;
                setBusy(true);
                try {
                  await invoke("osir_reconcile_legacy_connect", {
                    confirmedCloudReviewed: true,
                  });
                  setReviewed(false);
                  const refreshed = await recovery.refetch();
                  setMessage(
                    refreshed.data === "clear" && !refreshed.isError
                      ? "本机恢复记录已清理，可重新授权。网站密钥未被删除。"
                      : "核对已提交，但恢复状态尚未确认清除。请刷新状态，确认前不要重新授权。",
                  );
                } catch {
                  setMessage("恢复记录尚未清理，请稍后重试。");
                } finally {
                  cancelling.current = false;
                  if (mounted.current) setBusy(false);
                }
              })()
            }
          >
            完成核对，允许重新授权
          </button>
        </div>
      )}
      <div className="osir-auth-actions">
        <button
          type="button"
          className="primary"
          disabled={
            !desktop ||
            !available ||
            pending ||
            busy ||
            recovery.isPending ||
            recovery.isFetching ||
            recovery.data !== "clear" ||
            recovery.isError ||
            needsRecovery
          }
          onClick={() => void connect()}
        >
          {pending
            ? "正在连接订阅…"
            : failed
              ? "重试订阅连接"
              : "使用浏览器连接订阅"}
        </button>
        {pending && cancel && (
          <button type="button" disabled={busy} onClick={() => void stop()}>
            {busy ? "正在取消…" : "取消订阅授权"}
          </button>
        )}
        {pending && !cancel && <p>可在浏览器选择拒绝授权。</p>}
      </div>
    </section>
  );
}
