import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "@/lib/toast";
import { useAccountSession, type AccountSession } from "./accountApi";

export const ANYWHERE_LOCAL_QUERY = ["osir-anywhere-local"] as const;
export const ANYWHERE_INSTALL_QUERY = ["osir-anywhere-install"] as const;
const RELEASE_QUERY = ["osir-anywhere-release"] as const;
// Only complete, known native messages may reach the UI. Prefix matching both
// hid real launch blockers and allowed arbitrary diagnostics after a safe prefix.
const ANYWHERE_LAUNCH_ERRORS = new Set([
  "请先登录 OSIR 客户端，再打开 Anywhere",
  "本机模型信息暂时无法更新，请重试",
  "DSH 模型配置无法读取",
  "DSH 尚未启动，请检查 DSH 安装后重试",
  "Anywhere 正在连接，请稍候",
  "OSIR Anywhere 正在打开，请稍后重试",
  "未找到可安全启动的 OSIR Anywhere，请先安装或重新检查",
  "OSIR Anywhere 安装状态已改变，请重新检查",
  "Anywhere 安装状态已改变，请重试",
  "无法验证应用版本，请通过 OSIR 客户端重新安装 Anywhere",
  "无法创建本机安全连接，请重试",
  "Anywhere 版本无法验证，请通过客户端修复安装",
  "账号已改变，请重新打开 Anywhere",
  "账号已改变，已取消本次 Anywhere 连接",
  "Anywhere 账号连接未完成，请检查服务状态后重试",
  "Anywhere 连接超时，请重新打开",
  "Anywhere 安全连接未完成，请重新打开",
  "Anywhere 打开超时，请重试",
  "Anywhere 无法打开，请重试",
  "Anywhere 无法打开，请检查安装后重试",
  "无法确认 OSIR Anywhere 的安装身份与版本，请修复安装",
  "macOS 系统策略拒绝打开 OSIR Anywhere，客户端已尝试打开设置。",
  "macOS 系统策略拒绝打开 OSIR Anywhere，客户端已尝试打开“系统设置 → 隐私与安全性”。是否允许打开由你和系统决定；若有“仍要打开”选项且你信任该应用，可手动批准后重试。",
]);
// Exact static native messages only; never surface URLs, tokens or appended diagnostics.
const ANYWHERE_INSTALL_ERRORS = new Set([
  "安装包下载失败，请检查网络后重试",
  "无法获取正式发行信息，请检查网络后重试",
  "发行信息检查超时，请重试",
  "发行信息下载中断",
  "无法确认服务端兼容版本，请稍后重试",
  "服务端版本检查超时，请稍后重试",
  "正式发行信息暂不可用，未跟随跳转",
  "服务端版本检查不可用，已阻止不确定的安装",
  "下载长时间没有响应，请重试",
  "下载中断，请重试",
  "安装包下载超时，请重试",
  "无法写入安装缓存，请检查权限",
  "安装包写入失败，请检查空间及权限",
  "安装包大小或校验值不符，已阻止打开",
  "安装包校验失败，已阻止打开，请重新下载",
]);
export type AnywhereState = {
  status: "installed" | "not_installed" | "error" | "unsupported";
  installed: boolean;
  version: string | null;
  revision?: number | null;
  path: string | null;
  error: string | null;
  runtimeStatus: "running" | "stopped" | "unavailable";
};
export type AnywhereRelease = {
  status: "ready" | "not_configured" | "unsupported" | "error";
  version: string | null;
  revision?: number;
  platform: string | null;
  arch: string | null;
  size: number | null;
  error: string | null;
  systemSignature?: "unsigned" | "developer-id-notarized" | "authenticode";
};
export type AnywhereInstall = {
  phase:
    | "idle"
    | "downloading"
    | "ready"
    | "opening"
    | "waiting_installation"
    | "cancelled"
    | "error";
  downloadedBytes: number;
  totalBytes: number | null;
  error: string | null;
  installId: number;
  targetVersion: string | null;
  targetRevision: number | null;
  installationComplete: boolean;
};

