// CI-only check of the installed Windows application's real WebView2 renderer.
// Debugging is enabled by the owning smoke process, never by production code.
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { spawnSync } from "node:child_process";

export function debuggerEndpoint(value, port) {
  const url = new URL(value);
  assert.equal(url.protocol, "ws:");
  assert.equal(url.hostname, "127.0.0.1");
  assert.equal(url.port, String(port));
  assert.equal(url.username + url.password + url.search + url.hash, "");
  assert.match(url.pathname, /^\/devtools\/page\/[\w-]+$/);
  return url.href;
}

export function applicationPage(page) {
  if (page.type !== "page") return false;
  try {
    const url = new URL(page.url);
    return (
      ["http:", "https:"].includes(url.protocol) &&
      url.host === "tauri.localhost" &&
      ["/", "/index.html"].includes(url.pathname) &&
      !url.username &&
      !url.password &&
      !url.search &&
      !url.hash
    );
  } catch {
    return false;
  }
}

export function assertReady(state, width, height) {
  assert.equal(state.ready, true, "Welcome screen is not ready");
  assert.equal(state.logoLoaded, true, "Brand image did not load");
  assert.equal(
    state.focused,
    true,
    "Login action cannot receive keyboard focus",
  );
  assert.equal(state.overflow, false, "Renderer overflows the window");
  assert.equal(state.controlsFit, true, "Login action is outside the window");
  assert.equal(state.visible, true, "Welcome screen is hidden");
  assert.equal(
    state.clickable,
    true,
    "Login action is covered by another element",
  );
  assert.equal(state.width, width);
  assert.equal(state.height, height);
}

export function assertRendererEvidence(report, manifest) {
  assert.equal(report.phase, "reopen");
  assert.equal(report.launchRun?.phase, "launch");
  assert.notEqual(report.debugPort, report.launchRun.debugPort);
  for (const run of [report.launchRun, report]) {
    assert.equal(run.passed, true);
    assert.equal(run.version, manifest.version);
    assert.equal(run.sourceSha256, manifest.sourceProvenance.sourceSha256);
    assert.equal(run.nativeIpc?.passed, true);
    assert.equal(run.uncaughtExceptions, 0);
    assert.equal(run.nativeWindowResized, true);
    assert.equal(run.viewports.length, 2);
    for (const [index, [width, height]] of [
      [1100, 720],
      [900, 600],
    ].entries()) {
      const view = run.viewports[index];
      assert.equal(view.passed, true);
      assertReady(view, width, height);
      assert.equal(
        view.screenshot,
        `windows-renderer-${run.phase === "launch" ? "launch-" : ""}${width}x${height}.png`,
      );
    }
  }
}

async function connect(url) {
  const socket = new WebSocket(url);
  await new Promise((done, fail) => {
    const timer = setTimeout(() => {
      socket.close();
      fail(new Error("Installed renderer debugger did not connect"));
    }, 10000);
    socket.addEventListener(
      "open",
      () => {
        clearTimeout(timer);
        done();
      },
      { once: true },
    );
    socket.addEventListener(
      "error",
      () => {
        clearTimeout(timer);
        fail(new Error("Installed renderer debugger failed"));
      },
      { once: true },
    );
  });
  let serial = 0;
  const pending = new Map();
  let exceptions = 0;
  socket.addEventListener("message", ({ data }) => {
    const row = JSON.parse(String(data));
    if (row.method === "Runtime.exceptionThrown") exceptions++;
    pending.get(row.id)?.(row);
  });
  return {
    exceptions: () => exceptions,
    async call(method, params = {}) {
      const id = ++serial;
      return await new Promise((done, fail) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          fail(new Error(`Renderer command timed out: ${method}`));
        }, 10000);
        pending.set(id, (row) => {
          clearTimeout(timer);
          pending.delete(id);
          if (row.error) fail(new Error(`Renderer command failed: ${method}`));
          else done(row.result);
        });
        socket.send(JSON.stringify({ id, method, params }));
      });
    },
    close: () => socket.close(),
  };
}

