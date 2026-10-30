import { useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { formatBytes } from "@/components/settings/BackupStorageSection";
import { backupsApi } from "@/lib/api";
import { toast } from "@/lib/toast";
import type { DbRestoreReport } from "@/lib/api/settings";
import {
  BackupRecoveryDialog,
  BackupRecoveryReport,
} from "./BackupRecoveryDialog";
import { connectionError } from "./errors";

export function OsirLocalBackupList({
  disabled,
  onSelectionChange,
}: {
  disabled: boolean;
  onSelectionChange: (selected: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const backups = useQuery({
    queryKey: ["db-backups"],
    queryFn: () => backupsApi.listDbBackups(),
    retry: false,
  });
  const [action, setAction] = useState<{
    kind: "restore" | "rename" | "delete";
    filename: string;
  } | null>(null);
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [refreshWarning, setRefreshWarning] = useState("");
  const [restoreReport, setRestoreReport] = useState<DbRestoreReport | null>(
    null,
  );
  const [locked, setLocked] = useState(false);
  const operationLock = useRef(false);
  const busy = disabled || locked;
  const select = (next: typeof action) => {
    setAction(next);
    setError("");
    if (next?.kind === "rename") setName(next.filename.slice(0, -3));
    onSelectionChange(!!next);
  };
  const run = async <T,>(
    operation: () => Promise<T>,
    success: (result: T) => string,
    restore = false,
  ) => {
    if (busy || operationLock.current) return;
    operationLock.current = true;
    setLocked(true);
    setError("");
    onSelectionChange(true);
    try {
      const result = await operation();
      setAction(null);
      toast.success(success(result));
      try {
        if (restore) await queryClient.invalidateQueries();
        else
          await Promise.all([
            queryClient.invalidateQueries({ queryKey: ["db-backups"] }),
            queryClient.invalidateQueries({ queryKey: ["backup-locations"] }),
          ]);
        setRefreshWarning("");
      } catch {
        setRefreshWarning("页面数据刷新失败，请重新打开此页；无需重复操作。");
      }
      onSelectionChange(false);
    } catch (e) {
      setError(connectionError(e));
      onSelectionChange(!!action);
    } finally {
      operationLock.current = false;
      setLocked(false);
    }
  };
  return (
    <section
      className="osir-backup-records"
      aria-labelledby="osir-backup-records-title"
    >
      <header className="osir-backup-card-heading">
        <div>
          <h2 id="osir-backup-records-title">本地数据备份</h2>
          <p>整份数据库备份 · .db</p>
        </div>
        <Button
          className="osir-backup-primary"
          disabled={busy || !!action}
          onClick={() =>
            void run(
              () => backupsApi.createDbBackup(),
              (result) => "已创建本地数据备份：" + String(result),
            )
          }
        >
          {locked && !action ? "正在处理…" : "立即备份"}
        </Button>
      </header>
      {refreshWarning && (
        <p role="alert" className="osir-backup-note">
          {refreshWarning}
        </p>
      )}
      {restoreReport && (
        <>
          <p role="status" className="osir-backup-note">
            {restoreReport.safetyBackupId
              ? "安全备份：" + restoreReport.safetyBackupId + "。"
              : "未返回安全备份编号。"}
          </p>
          <BackupRecoveryReport report={restoreReport} />
        </>
      )}
      {error && action?.kind !== "restore" && (
        <p role="alert" className="osir-management-feedback">
          {error}
        </p>
      )}
      {backups.isLoading ? (
        <p role="status">正在读取备份记录…</p>
      ) : backups.isError ? (
        <div role="alert" className="osir-management-feedback">
          <p>备份记录读取失败，请重试。</p>
          <Button
            variant="outline"
            disabled={busy || backups.isFetching}
            onClick={() => void backups.refetch()}
          >
            重试
          </Button>
        </div>
      ) : (backups.data ?? []).length === 0 ? (
        <p className="osir-backup-empty">
          还没有本地数据备份。可以立即创建一份。
        </p>
      ) : (
        <div className="osir-backup-table-wrap">
          <table className="osir-backup-table">
            <thead>
              <tr>
                <th>备份文件 / 时间</th>
                <th>大小</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {(backups.data ?? []).map((backup) => (
                <tr key={backup.filename}>
                  <td>
                    {action?.kind === "rename" &&
                    action.filename === backup.filename ? (
                      <form
                        className="osir-backup-rename"
                        onSubmit={(event) => {
                          event.preventDefault();
                          if (name.trim())
                            void run(
                              () =>
                                backupsApi.renameDbBackup(
                                  backup.filename,
                                  name.trim(),
                                ),
                              () => "备份已重命名。",
                            );
                        }}
                      >
                        <Input
                          autoFocus
                          aria-label="备份名称"
                          value={name}
                          disabled={busy}
                          onChange={(event) => setName(event.target.value)}
                        />
                        <Button
                          variant="outline"
                          type="submit"
                          disabled={busy || !name.trim()}
                        >
                          保存
                        </Button>
                        <Button
                          variant="ghost"
                          type="button"
                          disabled={busy}
                          onClick={() => select(null)}
                        >
                          取消
                        </Button>
                      </form>
                    ) : (
                      <strong>{backup.filename}</strong>
                    )}
                    <time dateTime={backup.createdAt}>
                      {Number.isNaN(Date.parse(backup.createdAt))
                        ? backup.createdAt
                        : new Date(backup.createdAt).toLocaleString("zh-CN", {
                            hour12: false,
                          })}
                    </time>
                  </td>
                  <td>{formatBytes(backup.sizeBytes)}</td>
                  <td>
                    <div className="osir-backup-row-actions">
                      <Button
                        variant="outline"
                        disabled={busy || !!action}
                        onClick={() =>
                          select({ kind: "restore", filename: backup.filename })
                        }
                      >
                        恢复
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={"重命名备份 " + backup.filename}
                        disabled={busy || !!action}
                        onClick={() =>
                          select({ kind: "rename", filename: backup.filename })
                        }
                      >
                        <Pencil size={14} />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={"删除备份 " + backup.filename}
                        disabled={busy || !!action}
                        onClick={() =>
                          select({ kind: "delete", filename: backup.filename })
                        }
                      >
                        <Trash2 size={14} />
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {action?.kind === "restore" && (
        <BackupRecoveryDialog
          source={action.filename}
          pending={busy}
          error={error}
          onCancel={() => select(null)}
          onConfirm={() =>
            void run(
              () => backupsApi.restoreDbBackup(action.filename),
              (report) => {
                setRestoreReport(report);
                return "本地数据库已恢复。";
              },
              true,
            )
          }
        />
      )}
      <ConfirmDialog
        isOpen={action?.kind === "delete"}
        title="删除本地备份？"
        message={
          "将永久删除备份文件 " +
          (action?.filename ?? "") +
          "，不会删除当前数据库或云端资源。"
        }
        pending={busy}
        confirmText="删除备份"
        cancelText="取消"
        onCancel={() => select(null)}
        onConfirm={() => {
          if (action?.kind === "delete")
            void run(
              () => backupsApi.deleteDbBackup(action.filename),
              () => "本地备份已删除。",
            );
        }}
      />
    </section>
  );
}
