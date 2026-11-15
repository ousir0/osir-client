import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  OsirAnywhereProvider,
  useOsirAnywhere,
  ANYWHERE_INSTALL_QUERY,
  ANYWHERE_LOCAL_QUERY,
  type AnywhereInstall,
  type AnywhereState,
  type AnywhereRelease,
} from "@/features/osir/useOsirAnywhere";

const native = vi.hoisted(() => ({ invoke: vi.fn(), success: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@/lib/toast", () => ({ toast: { success: native.success } }));
const sessionKey = ["osir-account", "session"];
const absent: AnywhereState = {
  status: "not_installed",
  installed: false,
  version: null,
  revision: null,
  path: null,
  error: null,
  runtimeStatus: "stopped",
};
const old: AnywhereState = {
  ...absent,
  status: "installed",
  installed: true,
  version: "2.2.1",
  revision: 0,
  path: "/fixture/Applications/OSIR Anywhere.app",
};
const target: AnywhereState = { ...old, revision: 1 };
const released: AnywhereRelease = {
  status: "ready",
  version: "2.2.1",
  revision: 1,
  platform: "macos",
  arch: "arm64",
  size: 1000,
  error: null,
};
const idle: AnywhereInstall = {
  phase: "idle",
  downloadedBytes: 0,
  totalBytes: null,
  error: null,
  installId: 0,
  targetVersion: null,
  targetRevision: null,
  installationComplete: false,
};
let local: AnywhereState;
let status: AnywhereInstall;
let client: QueryClient;
let controller: ReturnType<typeof useOsirAnywhere>;
let detectFailure: boolean;
let statusFailure: boolean;
let launchFailure: boolean;
function Probe() {
  controller = useOsirAnywhere();
  return null;
}
function show() {
  return render(
    <QueryClientProvider client={client}>
      <OsirAnywhereProvider desktop>
        <Probe />
      </OsirAnywhereProvider>
    </QueryClientProvider>,
  );
}
const count = (command: string) =>
  native.invoke.mock.calls.filter(([c]) => c === command).length;
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
async function advance(ms = 20) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
  // Queries enabled by the preceding render complete on the next notify tick.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10);
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10);
  });
}
async function begin() {
  const view = show();
  await advance();
  expect(controller.canInstall).toBe(true);
  await act(async () => {
    await controller.install();
  });
  await advance();
  return view;
}
async function handoff() {
  status = {
    ...status,
    phase: "waiting_installation",
    targetVersion: released.version,
    targetRevision: released.revision!,
    installationComplete:
      local.version === released.version &&
      local.revision === released.revision,
  };
  await advance(800);
}
function completeTargetInstall() {
  local = target;
  status = { ...status, installationComplete: true };
}
async function refresh() {
  await act(async () => {
    await client.refetchQueries({ queryKey: ANYWHERE_LOCAL_QUERY });
    await client.refetchQueries({ queryKey: ANYWHERE_INSTALL_QUERY });
  });
  await advance();
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  local = absent;
  status = idle;
  detectFailure = statusFailure = launchFailure = false;
  client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  client.setQueryData(sessionKey, { connected: true, generation: 10 });
  native.invoke.mockImplementation(async (command: string) => {
    if (command === "osir_anywhere_detect") {
      if (detectFailure) throw Error("private fixture");
      return local;
    }
    if (command === "osir_anywhere_release") return released;
    if (command === "osir_anywhere_install_status") {
      if (statusFailure) throw Error("private fixture");
      return status;
    }
    if (command === "osir_anywhere_install")
      return (status = {
        ...idle,
        phase: "downloading",
        installId: status.installId + 1,
        targetVersion: released.version,
        targetRevision: released.revision!,
      });
    if (command === "osir_anywhere_cancel_install")
      return (status = { ...idle, phase: "cancelled" });
    if (command === "osir_anywhere_launch") {
      if (launchFailure) throw Error("private fixture");
      return { foreground: true };
    }
    throw Error("unexpected native command: " + command);
  });
});
afterEach(() => {
  cleanup();
  client.clear();
  vi.useRealTimers();
});

