import test from "node:test";
import assert from "node:assert/strict";
import {
  debuggerEndpoint,
  applicationPage,
  assertReady,
  assertRendererEvidence,
  safeDiagnostic,
} from "./osir-packaged-renderer-check.mjs";

test("acceptance diagnostics omit paths, secrets and subsequent stack lines", () => {
  assert.equal(
    safeDiagnostic(
      "failed at C:\\Users\\runner\\app.js with sk-synthetic123456789\nprivate stack detail",
    ),
    "failed at [path] with [redacted]",
  );
  assert.equal(safeDiagnostic("x".repeat(300)).length, 240);
});

test("only the installed main document can satisfy renderer acceptance", () => {
  assert.equal(
    applicationPage({ type: "page", url: "http://tauri.localhost/" }),
    true,
  );
  assert.equal(
    applicationPage({
      type: "page",
      url: "https://tauri.localhost/index.html",
    }),
    true,
  );
  for (const url of [
    "http://tauri.localhost/tray-panel.html",
    "https://tauri.localhost.example/",
    "http://127.0.0.1/",
    "http://tauri.localhost/?fixture=true",
    "about:blank",
  ]) {
    assert.equal(applicationPage({ type: "page", url }), false);
  }
});

test("renderer debugger refuses remote hosts, credentials and wrong ports", () => {
  assert.equal(
    debuggerEndpoint("ws://127.0.0.1:9222/devtools/page/fixture", 9222),
    "ws://127.0.0.1:9222/devtools/page/fixture",
  );
  for (const url of [
    "ws://example.com:9222/devtools/page/fixture",
    "ws://127.0.0.1:9223/devtools/page/fixture",
    "ws://user:password@127.0.0.1:9222/devtools/page/fixture",
    "ws://127.0.0.1:9222/devtools/browser/fixture",
    "ws://127.0.0.1:9222/devtools/page/fixture?token=fixture",
  ])
    assert.throws(() => debuggerEndpoint(url, 9222));
});

test("a window handle cannot pass blank, locked, broken-image or clipped renderers", () => {
  const ready = {
    ready: true,
    logoLoaded: true,
    focused: true,
    overflow: false,
    controlsFit: true,
    visible: true,
    clickable: true,
    width: 900,
    height: 600,
  };
  assertReady(ready, 900, 600);
  for (const change of [
    { ready: false },
    { logoLoaded: false },
    { focused: false },
    { overflow: true },
    { controlsFit: false },
    { visible: false },
    { clickable: false },
    { height: 599 },
  ])
    assert.throws(() => assertReady({ ...ready, ...change }, 900, 600));
});

test("release evidence requires distinct native launches and both real window sizes", () => {
  const manifest = {
    version: "0.1.1",
    sourceProvenance: { sourceSha256: "synthetic-source" },
  };
  const run = (phase, debugPort) => ({
    ...manifest,
    sourceSha256: manifest.sourceProvenance.sourceSha256,
    phase,
    debugPort,
    passed: true,
    nativeIpc: { passed: true },
    uncaughtExceptions: 0,
    nativeWindowResized: true,
    viewports: [
      [1100, 720],
      [900, 600],
    ].map(([width, height]) => ({
      width,
      height,
      passed: true,
      ready: true,
      logoLoaded: true,
      focused: true,
      overflow: false,
      controlsFit: true,
      visible: true,
      clickable: true,
      screenshot: `windows-renderer-${phase === "launch" ? "launch-" : ""}${width}x${height}.png`,
    })),
  });
  const report = { ...run("reopen", 9223), launchRun: run("launch", 9222) };
  assertRendererEvidence(report, manifest);
  for (const change of [
    { debugPort: 9222 },
    { launchRun: undefined },
    { nativeWindowResized: false },
    { viewports: report.viewports.slice(0, 1) },
    { launchRun: { ...report.launchRun, sourceSha256: "another-source" } },
  ])
    assert.throws(() =>
      assertRendererEvidence({ ...report, ...change }, manifest),
    );
});
