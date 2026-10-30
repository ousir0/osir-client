import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { toast } from "@/lib/toast";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  toolConfigBackupsApi as api,
  type BackupTool,
  type ToolBackupPreview,
  type ToolBackupRestoreReport,
  type ToolBackupState,
} from "./toolConfigBackupsApi";
import "./tool-config-backups.css";

const tools: Record<BackupTool, string> = {
  codex: "Codex",
  claude: "Claude Code",
  opencode: "OpenCode",
  gemini: "Gemini CLI",
};
const errors: Record<string, string> = {
  changed: "配置已被外部修改，本次恢复已拒绝。请重新预览后再确认。",
  previewExpired: "预览已过期或已使用，请重新预览。",
  pendingRecovery: "上次恢复尚未完成，请先处理恢复状态。",
  toolBusy: "工具正在切换配置、代理仍在使用或有未完成操作。请先停止相关操作。",
  unsafePath: "配置路径或文件权限不符合安全要求，操作已停止。",
  invalidSnapshot: "快照无效或不属于此工具，操作已停止。",
  tooLarge: "配置文件超过安全大小限制，操作已停止。",
  limitReached: "本地快照或预览已达到上限，操作未执行。",
  invalidConfirmation: "请先确认已关闭工具并保存外部修改。",
};
function errorMessage(error: unknown): string {
  let value = error;
  if (typeof value === "string") {
    try {
      value = JSON.parse(value);
    } catch {
      value = null;
    }
  }
  const code =
    value && typeof value === "object" && "code" in value
      ? String(value.code)
      : "";
  return (
    errors[code] ?? "操作结果未确认，请刷新快照与恢复状态；不要直接重复恢复。"
  );
}
function size(bytes: number) {
  return bytes < 1024 ? bytes + " B" : (bytes / 1024).toFixed(1) + " KB";
}
function time(value: string) {
  return Number.isNaN(Date.parse(value))
    ? "时间不可用"
    : new Date(value).toLocaleString("zh-CN", { hour12: false });
}
function hash(value: string | null) {
  return value ?? "文件不存在";
}

