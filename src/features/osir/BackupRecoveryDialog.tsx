import { useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import type { DbRestoreReport, RestoreSyncPhase } from "@/lib/api/settings";
import { connectionError } from "./errors";

const syncPhaseLabels: Record<RestoreSyncPhase, string> = {
  live_configuration: "工具配置（供应商、MCP、Skills）",
  prompts: "提示词",
  model_pricing: "本地模型定价",
  settings_cache: "设置缓存",
  runtime_log_level: "运行时日志级别",
  usage_cache: "用量缓存清理",
};
const syncStatusLabels = {
  success: "成功",
  failed: "失败",
  skipped: "跳过",
};

export function BackupRecoveryReport({ report }: { report: DbRestoreReport }) {
  const incomplete = report.postImportSync.some(
    (stage) => stage.status !== "success",
  );
  return (
    <section aria-label="恢复后同步结果">
      <h3>恢复后同步结果</h3>
      <p role={incomplete ? "alert" : undefined} className="osir-backup-note">
        {incomplete
          ? "数据库已恢复，但部分同步失败或跳过；请检查对应功能，无需重复恢复数据库。"
          : "各同步阶段均已完成；这不代表完整工具目录已回滚或所有工具已逐一验证。"}
      </p>
      <div className="osir-backup-table-wrap">
        <table className="osir-backup-table" aria-label="恢复后同步阶段">
          <thead>
            <tr>
              <th scope="col">阶段</th>
              <th scope="col">结果</th>
              <th scope="col">说明</th>
            </tr>
          </thead>
          <tbody>
            {report.postImportSync.map((stage) => (
              <tr key={stage.phase}>
                <th scope="row">{syncPhaseLabels[stage.phase]}</th>
                <td>{syncStatusLabels[stage.status]}</td>
                <td>{stage.message ? connectionError(stage.message) : "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

export function BackupScope({ compact = false }: { compact?: boolean }) {
  return (
    <div className="osir-backup-scope">
      <section>
        <h3>将替换的本地数据</h3>
        <p>
          供应商及模型配置、MCP 配置、提示词、Skills
          安装记录与启用状态，以及数据库内的设置和本地用量记录。
        </p>
      </section>
      <section>
        <h3>不包含在备份中</h3>
        <p>
          会话正文、项目文件、工具安装程序、Skills
          文件、系统凭据和客户端设备设置。
        </p>
      </section>
      {!compact && (
        <section>
          <h3>恢复后仍需检查</h3>
          <p>
            会按恢复的数据重新应用供应商、MCP、Skills
            和提示词，可能改写工具配置；这不是完整工具目录回滚。系统凭据不会随备份迁移，密钥引用失效时需要重新连接。
          </p>
        </section>
      )}
    </div>
  );
}

export function BackupRecoveryDialog({
  source,
  sql = false,
  pending,
  error,
  onConfirm,
  onCancel,
}: {
  source: string;
  sql?: boolean;
  pending: boolean;
  error: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [acknowledged, setAcknowledged] = useState(false);
  return (
    <Dialog open onOpenChange={(open) => !open && !pending && onCancel()}>
      <DialogContent className="osir-backup-confirm" zIndex="alert">
        <DialogHeader>
          <DialogTitle>{sql ? "导入本地数据？" : "恢复本地数据？"}</DialogTitle>
          <DialogDescription>
            将整体替换当前本地数据库，不是合并，也不能只恢复某一个工具。
          </DialogDescription>
        </DialogHeader>
        <div className="osir-backup-confirm-body">
          <p className="osir-backup-source">
            <span>备份文件</span>
            <strong>{source}</strong>
          </p>
          <BackupScope />
          <p className="osir-backup-note">
            系统会先校验备份，替换前尝试创建当前数据库的安全备份；创建报错时停止恢复，完成后显示返回的安全备份编号。云端账户、余额、订阅和密钥不受影响。
          </p>
          <p className="osir-backup-note">
            请先关闭相关工具并自行保存外部修改；此页不提供工具文件差异检查。
            {sql
              ? "导入后请检查各工具配置是否已同步。"
              : "完成后逐项显示同步阶段结果；数据库恢复成功与后续同步失败会分别说明，不代表全部工具已逐一验证。"}
          </p>
          {error && (
            <p role="alert" className="osir-management-feedback">
              {error}
            </p>
          )}
          <label className="osir-backup-acknowledgement">
            <Checkbox
              checked={acknowledged}
              disabled={pending}
              onCheckedChange={setAcknowledged}
            />
            <span>
              我已保存外部修改，了解本地数据将被替换及工具配置可能被改写。
            </span>
          </label>
        </div>
        <DialogFooter>
          <Button variant="outline" disabled={pending} onClick={onCancel}>
            取消
          </Button>
          <Button
            className="osir-backup-primary"
            disabled={pending || !acknowledged}
            aria-busy={pending}
            onClick={onConfirm}
          >
            {pending ? "正在处理…" : sql ? "确认导入" : "确认恢复"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
