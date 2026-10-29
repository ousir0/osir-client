import { extractErrorMessage } from "@/utils/errorUtils";
import type { ToolLifecycleAction, ToolName } from "./useToolManagement";

export type ToolLifecycleReason =
  | "completed"
  | "versionUnchanged"
  | "notRunnable"
  | "uninstallUnverified"
  | "unmanaged"
  | "inProgress"
  | "network"
  | "dns"
  | "permission"
  | "diskFull"
  | "node"
  | "incompleteInstall"
  | "missingInstaller"
  | "timeout"
  | "failed";

export interface ToolLifecycleResult {
  tool: ToolName;
  action: ToolLifecycleAction;
  status: "success" | "warning" | "error";
  reason: ToolLifecycleReason;
  version: string | null;
}

/** Raw installer output is used transiently for classification, never retained. */
export function toolLifecycleFailureReason(
  error: unknown,
): ToolLifecycleReason {
  const text = extractErrorMessage(error).toLowerCase();
  const nativeReasons: Record<string, ToolLifecycleReason> = {
    tool_install_dns: "dns",
    tool_install_network: "network",
    tool_install_permission: "permission",
    tool_install_disk_full: "diskFull",
    tool_install_node: "node",
    tool_install_native_missing: "incompleteInstall",
    tool_install_timeout: "timeout",
    tool_install_failed: "failed",
  };
  for (const [code, reason] of Object.entries(nativeReasons))
    if (text.startsWith(code)) return reason;
  if (text.includes("tool_action_in_progress")) return "inProgress";
  if (/enospc|no space left|磁盘空间/.test(text)) return "diskFull";
  if (
    /run the postinstall manually|could not find native binary package|missing optional dependency/.test(
      text,
    )
  )
    return "incompleteInstall";
  if (/ebadengine|unsupported engine|node.*requires|node.*required/.test(text))
    return "node";
  if (/enotfound|eai_again|could not resolve/.test(text)) return "dns";
  if (/timeout|timed out|超时/.test(text)) return "timeout";
  if (/eacces|eperm|permission denied|权限不足|无权/.test(text))
    return "permission";
  if (/not found|enoent|command not found|未找到|找不到/.test(text))
    return "missingInstaller";
  if (
    /enotfound|econnrefused|econnreset|network|download|failed to fetch|下载|网络|无法连接/.test(
      text,
    )
  )
    return "network";
  return "failed";
}

export function safeToolLifecycleVersion(version: string | null | undefined) {
  return version && /^v?\d[\dA-Za-z.+_-]{0,63}$/.test(version) ? version : null;
}
