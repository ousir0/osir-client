import { useRef, useState } from "react";
import { proxyApi } from "@/lib/api";
import type { ProxyStack, ProxyStackNotice } from "@/types/proxy";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { connectionError } from "./errors";
import "./catalog-status.css";

const messages: Record<ProxyStackNotice, string> = {
  routeOwnsCatalog: "当前连接使用独立模型目录，聚合模型暂不可见。",
  officialModelsBundled: "官方模型暂时使用本机内置列表，可能缺少账号专属模型。",
  officialModelsUnavailable:
    "官方模型列表不可用，请检查 Codex 登录状态后刷新。",
  officialModelsOutdated: "Codex 版本过旧，请到安装与更新升级后刷新模型。",
};
type Props = {
  stack: ProxyStack;
  disabled: boolean;
  onAction: (
    operation: () => Promise<unknown>,
    success: string,
  ) => Promise<boolean | undefined>;
};
type Operation = "adopt" | "restart" | "acknowledge";

export function CodexCatalogStatus({ stack, disabled, onAction }: Props) {
  const [confirmation, setConfirmation] = useState<"adopt" | "restart" | null>(
    null,
  );
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const lock = useRef(false);
  const stale = stack.staleClients;
  const hasStale = !!(stale?.daemon || stale?.others || stale?.unverified);
  const canAdopt = stack.active && stack.notice === "routeOwnsCatalog";

  async function run(kind: Operation) {
    if (disabled || lock.current) return;
    if (kind === "adopt" && !canAdopt) return;
    if (kind === "restart" && !stale?.daemon) return;
    if (kind === "acknowledge" && !stale?.unverified) return;
    lock.current = true;
    setPending(true);
    setError("");
    try {
      const ok = await onAction(
        async () => {
          try {
            if (kind === "adopt") {
              const notice = await proxyApi.adoptCodexStackCatalog();
              if (notice === "routeOwnsCatalog")
                throw new Error(messages[notice]);
              return notice ? messages[notice] : undefined;
            } else if (kind === "restart") {
              const outcome = await proxyApi.restartCodexAppServerDaemon();
              return outcome === "notRunning"
                ? "未检测到运行中的命令行后台进程，无需重启。"
                : "命令行后台进程已重启。";
            } else {
              await proxyApi.acknowledgeCodexStaleClients();
            }
          } catch (cause) {
            setError(connectionError(cause));
            throw cause;
          }
        },
        kind === "adopt"
          ? "已应用聚合模型目录，请重新打开 Codex 读取。"
          : kind === "restart"
            ? "命令行后台进程状态已更新，请重新打开 Codex 命令行核对模型。"
            : "已记录本次重新打开确认，请在 Codex 中核对模型是否可选。",
      );
      if (ok) setConfirmation(null);
    } catch (cause) {
      setError(connectionError(cause));
    } finally {
      lock.current = false;
      setPending(false);
    }
  }
  function ask(kind: "adopt" | "restart") {
    setError("");
    setConfirmation(kind);
  }
  if (!stack.notice && !hasStale) return null;

  return (
    <section className="osir-catalog-status" aria-label="Codex 模型目录状态">
      {stack.notice && (
        <div className="osir-catalog-status-row">
          <p role="status">{messages[stack.notice]}</p>
          {canAdopt && (
            <button disabled={disabled || pending} onClick={() => ask("adopt")}>
              使用聚合模型目录
            </button>
          )}
        </div>
      )}
      {hasStale && (
        <div className="osir-catalog-status-row">
          <div role="status">
            <strong>
              {stale?.auth ? "Codex 登录状态已变化" : "Codex 模型目录已更新"}
            </strong>
            {stale?.daemon && (
              <p>命令行后台进程仍在使用旧目录，重启后读取更新。</p>
            )}
            {stale?.others && (
              <p>请彻底退出并重新打开 Codex 桌面版或编辑器插件。</p>
            )}
            {stale?.unverified && (
              <p>暂时无法确认客户端是否已重新打开，请手动重开后核对模型。</p>
            )}
          </div>
          <div className="osir-catalog-actions">
            {stale?.daemon && (
              <button
                disabled={disabled || pending}
                onClick={() => ask("restart")}
              >
                重启命令行后台进程
              </button>
            )}
            {stale?.unverified && (
              <button
                disabled={disabled || pending}
                onClick={() => void run("acknowledge")}
              >
                我已重新打开
              </button>
            )}
          </div>
        </div>
      )}
      {error && !confirmation && <p role="alert">{error}</p>}
      <Dialog
        open={confirmation !== null}
        onOpenChange={(open) => {
          if (!open && !pending) setConfirmation(null);
        }}
      >
        <DialogContent
          className="osir-drawer osir-catalog-confirm"
          onEscapeKeyDown={(event) => {
            if (pending) event.preventDefault();
          }}
        >
          <DialogHeader>
            <DialogTitle>
              {confirmation === "adopt"
                ? "使用聚合模型目录？"
                : "重启命令行后台进程？"}
            </DialogTitle>
            <DialogDescription>
              {confirmation === "adopt"
                ? "将当前连接的独立模型目录切换为已配置供应商的聚合目录。完成后需要重新打开 Codex，才能读取模型列表。"
                : "这会中断 Codex 命令行后台进程中正在运行的任务。请先保存工作，完成后重新打开命令行；桌面版和编辑器插件需要单独退出再打开。"}
            </DialogDescription>
          </DialogHeader>
          {error && <p role="alert">{error}</p>}
          <DialogFooter>
            <button disabled={pending} onClick={() => setConfirmation(null)}>
              取消
            </button>
            <button
              className="primary"
              disabled={disabled || pending}
              onClick={() => {
                if (confirmation) void run(confirmation);
              }}
            >
              {pending
                ? "正在处理…"
                : confirmation === "adopt"
                  ? "确认切换"
                  : "确认重启"}
            </button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
