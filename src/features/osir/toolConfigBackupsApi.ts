import { invoke } from "@tauri-apps/api/core";

export type BackupTool = "codex" | "claude" | "opencode" | "gemini";
export type ToolBackupFile = {
  path: string;
  hash: string | null;
  sizeBytes: number;
};
export type ToolBackupSnapshot = {
  id: string;
  tool: BackupTool;
  createdAt: string;
  safety: boolean;
  files: ToolBackupFile[];
};
export type ToolBackupState = {
  snapshots: ToolBackupSnapshot[];
  pendingRecovery: boolean;
};
export type ToolBackupPreview = {
  previewId: string;
  snapshotId: string;
  tool: BackupTool;
  files: Array<{
    path: string;
    currentHash: string | null;
    snapshotHash: string | null;
    currentSize: number;
    snapshotSize: number;
    changed: boolean;
  }>;
};
export type ToolBackupRestoreReport = {
  status: "restored" | "noChanges" | "rolledBack" | "recoveryRequired";
  safetySnapshotId: string | null;
  conflicts: string[];
};
export const toolConfigBackupsApi = {
  list: (tool: BackupTool) =>
    invoke<ToolBackupState>("osir_tool_backups_list", { tool }),
  create: (tool: BackupTool) =>
    invoke<ToolBackupSnapshot>("osir_tool_backup_create", { tool }),
  preview: (tool: BackupTool, snapshotId: string) =>
    invoke<ToolBackupPreview>("osir_tool_backup_preview", { tool, snapshotId }),
  restore: (tool: BackupTool, previewId: string, confirmed: boolean) =>
    invoke<ToolBackupRestoreReport>("osir_tool_backup_restore", {
      tool,
      previewId,
      confirmed,
    }),
  recover: (tool: BackupTool) =>
    invoke<ToolBackupRestoreReport>("osir_tool_backup_recover", { tool }),
};
