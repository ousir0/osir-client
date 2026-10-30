import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Download, LoaderCircle, RefreshCw } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useUpdate } from "@/contexts/UpdateContext";
import { settingsApi } from "@/lib/api";
import { discardUnsavedChanges, hasUnsavedChanges } from "@/lib/unsavedChanges";
import type { UpdateInfo } from "@/lib/updater";
import { extractErrorMessage } from "@/utils/errorUtils";
import "./client-update.css";

type Phase = "ready" | "downloading" | "restarting" | "current";
interface DownloadProgress {
  downloaded: number;
  total: number | null;
}

/** Presentational surface also used by the isolated visual acceptance fixture. */
export function OsirClientUpdateDialogView({
  open,
  info,
  phase,
  progress,
  error,
  confirmUnsaved,
  onInstall,
  onDismiss,
}: {
  open: boolean;
  info: UpdateInfo;
  phase: Phase;
  progress: DownloadProgress | null;
  error: string | null;
  confirmUnsaved: boolean;
  onInstall: () => void;
  onDismiss: () => void;
}) {
  const pending = phase === "downloading" || phase === "restarting";
  const percent = progress?.total
    ? Math.min(100, Math.round((progress.downloaded / progress.total) * 100))
    : undefined;
  const progressLabel =
    phase === "restarting"
      ? "更新已安装，正在重启客户端…"
      : percent === 100
        ? "下载完成，正在校验并安装…"
        : percent === undefined
          ? "正在准备下载更新…"
          : `正在下载更新 ${percent}%`;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => !next && !pending && onDismiss()}
    >
      <DialogContent
        className="osir-client-update-dialog"
        zIndex="top"
        onEscapeKeyDown={(event) => {
          if (pending) event.preventDefault();
        }}
      >
        <DialogHeader>
          <DialogTitle>
            {confirmUnsaved ? "放弃未保存修改并更新？" : "OSIR Client 有新版本"}
          </DialogTitle>
          <DialogDescription>
            {confirmUnsaved
              ? "更新完成后将重启客户端，未保存的修改会丢失。请先返回保存，或确认放弃修改后继续。"
              : "安装更新后，客户端将自动重新启动。"}
          </DialogDescription>
        </DialogHeader>
        <div className="osir-client-update-body">
          <dl className="osir-client-update-versions">
            <div>
              <dt>当前版本</dt>
              <dd>{info.currentVersion || "未能读取"}</dd>
            </div>
            <div>
              <dt>最新版本</dt>
              <dd>{info.availableVersion}</dd>
            </div>
          </dl>
          {!confirmUnsaved && (
            <section className="osir-client-update-notes" aria-label="更新说明">
              <h3>更新说明</h3>
              <p>{info.notes?.trim() || "此版本暂未提供更新说明。"}</p>
            </section>
          )}
          {pending && (
            <div
              className="osir-client-update-progress"
              role="status"
              aria-live="polite"
            >
              <span>
                <LoaderCircle
                  size={15}
                  className="animate-spin"
                  aria-hidden="true"
                />
                {progressLabel}
              </span>
              <progress aria-label="更新下载进度" max={100} value={percent} />
            </div>
          )}
          {phase === "current" && (
            <p role="status">当前没有可安装的新版本，可稍后重新检查。</p>
          )}
          {error && (
            <p role="alert" className="osir-client-update-error">
              {error}
            </p>
          )}
        </div>
        <DialogFooter>
          <button type="button" disabled={pending} onClick={onDismiss}>
            {confirmUnsaved
              ? "返回保存"
              : phase === "current"
                ? "关闭"
                : "稍后"}
          </button>
          {phase !== "current" && (
            <button
              type="button"
              className="primary"
              disabled={pending}
              onClick={onInstall}
            >
              {error ? (
                <RefreshCw size={15} aria-hidden="true" />
              ) : (
                <Download size={15} aria-hidden="true" />
              )}
              {pending
                ? "正在更新…"
                : confirmUnsaved
                  ? "放弃修改并更新"
                  : error
                    ? "重试更新"
                    : "更新并重启"}
            </button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** One native installation owner, independent from the account/login lifecycle. */
export function OsirClientUpdateDialog() {
  const { distribution, hasUpdate, updateInfo, isDismissed, dismissUpdate } =
    useUpdate();
  const [phase, setPhase] = useState<Phase>("ready");
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmUnsaved, setConfirmUnsaved] = useState(false);
  const installing = useRef(false);
  const mounted = useRef(false);
  const unlisten = useRef<(() => void) | null>(null);
  const enabled =
    "__TAURI_INTERNALS__" in window &&
    distribution?.kind === "osir" &&
    distribution.updatesEnabled;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      unlisten.current?.();
      unlisten.current = null;
    };
  }, []);

  useEffect(() => {
    if (installing.current) return;
    setPhase("ready");
    setError(null);
    setProgress(null);
    setConfirmUnsaved(false);
  }, [updateInfo?.availableVersion]);

  async function install(approvedDiscard: boolean) {
    if (!enabled || !hasUpdate || !updateInfo || installing.current) return;
    if (hasUnsavedChanges() && !approvedDiscard) {
      setConfirmUnsaved(true);
      return;
    }
    installing.current = true;
    setConfirmUnsaved(false);
    setPhase("downloading");
    setProgress(null);
    setError(null);
    let awaitingRestart = false;
    try {
      const stop = await listen<DownloadProgress>(
        "update-download-progress",
        ({ payload }) => {
          if (
            !mounted.current ||
            !installing.current ||
            !payload ||
            !Number.isFinite(payload.downloaded) ||
            payload.downloaded < 0
          )
            return;
          setProgress({
            downloaded: payload.downloaded,
            total:
              typeof payload.total === "number" &&
              Number.isFinite(payload.total) &&
              payload.total > 0
                ? payload.total
                : null,
          });
        },
      );
      if (!mounted.current) {
        stop();
        return;
      }
      unlisten.current = stop;
      const installed = await settingsApi.installUpdateAndRestart();
      if (!mounted.current) return;
      if (installed) {
        // Keep drafts registered if download/install fails; only a successful
        // restart can consume the user's explicit permission to discard them.
        if (approvedDiscard) discardUnsavedChanges();
        awaitingRestart = true;
        setPhase("restarting");
      } else {
        setPhase("current");
      }
    } catch (failure) {
      if (mounted.current) {
        setPhase("ready");
        setError(`更新失败：${extractErrorMessage(failure) || "请稍后重试。"}`);
      }
    } finally {
      unlisten.current?.();
      unlisten.current = null;
      // A successful install awaits native restart and must not run twice.
      installing.current = awaitingRestart;
    }
  }

  if (!enabled || !updateInfo) return null;
  const pending = phase === "downloading" || phase === "restarting";
  return (
    <OsirClientUpdateDialogView
      open={pending || (hasUpdate && !isDismissed)}
      info={updateInfo}
      phase={phase}
      progress={progress}
      error={error}
      confirmUnsaved={confirmUnsaved}
      onInstall={() => {
        if (!pending) void install(confirmUnsaved);
      }}
      onDismiss={() => {
        if (pending) return;
        setConfirmUnsaved(false);
        dismissUpdate();
      }}
    />
  );
}
