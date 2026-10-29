import { useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { settingsApi } from "@/lib/api";
import { toast } from "@/lib/toast";
import { BackupRecoveryDialog } from "./BackupRecoveryDialog";
import { connectionError } from "./errors";

export function OsirBackupTransfer({
  disabled,
  onBlockedChange,
}: {
  disabled: boolean;
  onBlockedChange: (blocked: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const [selected, setSelected] = useState("");
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [recoveryNotice, setRecoveryNotice] = useState("");
  const lock = useRef(false);
  const selectedRef = useRef("");
  const filename =
    selected.split(String.fromCharCode(92)).join("/").split("/").pop() ?? "";
  const run = async (operation: () => Promise<void>) => {
    if (lock.current || disabled) return;
    lock.current = true;
    setBusy(true);
    setError("");
    onBlockedChange(true);
    try {
      await operation();
    } catch (e) {
      setError(connectionError(e));
    } finally {
      lock.current = false;
      setBusy(false);
      onBlockedChange(!!selectedRef.current);
    }
  };
  const release = () => {
    selectedRef.current = "";
    setSelected("");
    setConfirm(false);
    if (!lock.current) onBlockedChange(false);
  };
  const select = () =>
    run(async () => {
      const path = await settingsApi.openFileDialog();
      if (path) {
        selectedRef.current = path;
        setSelected(path);
      }
    });
  const importData = () =>
    run(async () => {
      const result = (await settingsApi.importConfigFromFile(
        selected,
      )) as Awaited<ReturnType<typeof settingsApi.importConfigFromFile>> & {
        warning?: string;
      };
      if (!result.success)
        throw new Error(result.message || "本地数据未导入，请检查文件后重试。");
      release();
      toast.success("本地数据库已导入。");
      let message = result.backupId
        ? "安全备份：" + result.backupId + "。"
        : "未返回安全备份编号。";
      message += result.warning
        ? "部分配置同步未完成：" + connectionError(result.warning)
        : "请检查各工具连接和本机 Skills 文件，数据库导入不代表工具完整恢复。";
      setRecoveryNotice(message);
      try {
        await queryClient.invalidateQueries();
      } catch {
        setRecoveryNotice(
          message + "页面数据刷新失败，请重新打开此页；无需重复导入。",
        );
      }
    });
  const exportData = () =>
    run(async () => {
      const destination = await settingsApi.saveFileDialog(
        "osir-local-data-" +
          new Date().toISOString().replace(/[:.]/g, "-") +
          ".sql",
      );
      if (!destination) return;
      const result = await settingsApi.exportConfigToFile(destination);
      if (!result.success)
        throw new Error(result.message || "本地数据导出失败，请重试。");
      toast.success("本地数据已导出。此文件未脱敏，请妥善保管。");
    });
  return (
    <section
      className="osir-backup-transfer"
      aria-labelledby="osir-backup-transfer-title"
    >
      <header className="osir-backup-card-heading">
        <div>
          <h2 id="osir-backup-transfer-title">导入与导出</h2>
          <p>本地数据库 SQL 文件，与上方备份范围相同。</p>
        </div>
      </header>
      <p className="osir-backup-note">
        备份和导出文件可能包含手动供应商密钥、MCP
        环境变量等敏感配置，未加密、未脱敏；请仅导入可信来源，不要作为诊断文件分享。
      </p>
      <div className="osir-backup-transfer-actions">
        <Button
          variant="outline"
          disabled={disabled || busy || !!selected}
          onClick={() => void select()}
        >
          选择 SQL 备份
        </Button>
        <Button
          variant="outline"
          disabled={disabled || busy || !!selected}
          onClick={() => void exportData()}
        >
          导出本地数据
        </Button>
      </div>
      {selected && (
        <div className="osir-backup-selected">
          <strong>{filename}</strong>
          <div className="osir-backup-row-actions">
            <Button
              className="osir-backup-primary"
              disabled={disabled || busy}
              onClick={() => {
                setError("");
                setConfirm(true);
              }}
            >
              导入本地数据
            </Button>
            <Button
              variant="ghost"
              disabled={busy}
              onClick={() => {
                release();
                setError("");
              }}
            >
              取消选择
            </Button>
          </div>
        </div>
      )}
      {busy && !confirm && <p role="status">正在处理文件…</p>}
      {recoveryNotice && (
        <p role="status" className="osir-backup-note">
          {recoveryNotice}
        </p>
      )}
      {error && !confirm && (
        <p role="alert" className="osir-management-feedback">
          {error}
        </p>
      )}
      {confirm && (
        <BackupRecoveryDialog
          source={filename}
          sql
          pending={busy}
          error={error}
          onConfirm={() => void importData()}
          onCancel={() => {
            setConfirm(false);
            setError("");
          }}
        />
      )}
    </section>
  );
}