const INSTALL_WAIT_MS = 5 * 60_000;
type InstallIntent = {
  version: string;
  revision: number;
  generation: number;
  wasInstalled: boolean;
  alreadySatisfied: boolean;
  waitingSince: number | null;
  expiresAt: number;
  installId: number | null;
};

function useAnywhereController(desktop: boolean) {
  const client = useQueryClient();
  const [action, setAction] = useState<
    "launch" | "install" | "cancel" | "detect" | null
  >(null);
  const actionLock = useRef(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const previousInstallation = useRef<AnywhereState | null>(null);
  const account = useAccountSession();
  const accountSnapshot = useRef(account.data);
  accountSnapshot.current = account.isError ? undefined : account.data;
  const [installIntent, setInstallIntent] = useState<InstallIntent | null>(
    null,
  );
  const intentRef = useRef<InstallIntent | null>(null);
  const mounted = useRef(true);
  const rememberIntent = useCallback((intent: InstallIntent | null) => {
    intentRef.current = intent;
    setInstallIntent(intent);
  }, []);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      intentRef.current = null;
    };
  }, []);
  // Revoke synchronously on the shared session cache notification, including
  // a switch while the install/detection IPC promise is still pending.
  useEffect(
    () =>
      client.getQueryCache().subscribe((event) => {
        if (
          event.query.queryKey[0] !== "osir-account" ||
          event.query.queryKey[1] !== "session"
        )
          return;
        const next =
          event.type === "removed" || event.query.state.status === "error"
            ? undefined
            : (event.query.state.data as AccountSession | undefined);
        accountSnapshot.current = next;
        const intent = intentRef.current;
        if (
          intent &&
          (!next?.connected || next.generation !== intent.generation)
        )
          rememberIntent(null);
      }),
    [client, rememberIntent],
  );
  useEffect(() => {
    const intent = intentRef.current;
    if (
      intent &&
      (!desktop ||
        account.isError ||
        !account.data?.connected ||
        account.data.generation !== intent.generation)
    )
      rememberIntent(null);
  }, [
    desktop,
    account.isError,
    account.data?.connected,
    account.data?.generation,
    rememberIntent,
  ]);
  const local = useQuery({
    queryKey: ANYWHERE_LOCAL_QUERY,
    queryFn: () => invoke<AnywhereState>("osir_anywhere_detect"),
    enabled: desktop,
    retry: false,
    staleTime: 15_000,
    refetchOnWindowFocus: "always",
    refetchIntervalInBackground: true,
    refetchInterval: (query) =>
      installIntent?.waitingSince != null &&
      !action &&
      !query.state.error &&
      Date.now() < installIntent.expiresAt
        ? 1000
        : false,
  });
  const installed =
    desktop &&
    !local.isError &&
    local.data?.status === "installed" &&
    local.data.installed;
  useEffect(() => {
    if (installed) previousInstallation.current = local.data!;
    else if (
      local.data?.status === "not_installed" ||
      local.data?.status === "unsupported"
    )
      previousInstallation.current = null;
  }, [installed, local.data]);
  const release = useQuery({
    queryKey: RELEASE_QUERY,
    queryFn: () => invoke<AnywhereRelease>("osir_anywhere_release"),
    enabled:
      desktop &&
      !local.isError &&
      ["installed", "not_installed"].includes(local.data?.status ?? ""),
    retry: false,
    staleTime: 60_000,
  });
  const installation = useQuery({
    queryKey: ANYWHERE_INSTALL_QUERY,
    queryFn: () => invoke<AnywhereInstall>("osir_anywhere_install_status"),
    enabled: desktop,
    retry: false,
    staleTime: 0,
    refetchOnWindowFocus: false,
    refetchIntervalInBackground: true,
    // Ready still belongs to the native worker: package verification and the
    // OS handoff continue after download. Stop only at a terminal/handoff state.
    // A failed status read needs an explicit retry before another request.
    refetchInterval: (query) =>
      !action &&
      !query.state.error &&
      (["downloading", "ready", "opening"].includes(
        query.state.data?.phase ?? "",
      ) ||
        (query.state.data?.phase === "waiting_installation" &&
          installIntent?.waitingSince != null &&
          Date.now() < installIntent.expiresAt))
        ? 750
        : false,
  });
  const phase = installation.data?.phase;
  const updateAvailable =
    installed &&
    release.data?.status === "ready" &&
    !!local.data?.version &&
    !!release.data.version &&
    compareAnywhereBuild(
      release.data.version,
      release.data.revision ?? 0,
      local.data.version,
      local.data.revision ?? 0,
    ) > 0;
  const previousPhase = useRef(phase);
  useEffect(() => {
    if (
      phase !== previousPhase.current &&
      (phase === "waiting_installation" || phase === "ready")
    ) {
      void client.invalidateQueries({ queryKey: ANYWHERE_LOCAL_QUERY });
    }
    previousPhase.current = phase;
  }, [phase, client]);

  async function redetect() {
    if (!desktop || actionLock.current) return;
    actionLock.current = true;
    setAction("detect");
    setActionError(null);
    try {
      await Promise.all([
        local.refetch(),
        release.refetch(),
        installation.refetch(),
        client.invalidateQueries({ queryKey: ["osir-anywhere-codex-status"] }),
      ]);
    } finally {
      actionLock.current = false;
      setAction(null);
    }
  }
  async function launch(expectedIntent?: InstallIntent) {
    if (!desktop || actionLock.current || !installed || local.isFetching)
      return;
    if (expectedIntent && intentRef.current !== expectedIntent) return;
    // A separate explicit Open action consumes any pending automatic intent.
    if (!expectedIntent) rememberIntent(null);
    actionLock.current = true;
    setAction("launch");
    setActionError(null);
    try {
      if (expectedIntent) {
        const [detected, installer] = await Promise.all([
          local.refetch(),
          installation.refetch(),
        ]);
        const session = accountSnapshot.current;
        const current = detected.data;
        if (!mounted.current || intentRef.current !== expectedIntent) return;
        rememberIntent(null);
        if (
          !session?.connected ||
          session.generation !== expectedIntent.generation ||
          Date.now() >= expectedIntent.expiresAt ||
          detected.isError ||
          installer.isError ||
          installer.data?.phase !== "waiting_installation" ||
          !installer.data.installationComplete ||
          installer.data.installId !== expectedIntent.installId ||
          installer.data.targetVersion !== expectedIntent.version ||
          installer.data.targetRevision !== expectedIntent.revision ||
          current?.status !== "installed" ||
          !current.installed ||
          !current.path ||
          !current.version ||
          !validAnywhereBuild(current.version, current.revision) ||
          compareAnywhereBuild(
            current.version,
            current.revision,
            expectedIntent.version,
            expectedIntent.revision,
          ) !== 0
        ) {
          setActionError(
            "安装尚未确认，已停止自动连接。请重新检测后点击打开。",
          );
          return;
        }
      }
      const result = await invoke<{ foreground: boolean }>(
        "osir_anywhere_launch",
      );
      if (
        !mounted.current ||
        (expectedIntent &&
          accountSnapshot.current?.generation !== expectedIntent.generation)
      )
        return;
      if (!result?.foreground) throw new Error("unconfirmed");
      toast.success("OSIR Anywhere 已打开，正在准备账号连接和实例。");
    } catch (error) {
      const message =
        typeof error === "string"
          ? error
          : error instanceof Error
            ? error.message
            : "";
      setActionError(
        ANYWHERE_LAUNCH_ERRORS.has(message)
          ? message
          : "暂时无法确认应用已打开，请查看应用或重新检测后重试。",
      );
    } finally {
      await client.invalidateQueries({ queryKey: ANYWHERE_LOCAL_QUERY });
      actionLock.current = false;
      if (mounted.current) setAction(null);
    }
  }
  const canInstall =
    desktop &&
    (!installed || updateAvailable) &&
    !local.isError &&
    ["installed", "not_installed"].includes(local.data?.status ?? "") &&
    release.data?.status === "ready" &&
    !release.isError &&
    !release.isFetching &&
    !installation.isPending &&
    !installation.isError &&
    !local.isFetching &&
    !["downloading", "ready", "opening"].includes(phase ?? "") &&
    (phase !== "waiting_installation" || !installed || updateAvailable);
  async function install() {
    if (!canInstall || actionLock.current) return;
    const expectedInstallId = installation.data?.installId;
    if (
      expectedInstallId == null ||
      !Number.isSafeInteger(expectedInstallId) ||
      expectedInstallId < 0
    )
      return;
    actionLock.current = true;
    setAction("install");
    setActionError(null);
    const session = accountSnapshot.current;
    const targetVersion = release.data?.version;
    const targetRevision = release.data?.revision ?? 0;
    // Only this button grants a single, account-bound automatic connection.
    // Baseline satisfaction is NOT proof that a same-build reinstall finished.
    let intent: InstallIntent | null =
      session?.connected &&
      targetVersion &&
      validAnywhereBuild(targetVersion, targetRevision)
        ? {
            version: targetVersion,
            revision: targetRevision,
            generation: session.generation,
            wasInstalled: !!installed,
            alreadySatisfied:
              !!installed &&
              !!local.data?.version &&
              compareAnywhereBuild(
                local.data.version,
                local.data.revision ?? 0,
                targetVersion,
                targetRevision,
              ) >= 0,
            waitingSince: null,
            expiresAt: Date.now() + 2 * INSTALL_WAIT_MS,
            installId: null,
          }
        : null;
    rememberIntent(intent);
    try {
      await client.cancelQueries({ queryKey: ANYWHERE_INSTALL_QUERY });
      const before = await local.refetch();
      if (!mounted.current || (intent && intentRef.current !== intent)) return;
      if (
        before.isError ||
        !before.data ||
        !["installed", "not_installed"].includes(before.data.status)
      )
        throw new Error("unconfirmed installation");
      const present =
        before.data.status === "installed" && before.data.installed;
      if (
        present &&
        (!before.data.version ||
          !validAnywhereBuild(before.data.version, before.data.revision))
      )
        throw new Error("unconfirmed version");
      if (
        present &&
        targetVersion &&
        compareAnywhereBuild(
          before.data.version!,
          before.data.revision!,
          targetVersion,
          targetRevision,
        ) >= 0
      ) {
        rememberIntent(null);
        setActionError("已检测到当前或更新版本，请点击打开 Anywhere。");
        return;
      }
      if (intent) {
        intent = { ...intent, wasInstalled: present, alreadySatisfied: false };
        rememberIntent(intent);
      }
      const result = await invoke<AnywhereInstall>("osir_anywhere_install", {
        expectedInstallId,
      });
      if (!mounted.current) return;
      if (
        intent &&
        intentRef.current === intent &&
        accountSnapshot.current?.connected &&
        accountSnapshot.current.generation === intent.generation &&
        result.phase === "downloading" &&
        Number.isSafeInteger(result.installId) &&
        result.installId === expectedInstallId + 1
      ) {
        intent = { ...intent, installId: result.installId };
        rememberIntent(intent);
      } else if (intent) {
        rememberIntent(null);
      }
      if (["error", "cancelled", "idle"].includes(result.phase))
        rememberIntent(null);
      client.setQueryData(ANYWHERE_INSTALL_QUERY, result);
      if (result.phase !== "downloading")
        await client.invalidateQueries({ queryKey: ANYWHERE_LOCAL_QUERY });
    } catch {
      if (!mounted.current) return;
      rememberIntent(null);
      setActionError("安装未完成，请重新检测下载状态后重试。");
      await installation.refetch();
    } finally {
      actionLock.current = false;
      if (mounted.current) setAction(null);
    }
  }
  async function cancel() {
    if (!desktop || actionLock.current) return;
    if (phase === "waiting_installation" && intentRef.current) {
      rememberIntent(null);
      setActionError(null);
      return;
    }
    if (phase !== "downloading") return;
    actionLock.current = true;
    setAction("cancel");
    rememberIntent(null);
    setActionError(null);
    try {
      await client.cancelQueries({ queryKey: ANYWHERE_INSTALL_QUERY });
      client.setQueryData(
        ANYWHERE_INSTALL_QUERY,
        await invoke<AnywhereInstall>("osir_anywhere_cancel_install"),
      );
    } catch {
      setActionError("取消未确认，请重新检测下载状态。");
      await installation.refetch();
    } finally {
      actionLock.current = false;
      setAction(null);
    }
  }
  useEffect(() => {
    const intent = intentRef.current;
    if (!intent) return;
    // Cached terminal states belong to the previous native generation
    // until install() binds this button's intent to the returned install ID.
    // After binding, terminal states still revoke conservatively even if the
    // status ID is unexpected; auto-launch retains its exact ID checks below.
    const installStarted = intent.installId !== null;
    if (
      local.isError ||
      installation.isError ||
      local.data?.status === "error" ||
      local.data?.status === "unsupported" ||
      (installStarted && (phase === "cancelled" || phase === "error")) ||
      (intent.wasInstalled && local.data?.status === "not_installed") ||
      (!action && phase === "idle")
    ) {
      rememberIntent(null);
      return;
    }
    if (
      phase === "waiting_installation" &&
      intent.waitingSince === null &&
      !action
    ) {
      rememberIntent({
        ...intent,
        waitingSince: Date.now(),
        expiresAt: Date.now() + INSTALL_WAIT_MS,
      });
      // Refetch after handoff; a pre-existing cached installation cannot qualify.
      void local.refetch();
    }
  }, [
    phase,
    action,
    local.isError,
    local.data,
    installation.isError,
    installation.data?.installId,
    rememberIntent,
  ]);
  useEffect(() => {
    if (!installIntent) return;
    const timer = setTimeout(
      () => {
        if (intentRef.current !== installIntent) return;
        rememberIntent(null);
        setActionError(
          "未能确认安装完成，已停止自动连接。完成安装后请重新检测并点击打开。",
        );
      },
      Math.max(0, installIntent.expiresAt - Date.now()),
    );
    return () => clearTimeout(timer);
  }, [installIntent, rememberIntent]);
  useEffect(() => {
    const intent = intentRef.current;
    const session = accountSnapshot.current;
    if (
      !intent ||
      intent.waitingSince === null ||
      intent.alreadySatisfied ||
      !desktop ||
      action ||
      actionLock.current ||
      !session?.connected ||
      session.generation !== intent.generation ||
      Date.now() >= intent.expiresAt ||
      phase !== "waiting_installation" ||
      !installation.data?.installationComplete ||
      installation.data.installId !== intent.installId ||
      installation.data.targetVersion !== intent.version ||
      installation.data.targetRevision !== intent.revision ||
      installation.isError ||
      local.isFetching ||
      !installed ||
      local.dataUpdatedAt < intent.waitingSince ||
      !local.data?.version ||
      !local.data.path ||
      !validAnywhereBuild(local.data.version, local.data.revision)
    )
      return;
    // Exact target identity, not >=: an unrelated later/manual installation
    // must not inherit this button's connection authorization.
    if (
      compareAnywhereBuild(
        local.data.version,
        local.data.revision!,
        intent.version,
        intent.revision,
      ) === 0
    ) {
      void launch(intent);
    }
  }, [
    installIntent,
    desktop,
    phase,
    action,
    installed,
    local.isFetching,
    local.data,
    local.dataUpdatedAt,
    installation.isError,
    installation.data,
  ]);
  const detectionFailed = local.isError || local.data?.status === "error";
  let label = "检测中";
  let description = "正在检查本机安装状态…";
  if (!desktop) {
    label = "桌面预览";
    description = "请在桌面客户端检测、安装和打开 OSIR Anywhere。";
  } else if (detectionFailed) {
    label = "检测异常";
    description = "本机检测失败，请重新检测。";
  } else if (local.data?.status === "unsupported") {
    label = "平台暂不支持";
    description = "当前系统暂不支持安装 OSIR Anywhere。";
  } else if (installation.isError || phase === "error") {
    label = "安装异常";
    const installError = installation.data?.error ?? "";
    description = installation.isError
      ? "下载状态读取失败，请重新检测后重试。"
      : ANYWHERE_INSTALL_ERRORS.has(installError)
        ? installError
        : "下载或安装未完成，请检查网络、磁盘空间与系统权限后重试。";
  } else if (phase === "downloading") {
    label = "下载中";
    description = "正在下载安装包，完成后将打开安装器。";
  } else if (phase === "opening") {
    label = "正在打开安装器";
    description = installIntent
      ? "请完成系统确认，安装完成后将自动检测并打开。"
      : "请完成系统确认，安装后重新检测并点击打开。";
  } else if (
    phase === "waiting_installation" &&
    (!installed || installIntent)
  ) {
    label = "等待安装完成";
    description = installIntent
      ? "请完成安装；macOS 请拖入 Applications。确认安装完成后将自动打开并连接。"
      : installed
        ? "正在确认本次安装结果。"
        : "当前未检测到已安装的 Anywhere，可重新下载安装包继续安装。";
  } else if (phase === "ready") {
    label = "正在准备安装";
    description = "下载已完成，正在检查安装包，随后将打开安装器。";
  } else if (installed) {
    label = updateAvailable
      ? "有修订更新"
      : local.data?.runtimeStatus === "running"
        ? "运行中"
        : "已安装";
    description = updateAvailable
      ? "更新 Anywhere 后，使用当前账号和已配置模型直接连接。"
      : "打开后使用当前 OSIR 账号连接本机，自动添加已配置的工具实例。";
  } else if (
    local.data?.status === "not_installed" &&
    !release.isPending &&
    !installation.isPending
  ) {
    if (release.isError || release.data?.status === "error") {
      label = "发行信息异常";
      const releaseError = release.data?.error ?? "";
      description =
        !release.isError && ANYWHERE_INSTALL_ERRORS.has(releaseError)
          ? releaseError
          : "暂时无法获取安装包信息，请重试。";
    } else if (release.data?.status === "unsupported") {
      label = "平台暂不支持";
      description = "当前系统或处理器暂没有可用安装包。";
    } else if (release.data?.status === "not_configured") {
      label = "即将开放";
      description = "正式安装包尚未开放下载，请稍后再试。";
    } else if (release.data?.status === "ready") {
      label = phase === "cancelled" ? "已取消" : "未安装";
      description =
        phase === "cancelled"
          ? "下载已取消，可重新开始安装。"
          : "安装桌面应用，跨设备继续 AI 任务。";
    }
  }
  return {
    desktop,
    local,
    release,
    installation,
    installed,
    updateAvailable,
    label,
    description,
    displayPath: detectionFailed
      ? previousInstallation.current?.path
      : local.data?.path,
    detectionFailed,
    action,
    actionError,
    canInstall,
    autoConnectPending: !!installIntent,
    redetect,
    launch,
    install,
    cancel,
    retryingWaitingInstall: phase === "waiting_installation" && !installed,
  };
}

