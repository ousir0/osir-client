import { useEffect, useState } from "react";
import {
  useIsMutating,
  useMutation,
  useQueryClient,
} from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Loader2, Monitor, RotateCw, Terminal } from "lucide-react";
import { toast } from "@/lib/toast";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import "./tool-runtime-actions.css";
import {
  LOCAL_TOOL_CATALOG,
  LOCAL_TOOLS_QUERY,
  DESKTOP_RUNTIME_QUERY,
  type LocalToolInstallation,
  type SupportedToolId,
  type ToolLaunchResult,
  type ToolLaunchTarget,
} from "./toolsApi";

export const TOOL_ACTION_QUERY = ["osir-tool-action"] as const;
const SAFE_LAUNCH_ERRORS = new Set([
  "此桌面应用正在启动、重启或关闭，请稍后重试",
  "无法安全确认桌面应用的运行实例，请重新检测",
  "当前 OSIRAPI 连接尚未完整应用，请先连接后再启动",
  "当前工具连接配置已变化，请重新连接后再启动",
  "无法读取当前工具配置，请检查配置后重试",
  "请先选择一个项目文件夹",
  "请选择有效的项目文件夹",
  "项目文件夹已不可用，请重新选择",
  "请选择项目文件夹，而不是文件",
  "桌面应用启动不接受项目目录",
  "代理尚未连接，请先重新连接",
  "无法确认当前工具连接状态",
  "无法确定工具的默认安装，请重新检测安装状态",
  "工具安装路径不可用于启动",
  "无法准备工具启动环境",
  "无法打开工具终端，请检查终端设置后重试",
  "应用运行状态已变化，请重新打开",
  "系统未能强制退出应用，请重新检测",
  "应用仍未退出，请重新检测",
  "应用未同意退出，请保存工作后重试",
  "应用尚未退出，已取消重新打开；请保存工作后重试",
  "Codex 开发配置目录不可用，请重新启动 OSIRAPI 客户端",
  "Codex 开发配置被其他进程占用，请检查后重试",
  "无法创建 Codex 开发配置目录，请检查权限",
  "Codex 开发窗口启动未完成，请重试",
  "无法启动 Codex 开发窗口，请检查安装后重试",
  "Codex 开发窗口启动请求已发送，但尚未确认隔离配置窗口",
  "Codex 正在打开或重启，请稍后重试",
  "Codex 开发窗口正在启动，请稍后再点运行中检查；不会重复打开窗口",
  "Codex 开发窗口仍未完成启动，请检查该窗口；如需重试，请先关闭该开发窗口",
  "Codex 开发窗口已退出，请检查安装后重试",
  "Codex 开发窗口的启动状态暂不能确认，请检查该窗口，关闭后重试",
  "Codex 开发窗口状态已变化，请重新检查",
  "Codex 开发窗口检测超时，请重试",
  "Codex 安装状态已改变，请重新检查",
  "Codex 启动状态暂不可用，请重新打开 OSIRAPI 客户端",
  "Codex 启动状态暂不可用，请检查刚打开的开发窗口",
  "无法确认 Codex 开发窗口进程，请检查后重试",
  "上次 Codex 开发进程已退出，但配置仍被其他进程占用，请检查后重试",
  "Codex Desktop 启动请求已发送，但尚未确认前台窗口；请检查应用后重试",
  "系统未同意将应用切换到前台，请在 Dock 中打开后重试",
]);