describe("Anywhere launch error reporting", () => {
  const knownErrors = [
    "本机模型信息暂时无法更新，请重试",
    "DSH 模型配置无法读取",
    "DSH 尚未启动，请检查 DSH 安装后重试",
    "OSIR Anywhere 正在打开，请稍后重试",
    "未找到可安全启动的 OSIR Anywhere，请先安装或重新检查",
    "OSIR Anywhere 安装状态已改变，请重新检查",
    "无法验证应用版本，请通过 OSIR 客户端重新安装 Anywhere",
    "Anywhere 版本无法验证，请通过客户端修复安装",
    "Anywhere 账号连接未完成，请检查服务状态后重试",
    "Anywhere 连接超时，请重新打开",
    "账号已改变，已取消本次 Anywhere 连接",
    "macOS 系统策略拒绝打开 OSIR Anywhere，客户端已尝试打开“系统设置 → 隐私与安全性”。是否允许打开由你和系统决定；若有“仍要打开”选项且你信任该应用，可手动批准后重试。",
  ];
  it.each(knownErrors)(
    "preserves the known native failure: %s",
    async (message) => {
      await failLaunch(message);
      expect(controller.actionError).toBe(message);
    },
  );
  it("accepts an Error containing an exact known native failure", async () => {
    await failLaunch(new Error(knownErrors[0]));
    expect(controller.actionError).toBe(knownErrors[0]);
  });
  it.each([
    "Anywhere private-fixture-secret",
    "账号已改变 private-fixture-secret",
    "无法验证应用版本 private-fixture-secret",
    "本机模型信息暂时无法更新，请重试 private-fixture-secret",
    new Error("private-fixture-secret"),
    { message: "private-fixture-secret" },
  ])(
    "redacts unknown diagnostics instead of trusting a prefix: %s",
    async (error) => {
      await failLaunch(error);
      expect(controller.actionError).toBe(
        "暂时无法确认应用已打开，请查看应用或重新检测后重试。",
      );
    },
  );

  async function failLaunch(error: unknown) {
    local = target;
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((command: string) =>
      command === "osir_anywhere_launch"
        ? Promise.reject(error)
        : original(command),
    );
    show();
    await advance();
    await act(async () => {
      await controller.launch();
    });
    await advance();
    expect(count("osir_anywhere_launch")).toBe(1);
    expect(controller.action).toBeNull();
    expect(native.success).not.toHaveBeenCalled();
    await advance(2000);
    expect(count("osir_anywhere_launch")).toBe(1);
  }
});

describe("Anywhere installation error reporting", () => {
  it.each([
    "无法确认服务端兼容版本，请稍后重试",
    "服务端版本检查超时，请稍后重试",
    "正式发行信息暂不可用，未跟随跳转",
    "服务端版本检查不可用，已阻止不确定的安装",
  ])("shows the static preflight failure before first install: %s", async (message) => {
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((command: string) =>
      command === "osir_anywhere_release"
        ? Promise.resolve({ ...released, status: "error", error: message })
        : original(command),
    );
    show();
    await advance();
    expect(controller.description).toBe(message);
    expect(controller.label).toBe("发行信息异常");
    expect(controller.canInstall).toBe(false);
  });

  it.each([
    "private-fixture-secret",
    "无法确认服务端兼容版本，请稍后重试 token=private-fixture-secret",
    "服务端版本检查超时，请稍后重试 token=private-fixture-secret",
    "正式发行信息暂不可用，未跟随跳转 token=private-fixture-secret",
    "服务端版本检查不可用，已阻止不确定的安装 token=private-fixture-secret",
  ])("keeps preflight diagnostics generic: %s", async (message) => {
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((command: string) =>
      command === "osir_anywhere_release"
        ? Promise.resolve({ ...released, status: "error", error: message })
        : original(command),
    );
    show();
    await advance();
    expect(controller.description).toBe("暂时无法获取安装包信息，请重试。");
    expect(controller.canInstall).toBe(false);
  });

  it("does not expose an allowlisted cached preflight failure when IPC fails", async () => {
    const original = native.invoke.getMockImplementation()!;
    let fail = false;
    native.invoke.mockImplementation((command: string) =>
      command === "osir_anywhere_release"
        ? fail
          ? Promise.reject(Error("private-fixture-secret"))
          : Promise.resolve({
              ...released,
              status: "error",
              error: "无法确认服务端兼容版本，请稍后重试",
            })
        : original(command),
    );
    show();
    await advance();
    fail = true;
    await act(async () => {
      await client.refetchQueries({ queryKey: ["osir-anywhere-release"] });
    });
    await advance();
    expect(controller.description).toBe("暂时无法获取安装包信息，请重试。");
    expect(controller.canInstall).toBe(false);
  });

  it.each([
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
  ])("shows only the complete static native message: %s", async (message) => {
    status = { ...idle, phase: "error", error: message };
    show();
    await advance();
    expect(controller.description).toBe(message);
    expect(controller.label).toBe("安装异常");
    expect(controller.canInstall).toBe(true);
  });

  it.each([
    "private-fixture-secret",
    "安装包下载失败，请检查网络后重试 token=private-fixture-secret",
    "下载中断，请重试\nAuthorization: Bearer private-fixture-secret",
    ...[
      "无法确认服务端兼容版本，请稍后重试",
      "服务端版本检查超时，请稍后重试",
      "正式发行信息暂不可用，未跟随跳转",
      "服务端版本检查不可用，已阻止不确定的安装",
    ].map((message) => `${message} token=private-fixture-secret`),
    "安装包写入失败，请检查空间及权限 /private-fixture-secret",
    "https://fixture.invalid/download?token=private-fixture-secret",
    "",
    null,
  ])("keeps unknown native diagnostics generic: %s", async (message) => {
    status = { ...idle, phase: "error", error: message };
    show();
    await advance();
    expect(controller.description).toBe(
      "下载或安装未完成，请检查网络、磁盘空间与系统权限后重试。",
    );
    expect(controller.actionError).toBeNull();
  });

  it("does not expose even an allowlisted cached message when status IPC fails", async () => {
    status = { ...idle, phase: "error", error: "下载中断，请重试" };
    show();
    await advance();
    statusFailure = true;
    await refresh();
    expect(controller.description).toBe("下载状态读取失败，请重新检测后重试。");
    expect(controller.canInstall).toBe(false);
  });
});