export interface ToolConfigBackupsProps {
  /** Omit to show the four-tool selector. */
  tool?: BackupTool;
  disabled?: boolean;
  onBlockedChange?: (blocked: boolean) => void;
  /** Host may invalidate provider/model queries after restore or recovery. */
  onRestored?: (tool: BackupTool, report: ToolBackupRestoreReport) => void;
}
export function ToolConfigBackups({ tool, ...props }: ToolConfigBackupsProps) {
  const [selected, setSelected] = useState<BackupTool>(tool ?? "codex");
  return (
    <ToolBackupPanel
      key={tool ?? selected}
      {...props}
      tool={tool ?? selected}
      onToolChange={tool ? undefined : setSelected}
    />
  );
}
function ToolBackupPanel({
  tool,
  disabled = false,
  onBlockedChange,
  onRestored,
  onToolChange,
}: Omit<ToolConfigBackupsProps, "tool"> & {
  tool: BackupTool;
  onToolChange?: (tool: BackupTool) => void;
}) {
  const [state, setState] = useState<ToolBackupState | null>(null);
  const [preview, setPreview] = useState<ToolBackupPreview | null>(null);
  const [recovering, setRecovering] = useState(false);
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [reading, setReading] = useState(false);
  const [error, setError] = useState("");
  const [report, setReport] = useState<ToolBackupRestoreReport | null>(null);
  const lock = useRef(false);
  const readLock = useRef(false);
  const mounted = useRef(true);
  const blockedCallback = useRef(onBlockedChange);
  blockedCallback.current = onBlockedChange;
  const blocked = busy || !!preview || recovering;
  const controlsBlocked = blocked || reading;
  useEffect(() => {
    blockedCallback.current?.(blocked);
  }, [blocked]);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      blockedCallback.current?.(false);
    };
  }, []);
  const reload = async () => {
    const next = await api.list(tool);
    if (mounted.current) setState(next);
  };
  const refreshList = async (afterWrite = false) => {
    if (disabled || lock.current || readLock.current || !mounted.current)
      return;
    readLock.current = true;
    setReading(true);
    if (!afterWrite) setError("");
    try {
      await reload();
    } catch (e) {
      if (mounted.current)
        setError((previous) =>
          afterWrite
            ? previous || "操作后的状态读取失败，请刷新确认；不要直接重复恢复。"
            : errorMessage(e),
        );
    } finally {
      readLock.current = false;
      if (mounted.current) setReading(false);
    }
  };
  const run = async (operation: () => Promise<void>, refresh = false) => {
    if (disabled || lock.current || readLock.current || !mounted.current)
      return;
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      await operation();
    } catch (e) {
      if (mounted.current) setError(errorMessage(e));
      else if (refresh) toast.error(errorMessage(e));
    } finally {
      lock.current = false;
      if (mounted.current) setBusy(false);
      // The native write has settled. Its follow-up list read keeps only local
      // controls busy, so a slow refresh cannot retain the host navigation lock.
      if (refresh && mounted.current) await refreshList(true);
    }
  };
  useEffect(() => {
    if (!disabled) void refreshList();
  }, [tool, disabled]);
  const close = () => {
    if (!lock.current) {
      setPreview(null);
      setRecovering(false);
      setAcknowledged(false);
      setError("");
    }
  };
  const finish = (result: ToolBackupRestoreReport) => {
    if (mounted.current) {
      setReport(result);
      setPreview(null);
      setRecovering(false);
      setAcknowledged(false);
    }
    if (result.status === "restored" || result.status === "noChanges") {
      toast.success(
        result.status === "restored"
          ? "配置文件已恢复，客户端当前连接选择未改变。"
          : "配置文件与快照一致，客户端当前连接选择未改变。",
      );
    } else if (!mounted.current) {
      toast.warning(
        result.status === "rolledBack"
          ? "未完成快照恢复，已回滚至恢复前配置。请到备份与恢复页面核对。"
          : "工具配置恢复尚未完成，已保留安全快照与恢复记录。请到备份与恢复页面处理。",
      );
    }
    try {
      onRestored?.(tool, result);
    } catch {
      const message = "配置处理已结束，但页面数据刷新失败，请重新打开此页。";
      if (mounted.current) setError(message);
      else toast.warning(message);
    }
  };
  return (
    <section
      className="osir-tool-backups"
      aria-label="逐工具配置快照"
      aria-busy={busy || reading}
    >
      <header className="osir-tool-backups-heading">
        <div>
          <h2>工具配置快照</h2>
          <p>保存各工具的配置文件，恢复前可预览差异。</p>
        </div>
        <Button
          className="osir-tool-backups-primary"
          disabled={
            disabled || controlsBlocked || !state || state.pendingRecovery
          }
          onClick={() =>
            void run(async () => {
              await api.create(tool);
              toast.success("已创建工具配置快照。");
            }, true)
          }
        >
          创建快照
        </Button>
      </header>
      <div className="osir-tool-backups-toolbar">
        {onToolChange ? (
          <label>
            工具
            <select
              aria-label="快照工具"
              value={tool}
              disabled={disabled || controlsBlocked}
              onChange={(e) => onToolChange(e.target.value as BackupTool)}
            >
              {Object.entries(tools).map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </select>
          </label>
        ) : (
          <strong>{tools[tool]}</strong>
        )}
        <Button
          variant="outline"
          disabled={disabled || controlsBlocked}
          onClick={() => void refreshList()}
        >
          刷新
        </Button>
      </div>
      <p className="osir-tool-backups-note">
        仅恢复列出的工具配置文件，不包含会话、项目或登录凭据。已保存的供应商和当前选择不会改变，恢复后请在工具页重新应用连接。备份可能包含密钥，请妥善保管；恢复前请关闭相关工具。
      </p>
      {busy && <p role="status">正在处理…</p>}
      {reading && <p role="status">正在读取快照…</p>}
      {error && (
        <p role="alert" className="osir-tool-backups-error">
          {error}
        </p>
      )}
      {report && (
        <div className="osir-tool-backups-report">
          {(report.status === "restored" || report.status === "noChanges") && (
            <p role="status">
              {report.status === "restored"
                ? "已恢复快照中的配置文件。"
                : "配置文件与快照一致。"}
              客户端已保存的连接和当前选择保持原样，工具实际配置可能与页面显示不同。
              {tool === "opencode"
                ? "请到 OpenCode 页面核对目标供应商与默认模型；启用供应商和选择默认模型是分别设置的。"
                : `请到 ${tools[tool]} 页面重新选择并应用目标连接，再打开工具核对。`}
            </p>
          )}
          {report.status === "rolledBack" && (
            <p role="alert">未完成快照恢复，已安全回滚至恢复前配置。</p>
          )}
          {report.status === "recoveryRequired" && (
            <p role="alert">
              恢复尚未完成，已保留安全快照与恢复记录。外部修改不会被强行覆盖。
            </p>
          )}
          {report.safetySnapshotId && (
            <p>
              安全快照：<code>{report.safetySnapshotId}</code>
            </p>
          )}
          {report.conflicts.length > 0 && (
            <>
              <p>
                以下路径存在冲突或暂不可写，请保存外部修改并处理后重试恢复原状：
              </p>
              <ul>
                {report.conflicts.map((path) => (
                  <li key={path}>
                    <code>{path}</code>
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
      {state?.pendingRecovery && (
        <div className="osir-tool-backups-recovery">
          <p>
            有未完成的恢复记录。将按安全快照恢复原状，遇到外部修改时停止覆盖并保留恢复记录。
          </p>
          <Button
            variant="outline"
            disabled={disabled || controlsBlocked}
            onClick={() => {
              setRecovering(true);
              setAcknowledged(false);
              setError("");
            }}
          >
            恢复原状
          </Button>
        </div>
      )}
      {!state ? (
        !busy && !reading && <p>快照记录暂不可用，请刷新重试。</p>
      ) : state.snapshots.length === 0 ? (
        <p className="osir-tool-backups-empty">此工具还没有配置快照。</p>
      ) : (
        <ul className="osir-tool-backups-list">
          {state.snapshots.map((snapshot) => (
            <li key={snapshot.id}>
              <div className="osir-tool-backups-row">
                <div>
                  <strong>
                    {snapshot.safety ? "恢复前安全快照" : "手动快照"}
                  </strong>
                  <time dateTime={snapshot.createdAt}>
                    {time(snapshot.createdAt)}
                  </time>
                </div>
                <span>
                  {size(
                    snapshot.files.reduce(
                      (sum, file) => sum + file.sizeBytes,
                      0,
                    ),
                  )}
                </span>
                <Button
                  variant="outline"
                  disabled={
                    disabled || controlsBlocked || state.pendingRecovery
                  }
                  onClick={() =>
                    void run(async () => {
                      const next = await api.preview(tool, snapshot.id);
                      if (mounted.current) {
                        setPreview(next);
                        setAcknowledged(false);
                        setReport(null);
                      }
                    })
                  }
                >
                  预览恢复
                </Button>
              </div>
              <details>
                <summary>配置文件范围</summary>
                {snapshot.files.map((file) => (
                  <div className="osir-tool-backups-file" key={file.path}>
                    <code>{file.path}</code>
                    <span>{size(file.sizeBytes)}</span>
                    <code>SHA-256：{hash(file.hash)}</code>
                  </div>
                ))}
              </details>
            </li>
          ))}
        </ul>
      )}
      <Dialog
        open={!!preview || recovering}
        onOpenChange={(open) => {
          if (!open) close();
        }}
      >
        <DialogContent className="osir-tool-backups-dialog" zIndex="alert">
          <DialogHeader>
            <DialogTitle>
              {recovering ? "恢复到操作前配置" : "恢复工具配置"}
            </DialogTitle>
            <DialogDescription>
              {tools[tool]} ·{" "}
              {recovering
                ? "恢复到操作前的配置。文件若已被其它程序修改，将暂停恢复。"
                : "仅恢复下列文件；恢复前会自动备份当前配置。"}
            </DialogDescription>
          </DialogHeader>
          <div className="osir-tool-backups-body">
            {preview?.files.map((file) => (
              <div className="osir-tool-backups-diff" key={file.path}>
                <code>{file.path}</code>
                <strong>{file.changed ? "将变更" : "无变化"}</strong>
                <dl>
                  <dt>当前</dt>
                  <dd>
                    {size(file.currentSize)}
                    <code>{hash(file.currentHash)}</code>
                  </dd>
                  <dt>快照</dt>
                  <dd>
                    {size(file.snapshotSize)}
                    <code>{hash(file.snapshotHash)}</code>
                  </dd>
                </dl>
              </div>
            ))}
            <p>
              已保存的供应商和当前选择不会改变。恢复后请在工具页重新应用连接，并检查默认模型。若文件已被其它程序修改，恢复会暂停，并保留备份供后续处理。
            </p>
            {error && (
              <p role="alert" className="osir-tool-backups-error">
                {error}
              </p>
            )}
            <label className="osir-tool-backups-ack">
              <Checkbox
                checked={acknowledged}
                disabled={busy}
                onCheckedChange={setAcknowledged}
              />
              <span>我已保存修改并关闭相关工具。</span>
            </label>
          </div>
          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={close}>
              取消
            </Button>
            <Button
              className="osir-tool-backups-primary"
              disabled={
                disabled ||
                busy ||
                !acknowledged ||
                (!recovering && !preview?.files.some((file) => file.changed))
              }
              onClick={() =>
                void run(async () => {
                  const current = preview;
                  if (!recovering && !current) return;
                  try {
                    finish(
                      recovering
                        ? await api.recover(tool)
                        : await api.restore(tool, current!.previewId, true),
                    );
                  } catch (e) {
                    if (mounted.current) {
                      setPreview(null);
                      setRecovering(false);
                      setAcknowledged(false);
                    }
                    throw e;
                  }
                }, true)
              }
            >
              {busy ? "正在恢复…" : recovering ? "确认恢复原状" : "确认恢复"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