const welcomeState = `(() => {
  const button = document.querySelector('.osir-welcome-login');
  const logo = document.querySelector('.osir-welcome-logo img');
  if (button) button.focus();
  const rect = button?.getBoundingClientRect();
  const visible = node => {
    if (!node) return false;
    for (let current = node; current; current = current.parentElement) {
      const style = getComputedStyle(current);
      if (style.display === 'none' || style.visibility !== 'visible' || Number(style.opacity) <= 0.01) return false;
    }
    const bounds = node.getBoundingClientRect();
    return bounds.width > 0 && bounds.height > 0;
  };
  const hit = rect && document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
  return {
    ready: !!button && !button.disabled && button.textContent.trim() === '授权登录OSIRAPI',
    logoLoaded: !!logo && logo.complete && logo.naturalWidth > 0,
    focused: !!button && document.activeElement === button,
    overflow: document.documentElement.scrollWidth > innerWidth || document.documentElement.scrollHeight > innerHeight,
    controlsFit: !!rect && rect.width > 0 && rect.height > 0 && rect.left >= 0 && rect.top >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight,
    visible: visible(button) && visible(logo),
    clickable: !!hit && (hit === button || button.contains(hit)),
    width: innerWidth, height: innerHeight
  };
})()`;

function resizeNativeWindow(handle, width, height) {
  assert.match(String(handle), /^[1-9][0-9]{0,18}$/);
  // Resize the owned Win32 client area; emulating a viewport would miss native
  // minimum-size and frame/DPI behavior in the delivered application.
  const script = `
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class OsirResize {
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out Rect r);
  [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr h, out Rect r);
  [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int height, uint flags);
  public static bool Resize(long handle, int width, int height) {
    var h = new IntPtr(handle); Rect outer, client;
    if (!GetWindowRect(h, out outer) || !GetClientRect(h, out client)) return false;
    var dpi = GetDpiForWindow(h); if (dpi == 0) return false;
    var w = (int)Math.Round(width * dpi / 96.0) + (outer.Right - outer.Left) - (client.Right - client.Left);
    var heightWithFrame = (int)Math.Round(height * dpi / 96.0) + (outer.Bottom - outer.Top) - (client.Bottom - client.Top);
    return SetWindowPos(h, IntPtr.Zero, 0, 0, w, heightWithFrame, 6);
  }
}
'@
if (-not [OsirResize]::Resize(${handle}, ${width}, ${height})) { throw 'Owned native window could not resize' }
`;
  const result = spawnSync(
    join(
      process.env.SystemRoot,
      "System32/WindowsPowerShell/v1.0/powershell.exe",
    ),
    [
      "-NoLogo",
      "-NoProfile",
      "-NonInteractive",
      "-EncodedCommand",
      Buffer.from(script, "utf16le").toString("base64"),
    ],
    { timeout: 15000, windowsHide: true, stdio: "pipe" },
  );
  assert.ok(
    !result.error && result.status === 0,
    "Owned native window could not resize",
  );
}