function validAnywhereBuild(
  version: string,
  revision: number | null | undefined,
): revision is number {
  return (
    /^\d+\.\d+\.\d+$/.test(version) &&
    Number.isInteger(revision) &&
    revision != null &&
    revision >= 0 &&
    revision <= 65535
  );
}

function compareAnywhereBuild(
  left: string,
  leftRevision: number,
  right: string,
  rightRevision: number,
): number {
  const parts = (value: string) =>
    /^\d+\.\d+\.\d+$/.test(value) ? value.split(".").map(Number) : null;
  const a = parts(left),
    b = parts(right);
  if (!a || !b) return 0;
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i] ? 1 : -1;
  return leftRevision - rightRevision;
}

type AnywhereController = ReturnType<typeof useAnywhereController>;
const AnywhereContext = createContext<AnywhereController | null>(null);
export function OsirAnywhereProvider({
  desktop,
  children,
}: {
  desktop: boolean;
  children: ReactNode;
}) {
  const value = useAnywhereController(desktop);
  return (
    <AnywhereContext.Provider value={value}>
      {children}
    </AnywhereContext.Provider>
  );
}
export function useOsirAnywhere() {
  const value = useContext(AnywhereContext);
  if (!value) throw new Error("OSIR Anywhere provider missing");
  return value;
}