describe("Anywhere button-owned automatic installation connection", () => {
  it.each(["error", "cancelled"] as const)(
    "starts a fresh native generation after %s despite an asynchronously pending detection",
    async (phase) => {
      await begin();
      status = { ...status, phase, error: "安装包下载失败，请检查网络后重试" };
      await refresh();
      expect(controller.canInstall).toBe(true);
      expect(controller.autoConnectPending).toBe(false);

      const detection = deferred<AnywhereState>();
      const original = native.invoke.getMockImplementation()!;
      native.invoke.mockImplementation((command: string) =>
        command === "osir_anywhere_detect"
          ? detection.promise
          : original(command),
      );
      let retry!: Promise<void>;
      act(() => {
        retry = controller.install();
      });
      await advance(); // Let effects observe the old terminal status during IPC.
      expect(controller.action).toBe("install");
      expect(count("osir_anywhere_install")).toBe(1);
      await act(async () => {
        detection.resolve(absent);
        await retry;
      });
      native.invoke.mockImplementation(original);
      await advance();
      expect(count("osir_anywhere_install")).toBe(2);
      expect(
        native.invoke.mock.calls
          .filter(([c]) => c === "osir_anywhere_install")
          .slice(-1)[0],
      ).toEqual(["osir_anywhere_install", { expectedInstallId: 1 }]);
      expect(status.installId).toBe(2);
      expect(controller.autoConnectPending).toBe(true);
      completeTargetInstall();
      await handoff();
      await advance(1500);
      expect(count("osir_anywhere_launch")).toBe(1);
    },
  );

  it.each(["account switch", "unmount", "timeout"])(
    "does not start the retry after %s during its pending detection",
    async (reason) => {
      const view = await begin();
      status = { ...status, phase: "error" };
      await refresh();
      const detection = deferred<AnywhereState>();
      const original = native.invoke.getMockImplementation()!;
      native.invoke.mockImplementation((command: string) =>
        command === "osir_anywhere_detect"
          ? detection.promise
          : original(command),
      );
      let retry!: Promise<void>;
      act(() => {
        retry = controller.install();
      });
      await advance();
      if (reason === "account switch") {
        await act(async () => {
          client.setQueryData(sessionKey, { connected: true, generation: 11 });
        });
      } else if (reason === "unmount") view.unmount();
      else await advance(10 * 60_000 + 1000);
      await act(async () => {
        detection.resolve(absent);
        await retry;
      });
      expect(count("osir_anywhere_install")).toBe(1);
      expect(count("osir_anywhere_launch")).toBe(0);
    },
  );

  it("revokes a retried download on cancel and never auto-connects its late completion", async () => {
    await begin();
    status = { ...status, phase: "error" };
    await refresh();
    await act(async () => {
      await controller.install();
    });
    await advance();
    expect(status.installId).toBe(2);
    expect(controller.autoConnectPending).toBe(true);
    await act(async () => {
      await controller.cancel();
    });
    expect(count("osir_anywhere_cancel_install")).toBe(1);
    expect(controller.autoConnectPending).toBe(false);
    completeTargetInstall();
    status = {
      ...status,
      phase: "waiting_installation",
      installId: 2,
      targetVersion: released.version,
      targetRevision: released.revision!,
      installationComplete: true,
    };
    await refresh();
    await advance(2500);
    expect(count("osir_anywhere_launch")).toBe(0);
  });

  it("does not let an old handoff launch while a retry install IPC is pending", async () => {
    await begin();
    status = { ...status, phase: "error" };
    await refresh();
    const pendingInstall = deferred<AnywhereInstall>();
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((command: string) =>
      command === "osir_anywhere_install"
        ? pendingInstall.promise
        : original(command),
    );
    let retry!: Promise<void>;
    act(() => {
      retry = controller.install();
    });
    await advance();
    expect(count("osir_anywhere_install")).toBe(2);
    completeTargetInstall();
    status = {
      ...status,
      phase: "waiting_installation",
      installationComplete: true,
    };
    await refresh();
    expect(count("osir_anywhere_launch")).toBe(0);
    await act(async () => {
      client.setQueryData(sessionKey, { connected: true, generation: 11 });
    });
    await act(async () => {
      status = { ...idle, phase: "downloading", installId: 2 };
      pendingInstall.resolve(status);
      await retry;
    });
    expect(controller.autoConnectPending).toBe(false);
    await handoff();
    await refresh();
    await advance(2000);
    expect(count("osir_anywhere_launch")).toBe(0);
  });

  it.each(["new install", "revision upgrade", "patch upgrade"])(
    "polls after handoff and automatically launches %s exactly once without a Detect click",
    async (kind) => {
      if (kind === "revision upgrade") local = old;
      if (kind === "patch upgrade")
        local = { ...old, version: "2.2.0", revision: 9 };
      await begin();
      completeTargetInstall();
      await advance(1000); // New version alone, before installer handoff, is insufficient.
      expect(count("osir_anywhere_launch")).toBe(0);
      await handoff();
      await advance(1100);
      expect(count("osir_anywhere_launch")).toBe(1);
      await refresh();
      await advance(3000);
      expect(count("osir_anywhere_launch")).toBe(1);
    },
  );
  it("continues local detection while waiting and requires exact version AND revision", async () => {
    local = old;
    await begin();
    await handoff();
    const before = count("osir_anywhere_detect");
    await advance(1100);
    expect(count("osir_anywhere_detect")).toBeGreaterThan(before);
    expect(count("osir_anywhere_launch")).toBe(0);
    for (const wrong of [
      { ...target, revision: null },
      { ...target, version: "not-a-version" },
      { ...target, revision: 2 },
      { ...target, version: "2.2.2" },
    ]) {
      local = wrong;
      await advance(1100);
      expect(count("osir_anywhere_launch")).toBe(0);
    }
    completeTargetInstall();
    await advance(1100);
    expect(count("osir_anywhere_launch")).toBe(1);
  });
  it("never grants automatic connection from a pre-existing native waiting state or Detect", async () => {
    status = { ...idle, phase: "waiting_installation" };
    show();
    await advance();
    local = target;
    await act(async () => {
      await controller.redetect();
    });
    await advance(3000);
    expect(count("osir_anywhere_launch")).toBe(0);
    expect(controller.autoConnectPending).toBe(false);
  });
  it("restarts an orphaned wait with a new install ID and requires native completion for auto-connect", async () => {
    status = {
      ...idle,
      phase: "waiting_installation",
      installId: 9,
      targetVersion: released.version,
      targetRevision: released.revision!,
    };
    show();
    await advance();
    expect(controller.canInstall).toBe(true);
    await act(async () => {
      await controller.install();
    });
    expect(status.installId).toBe(10);
    status = {
      ...status,
      phase: "waiting_installation",
      installId: 9,
      installationComplete: true,
    };
    completeTargetInstall();
    status = { ...status, installId: 9 };
    await advance(2000);
    expect(count("osir_anywhere_launch")).toBe(0);
    status = { ...status, installId: 10, installationComplete: false };
    await advance(1200);
    expect(count("osir_anywhere_launch")).toBe(0);
    status = { ...status, installationComplete: true };
    await advance(1200);
    expect(count("osir_anywhere_launch")).toBe(1);
  });
  it("freshly checks baseline: a stale missing/old cache cannot auto-launch an already-satisfied same-build reinstall", async () => {
    local = old;
    show();
    await advance();
    expect(controller.canInstall).toBe(true);
    local = target; // Reinstalled outside this hook since the cached detection.
    await act(async () => {
      await controller.install();
    });
    await advance(3000);
    expect(count("osir_anywhere_install")).toBe(0);
    expect(count("osir_anywhere_launch")).toBe(0);
    expect(controller.autoConnectPending).toBe(false);
    expect(controller.actionError).toContain("请点击打开");
  });
  it.each([
    "cancelled",
    "error",
    "status failure",
    "detect failure",
    "uninstalled",
    "logout",
    "account switch",
    "stop automatic connection",
  ])("revokes the intent permanently on %s", async (reason) => {
    if (reason === "uninstalled") local = old;
    await begin();
    await handoff();
    if (reason === "cancelled" || reason === "error")
      status = { ...idle, phase: reason };
    if (reason === "status failure") statusFailure = true;
    if (reason === "detect failure") detectFailure = true;
    if (reason === "uninstalled") local = absent;
    if (reason === "logout" || reason === "account switch")
      await act(async () => {
        client.setQueryData(sessionKey, {
          connected: reason !== "logout",
          generation: 11,
        });
      });
    if (reason === "stop automatic connection")
      await act(async () => {
        await controller.cancel();
      });
    await advance(1200);
    expect(controller.autoConnectPending).toBe(false);
    statusFailure = detectFailure = false;
    status = { ...idle, phase: "waiting_installation" };
    local = target;
    await act(async () => {
      await controller.redetect();
    });
    await advance(2500);
    expect(count("osir_anywhere_launch")).toBe(0);
    if (reason === "stop automatic connection")
      expect(count("osir_anywhere_cancel_install")).toBe(0); // Does not pretend to cancel the OS installer.
  });
  it("revokes downloading intent before a cancellation IPC failure and never rearms on recovery", async () => {
    await begin();
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((command: string) =>
      command === "osir_anywhere_cancel_install"
        ? Promise.reject(Error("private fixture"))
        : original(command),
    );
    await act(async () => {
      await controller.cancel();
    });
    await advance();
    expect(controller.autoConnectPending).toBe(false);
    status = { ...idle, phase: "waiting_installation" };
    local = target;
    await refresh();
    await advance(2000);
    expect(count("osir_anywhere_launch")).toBe(0);
  });
  it("account switch during pending install IPC cannot rearm the intent when its late reply arrives", async () => {
    let finish!: (value: AnywhereInstall) => void;
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((command: string) =>
      command === "osir_anywhere_install"
        ? new Promise<AnywhereInstall>((resolve) => {
            finish = resolve;
          })
        : original(command),
    );
    show();
    await advance();
    let request!: Promise<void>;
    act(() => {
      request = controller.install();
    });
    await advance();
    await act(async () => {
      client.setQueryData(sessionKey, { connected: true, generation: 11 });
    });
    await act(async () => {
      status = {
        ...idle,
        phase: "downloading",
        installId: 1,
        targetVersion: released.version,
        targetRevision: released.revision!,
      };
      finish(status);
      await request;
    });
    local = target;
    status = {
      ...status,
      phase: "waiting_installation",
      installationComplete: true,
    };
    await refresh();
    await advance(2000);
    expect(count("osir_anywhere_launch")).toBe(0);
    expect(controller.autoConnectPending).toBe(false);
  });
  it("expires after five minutes of waiting and cannot revive from a later install/Detect", async () => {
    await begin();
    await handoff();
    await advance(5 * 60_000 + 1000);
    expect(controller.autoConnectPending).toBe(false);
    expect(controller.actionError).toContain("已停止自动连接");
    const before = count("osir_anywhere_detect");
    await advance(5000);
    expect(count("osir_anywhere_detect")).toBe(before);
    local = target;
    await act(async () => {
      await controller.redetect();
    });
    await advance();
    expect(count("osir_anywhere_launch")).toBe(0);
  });
  it("does not retry a failed automatic launch", async () => {
    launchFailure = true;
    await begin();
    await handoff();
    completeTargetInstall();
    await advance(1100);
    expect(count("osir_anywhere_launch")).toBe(1);
    expect(controller.autoConnectPending).toBe(false);
    await refresh();
    await advance(4000);
    expect(count("osir_anywhere_launch")).toBe(1);
    expect(native.success).not.toHaveBeenCalled();
  });
  it("does not persist authorization across provider unmount/remount", async () => {
    const view = await begin();
    await handoff();
    view.unmount();
    local = target;
    show();
    await refresh();
    await advance(3000);
    expect(count("osir_anywhere_launch")).toBe(0);
    expect(controller.autoConnectPending).toBe(false);
  });
});