export async function checkRenderer(port, directory, phase, windowHandle) {
  assert.equal(process.platform, "win32");
  assert.equal(process.env.GITHUB_ACTIONS, "true");
  assert.equal(process.env.CI, "true");
  assert.ok(Number.isInteger(port) && port > 1023 && port <= 65535);
  assert.ok(["launch", "reopen"].includes(phase));
  const manifest = JSON.parse(
    readFileSync(join(directory, "build-manifest.json"), "utf8"),
  );
  const report = {
    schemaVersion: 1,
    passed: false,
    version: manifest.version,
    sourceSha256: manifest.sourceProvenance.sourceSha256,
    scope: "installed-application-renderer-and-read-only-native-IPC",
    phase,
    debugPort: port,
    nativeWindowResized: true,
    viewports: [],
  };
  const deadline = Date.now() + 45000;
  let client;
  try {
    if (phase === "reopen") {
      const first = JSON.parse(
        readFileSync(join(directory, "windows-renderer-smoke.json"), "utf8"),
      );
      assert.equal(first.phase, "launch");
      assert.equal(first.passed, true);
      assert.equal(first.version, report.version);
      assert.equal(first.sourceSha256, report.sourceSha256);
      assert.notEqual(
        first.debugPort,
        port,
        "Reopen must use its own debugger endpoint",
      );
      report.launchRun = first;
    }
    while (!client && Date.now() < deadline) {
      try {
        const response = await fetch(`http://127.0.0.1:${port}/json/list`, {
          signal: AbortSignal.timeout(2000),
        });
        const pages = await response.json();
        const candidates = pages.filter(applicationPage);
        assert.ok(
          candidates.length <= 1,
          "Ambiguous installed application pages",
        );
        const page = candidates[0];
        if (page)
          client = await connect(
            debuggerEndpoint(page.webSocketDebuggerUrl, port),
          );
      } catch {
        /* WebView2 may still be starting. */
      }
      if (!client) await delay(250);
    }
    assert.ok(client, "Installed WebView2 renderer did not become available");
    await client.call("Runtime.enable");
    const evaluate = async (expression) => {
      const result = await client.call("Runtime.evaluate", {
        expression,
        returnByValue: true,
        awaitPromise: true,
      });
      assert.ok(
        !result.exceptionDetails,
        "Renderer evaluation or native IPC failed",
      );
      return result.result.value;
    };
    let state;
    while (Date.now() < deadline) {
      state = await evaluate(welcomeState);
      if (state.ready) break;
      await delay(250);
    }
    assert.equal(
      state?.ready,
      true,
      "Installed app is blank or login remains unavailable",
    );
    const native = await evaluate(`(async () => {
      const invoke = window.__TAURI_INTERNALS__.invoke;
      const distribution = await invoke('get_update_distribution');
      const account = await invoke('osir_account_session');
      return {updatesEnabled: distribution.updatesEnabled, signedOut: account.connected === false, recovery: account.recovery};
    })()`);
    assert.equal(
      native.updatesEnabled,
      true,
      "Installed update capability is unavailable",
    );
    assert.equal(
      native.signedOut,
      true,
      "Smoke must use a fresh isolated account",
    );
    assert.ok(
      !["pending", "restoring", "blocked", "unlock_required"].includes(
        native.recovery,
      ),
    );
    report.nativeIpc = {
      passed: true,
      updatesEnabled: true,
      freshAccount: true,
    };
    for (const [width, height] of [
      [1100, 720],
      [900, 600],
    ]) {
      resizeNativeWindow(windowHandle, width, height);
      await evaluate(
        "new Promise(done => requestAnimationFrame(() => requestAnimationFrame(() => done(true))))",
      );
      state = await evaluate(welcomeState);
      assertReady(state, width, height);
      const screenshot = await client.call("Page.captureScreenshot", {
        format: "png",
        captureBeyondViewport: false,
      });
      const name = `windows-renderer-${phase === "launch" ? "launch-" : ""}${width}x${height}.png`;
      writeFileSync(
        join(directory, name),
        Buffer.from(screenshot.data, "base64"),
      );
      report.viewports.push({
        width,
        height,
        passed: true,
        screenshot: name,
        ...state,
      });
    }
    assert.equal(
      client.exceptions(),
      0,
      "Renderer reported an uncaught exception",
    );
    report.uncaughtExceptions = 0;
    report.passed = true;
    return report;
  } finally {
    client?.close();
    writeFileSync(
      join(directory, "windows-renderer-smoke.json"),
      JSON.stringify(report, null, 2) + "\n",
    );
  }
}

if (
  process.argv[1] &&
  pathToFileURL(resolve(process.argv[1])).href === import.meta.url
) {
  try {
    const [port, directory, phase, windowHandle] = process.argv.slice(2);
    await checkRenderer(Number(port), resolve(directory), phase, windowHandle);
    console.log(
      "Installed renderer passed: welcome, logo, keyboard focus, native IPC and both window sizes.",
    );
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