function launchErrorMessage(error: unknown, name: string) {
  if (typeof error !== "string" || !SAFE_LAUNCH_ERRORS.has(error))
    return `无法打开 ${name}，请重新检测安装与配置后重试。`;
  const messages: Record<string, string> = {
    "无法安全确认桌面应用的运行实例，请重新检测":
      "暂时无法确认应用状态，请重新检测。",
    "Codex 开发配置目录不可用，请重新启动 OSIRAPI 客户端":
      "Codex 配置不可用，请重新打开 OSIR 客户端。",
    "Codex 开发配置被其他进程占用，请检查后重试":
      "Codex 配置正在使用，请稍后重试。",
    "无法创建 Codex 开发配置目录，请检查权限":
      "无法保存 Codex 配置，请检查文件夹权限。",
    "Codex 开发窗口启动未完成，请重试": "Codex 尚未打开，请重试。",
    "无法启动 Codex 开发窗口，请检查安装后重试":
      "无法打开 Codex，请检查安装后重试。",
    "Codex 开发窗口启动请求已发送，但尚未确认隔离配置窗口":
      "Codex 打开请求已发送，请查看应用。",
    "Codex 开发窗口正在启动，请稍后再点运行中检查；不会重复打开窗口":
      "Codex 正在打开，请稍后查看。",
    "Codex 开发窗口仍未完成启动，请检查该窗口；如需重试，请先关闭该开发窗口":
      "Codex 尚未完成启动，请查看应用，或关闭后重试。",
    "Codex 开发窗口已退出，请检查安装后重试":
      "Codex 已退出，请检查安装后重试。",
    "Codex 开发窗口的启动状态暂不能确认，请检查该窗口，关闭后重试":
      "暂时无法确认 Codex 是否已打开，请查看应用，或关闭后重试。",
    "Codex 开发窗口状态已变化，请重新检查": "Codex 状态已变化，请重新检测。",
    "Codex 开发窗口检测超时，请重试": "Codex 状态检测超时，请重试。",
    "Codex 启动状态暂不可用，请重新打开 OSIRAPI 客户端":
      "Codex 状态暂不可用，请重新打开 OSIR 客户端。",
    "Codex 启动状态暂不可用，请检查刚打开的开发窗口":
      "Codex 状态暂不可用，请查看应用。",
    "无法确认 Codex 开发窗口进程，请检查后重试":
      "暂时无法确认 Codex 状态，请重新检测。",
    "上次 Codex 开发进程已退出，但配置仍被其他进程占用，请检查后重试":
      "Codex 配置正在使用，请稍后重试。",
    "Codex Desktop 启动请求已发送，但尚未确认前台窗口；请检查应用后重试":
      "Codex 打开请求已发送，请查看应用。",
  };
  return messages[error] ?? error;
}

export function canLaunchTool(info?: LocalToolInstallation) {
  return !!info?.installed && info.launchSupported === true;
}

