import {
  useIsMutating,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Loader2, Power } from "lucide-react";
import { useState } from "react";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { toast } from "@/lib/toast";
import {
  DESKTOP_RUNTIME_QUERY,
  LOCAL_TOOLS_QUERY,
  LOCAL_TOOL_CATALOG,
  type LocalToolInstallation,
  type SupportedToolId,
  type ToolCloseResult,
} from "./toolsApi";
import { TOOL_ACTION_QUERY } from "./ToolLaunchButton";

const SAFE_CLOSE_ERRORS = new Set([
  "此桌面应用正在启动、重启或关闭，请稍后重试",
  "无法安全确认桌面应用的运行实例，请重新检测",
  "应用未同意退出，请保存工作后重试",
  "应用尚未退出，请保存工作后重试",
  "应用尚未退出，已取消关闭；请保存工作后重试",
  "应用尚未退出，已取消重新打开；请保存工作后重试",
  "Codex 正在打开或重启，请稍后重试",
  "应用运行状态已变化，请重新打开",
  "系统未能强制退出应用，请重新检测",
  "应用仍未退出，请重新检测",
]);

/** A confirmed close targets only the desktop instance revalidated by native code. */
export function ToolCloseButton({
  tool,
  installation,
  desktop,
  disabled = false,
  className,
}: {
  tool: SupportedToolId;
  installation?: LocalToolInstallation;
  desktop: boolean;
  disabled?: boolean;
  className?: string;
}) {
  const query = useQueryClient();
  const [forceConfirm, setForceConfirm] = useState(false);
  const actionPending = useIsMutating({ mutationKey: TOOL_ACTION_QUERY }) > 0;
  const resultKey = ["osir-tool-close-error", tool] as const;
  const failure = useQuery<string | null>({
    queryKey: resultKey,
    queryFn: () => null,
    enabled: false,
    staleTime: Infinity,
    gcTime: Infinity,
  });
  const name = LOCAL_TOOL_CATALOG[tool].name;
  const canClose =
    desktop &&
    installation?.installed === true &&
    installation.runtimeStatus === "running" &&
    installation.closeSupported === true;
  const close = useMutation({
    mutationKey: TOOL_ACTION_QUERY,
    mutationFn: async () => {
      const result = await invoke<ToolCloseResult>("osir_close_tool", {
        toolId: tool,
        force: true,
      });
      if (
        result?.runtimeStatus !== "stopped" ||
        (result.closed !== true && result.alreadyStopped !== true)
      )
        throw new Error("unverified_close_result");
      return result;
    },
    onMutate: () => query.setQueryData(resultKey, null),
    onSuccess: () => toast.success(`${name} 已关闭`),
    onError: (error) =>
      query.setQueryData(
        resultKey,
        typeof error === "string" && SAFE_CLOSE_ERRORS.has(error)
          ? error === "无法安全确认桌面应用的运行实例，请重新检测"
            ? "暂时无法确认应用状态，请重新检测。"
            : error
          : `无法关闭 ${name}。请保存工作后重试，或在应用中自行退出。`,
      ),
    onSettled: () => {
      void query.invalidateQueries({ queryKey: LOCAL_TOOLS_QUERY });
      void query.invalidateQueries({ queryKey: DESKTOP_RUNTIME_QUERY });
    },
  });
  if (!LOCAL_TOOL_CATALOG[tool].desktop || installation?.kind === "cli")
    return null;
  return (
    <>
      <button
        type="button"
        className={["osir-tool-close", className].filter(Boolean).join(" ")}
        disabled={disabled || !canClose || actionPending}
        aria-label={close.isPending ? `正在关闭 ${name}` : `关闭 ${name}`}
        aria-busy={close.isPending || undefined}
        title={
          installation?.runtimeStatus !== "running"
            ? "应用未运行"
            : installation.closeSupported !== true
              ? installation.runtimeError &&
                SAFE_CLOSE_ERRORS.has(installation.runtimeError)
                ? installation.runtimeError
                : "暂时无法确认应用状态，请重新检测"
              : `关闭 ${name}`
        }
        onClick={() => {
          if (!canClose || query.isMutating({ mutationKey: TOOL_ACTION_QUERY }))
            return;
          setForceConfirm(true);
        }}
      >
        {close.isPending ? (
          <Loader2 size={17} className="animate-spin" aria-hidden="true" />
        ) : (
          <Power size={17} strokeWidth={1.8} aria-hidden="true" />
        )}
      </button>
      {failure.data && (
        <span role="alert" className="osir-tool-launch-error">
          {failure.data}
          {canClose && (
            <button
              type="button"
              disabled={disabled || !canClose || actionPending}
              onClick={() => setForceConfirm(true)}
            >
              重试关闭…
            </button>
          )}
        </span>
      )}
      <ConfirmDialog
        className="osir-tool-update-dialog"
        isOpen={forceConfirm}
        title={`关闭 ${name}？`}
        message="关闭会中断正在进行的对话，未保存的内容可能丢失。"
        confirmText="确认关闭"
        cancelText="取消"
        variant="destructive"
        pending={close.isPending}
        confirmDisabled={disabled || !canClose || actionPending}
        onCancel={() => setForceConfirm(false)}
        onConfirm={() => {
          if (
            disabled ||
            !canClose ||
            query.isMutating({ mutationKey: TOOL_ACTION_QUERY })
          )
            return;
          setForceConfirm(false);
          close.mutate();
        }}
      />
    </>
  );
}
