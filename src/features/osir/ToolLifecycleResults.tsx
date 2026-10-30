import { CheckCircle2, TriangleAlert, X } from "lucide-react";
import {
  dismissToolLifecycleResult,
  toolDisplayName,
  useToolLifecycleResults,
  type ToolLifecycleAction,
  type ToolName,
} from "@/components/apps/useToolManagement";
import type {
  ToolLifecycleReason,
  ToolLifecycleResult,
} from "@/components/apps/toolLifecycleResults";
import "./tool-lifecycle-results.css";

const ACTION_LABELS: Record<ToolLifecycleAction, string> = {
  install: "安装",
  update: "更新",
  uninstall: "卸载",
};
const REASONS: Record<ToolLifecycleReason, string> = {
  completed: "",
  versionUnchanged: "版本仍未更新，请检查安装来源后重试。",
  notRunnable: "工具暂时无法运行，请检查安装环境。",
  uninstallUnverified: "仍有安装或检测结果待确认，请重新检测。",
  unmanaged: "当前安装来源暂不支持自动更新，请使用原安装方式更新。",
  inProgress: "正在处理，请稍后重试。",
  network: "下载或连接失败，请检查网络后重试。",
  dns: "无法解析下载地址，请检查网络或代理后重试。",
  permission: "安装位置的访问权限不足，请检查权限后重试。",
  diskFull: "磁盘空间不足，请释放空间后重试。",
  node: "当前 Node 环境不满足工具要求，请更新 Node 后重试。",
  incompleteInstall: "运行组件未安装完整，请重试修复安装。",
  missingInstaller: "未找到所需安装程序，请检查安装环境后重试。",
  timeout: "操作超时，请重新检测后重试。",
  failed: "操作未完成，请检查网络与安装环境后重试。",
};

export function ToolLifecycleResults({
  toolIds,
  disabled = false,
  onRetry,
}: {
  toolIds?: readonly ToolName[];
  disabled?: boolean;
  onRetry: (result: ToolLifecycleResult) => void;
}) {
  const results = useToolLifecycleResults();
  const visible = [...results.values()].filter(
    (result) => !toolIds || toolIds.includes(result.tool),
  );
  if (!visible.length) return null;
  return (
    <div className="osir-tool-results" aria-label="工具操作结果">
      {visible.map((result) => {
        const name = toolDisplayName(result.tool);
        const action = ACTION_LABELS[result.action];
        const retryable =
          result.status !== "success" &&
          result.action !== "uninstall" &&
          !["unmanaged", "inProgress"].includes(result.reason);
        const Icon = result.status === "success" ? CheckCircle2 : TriangleAlert;
        return (
          <section
            key={result.tool}
            className="osir-tool-result"
            data-state={result.status}
            role={result.status === "success" ? "status" : "alert"}
            aria-label={`${name} ${action}结果`}
          >
            <Icon size={16} aria-hidden="true" />
            <div className="osir-tool-result-copy">
              <strong>
                {name} · {action}
                {result.status === "success"
                  ? "完成"
                  : result.status === "warning"
                    ? "待核对"
                    : "失败"}
                {result.version ? ` · ${result.version}` : ""}
              </strong>
              {REASONS[result.reason] && <p>{REASONS[result.reason]}</p>}
            </div>
            <div className="osir-tool-result-actions">
              {retryable && (
                <button
                  type="button"
                  disabled={disabled}
                  onClick={() => onRetry(result)}
                  aria-label={`重试${action} ${name}`}
                >
                  重试
                </button>
              )}
              <button
                type="button"
                className="osir-tool-result-close"
                disabled={disabled}
                aria-label={`关闭 ${name} ${action}结果`}
                onClick={() => dismissToolLifecycleResult(result.tool)}
              >
                <X size={15} aria-hidden="true" />
              </button>
            </div>
          </section>
        );
      })}
    </div>
  );
}