/** Launching never implies running: only native detection updates the inventory. */
export function ToolLaunchButton({
  tool,
  installation,
  desktop,
  target: requestedTarget,
  disabled = false,
  className,
  onLaunch,
  onBusyChange,
}: {
  tool: SupportedToolId;
  installation?: LocalToolInstallation;
  desktop: boolean;
  target?: ToolLaunchTarget;
  disabled?: boolean;
  className?: string;
  onLaunch?: () => Promise<ToolLaunchResult | void>;
  onBusyChange?: (busy: boolean) => void;
}) {
  const query = useQueryClient();
  const launching = useIsMutating({ mutationKey: TOOL_ACTION_QUERY }) > 0;
  const target: ToolLaunchTarget =
    requestedTarget ?? (LOCAL_TOOL_CATALOG[tool].desktop ? "desktop" : "cli");
  const running =
    target === "desktop" && installation?.runtimeStatus === "running";
  const codexCli = tool === "codex" && target === "cli";
  const canLaunch = desktop && !disabled && canLaunchTool(installation);
  const canRestart =
    canLaunch &&
    running &&
    installation?.restartSupported === true &&
    !onLaunch;
  const name = LOCAL_TOOL_CATALOG[tool].name + (codexCli ? " CLI" : "");
  const [restartConfirm, setRestartConfirm] = useState(false);
  const launch = useMutation({
    mutationKey: TOOL_ACTION_QUERY,
    mutationFn: async (restart: boolean) => {
      if (onLaunch) return onLaunch();
      if (target === "cli") {
        const directory = await open({
          directory: true,
          multiple: false,
          title: `选择 ${name} 的项目文件夹`,
        });
        if (typeof directory !== "string") return;
        return invoke<ToolLaunchResult>("osir_launch_tool", {
          toolId: tool,
          target,
          directory,
        });
      }
      return invoke<ToolLaunchResult>("osir_launch_tool", {
        toolId: tool,
        target,
        ...(restart ? { restart: true } : {}),
      });
    },
    onSuccess: (result) => {
      if (!result) return;
      if (target === "cli") {
        if (result.exitCode !== undefined && result.exitCode !== null) {
          toast.info(
            `${name} 已退出（${result.exitCode}），请检查工具后重试。`,
          );
        } else if (result.foreground) toast.success(`${name} 终端已打开`);
        else toast.info("已发送打开请求，请查看终端。");
      } else if (result.foreground) {
        toast.success(
          name +
            (result.restarted
              ? " 已重启"
              : result.alreadyRunning
                ? " 已唤起"
                : " 已打开"),
        );
      } else {
        toast.info(name + " 打开请求已发送，请查看应用。");
      }
    },
    onSettled: () => {
      void query.invalidateQueries({ queryKey: LOCAL_TOOLS_QUERY });
      void query.invalidateQueries({ queryKey: DESKTOP_RUNTIME_QUERY });
    },
  });
  useEffect(() => {
    if (!launch.isPending) return;
    onBusyChange?.(true);
    return () => onBusyChange?.(false);
  }, [launch.isPending, onBusyChange]);
  const label = launch.isPending
    ? "正在打开…"
    : running
      ? "运行中"
      : codexCli
        ? "打开 CLI"
        : "打开";
  return (
    <>
      <span className="osir-tool-launch-actions">
        <button
          type="button"
          className={className}
          disabled={!canLaunch || launching}
          aria-busy={launch.isPending || undefined}
          aria-label={
            launch.isPending
              ? `正在打开 ${name}`
              : running
                ? `${name}：运行中`
                : `打开 ${name}`
          }
          onClick={() => {
            if (
              canLaunch &&
              query.isMutating({ mutationKey: TOOL_ACTION_QUERY }) === 0
            )
              launch.mutate(false);
          }}
        >
          {launch.isPending ? (
            <Loader2 size={15} className="animate-spin" aria-hidden="true" />
          ) : target === "desktop" ? (
            <Monitor size={15} aria-hidden="true" />
          ) : (
            <Terminal size={15} aria-hidden="true" />
          )}
          {label}
        </button>
        {target === "desktop" && running && !onLaunch && (
          <button
            type="button"
            className="osir-tool-restart"
            disabled={!canRestart || launching}
            onClick={() => setRestartConfirm(true)}
            aria-label={`重启 ${name}`}
            title={
              installation?.restartSupported === true
                ? `重启 ${name}`
                : installation?.runtimeError || "运行实例暂不可确认，请重新检测"
            }
          >
            <RotateCw size={18} strokeWidth={1.8} aria-hidden="true" />
          </button>
        )}
      </span>
      <ConfirmDialog
        className="osir-tool-update-dialog"
        isOpen={restartConfirm}
        title={`重启 ${name}？`}
        message="重启会中断正在进行的对话，未保存的内容可能丢失。"
        confirmText="确认重启"
        cancelText="取消"
        variant="destructive"
        pending={launch.isPending}
        confirmDisabled={!canRestart || launching}
        onCancel={() => setRestartConfirm(false)}
        onConfirm={() => {
          if (
            !canRestart ||
            query.isMutating({ mutationKey: TOOL_ACTION_QUERY }) !== 0
          )
            return;
          setRestartConfirm(false);
          launch.mutate(true);
        }}
      />
      {launch.isError && (
        <span role="alert" className="osir-tool-launch-error">
          {launchErrorMessage(launch.error, name)}
        </span>
      )}
    </>
  );
}
