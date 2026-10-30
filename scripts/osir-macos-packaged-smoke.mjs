#!/usr/bin/env node
// Native Intel CI lifecycle evidence. All app data belongs to a temporary home.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  closeSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { join, resolve, dirname } from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";

assert.equal(process.platform, "darwin");
assert.equal(process.arch, "x64");
assert.equal(process.env.GITHUB_ACTIONS, "true");
assert.equal(process.env.CI, "true");
const directory = resolve(process.argv[2] || "");
const repo = fileURLToPath(new URL("../", import.meta.url));
assert.ok(directory.startsWith(join(repo, "release/osir/")));
assert.ok(
  lstatSync(directory).isDirectory() && !lstatSync(directory).isSymbolicLink(),
);
const manifestPath = join(directory, "build-manifest.json");
const manifest = JSON.parse(readFileSync(manifestPath));
assert.equal(manifest.target, "x86_64-apple-darwin");
assert.equal(
  manifest.sourceProvenance.sourceSha256,
  process.env.OSIR_SOURCE_SHA256,
);
assert.equal(manifest.sourceProvenance.git.head, process.env.GITHUB_SHA);
const apps = readdirSync(directory).filter((name) => name.endsWith(".app"));
assert.equal(apps.length, 1);
const app = join(directory, apps[0]),
  binary = join(app, "Contents/MacOS/osir_client");
const digest = (path) =>
  createHash("sha256").update(readFileSync(path)).digest("hex");
function run(command, args, timeout = 15000) {
  const result = spawnSync(command, args, { encoding: "utf8", timeout });
  assert.equal(result.status, 0, command + " failed");
  return result.stdout.trim();
}
run("/usr/bin/codesign", ["--verify", "--deep", "--strict", app]);
assert.equal(run("/usr/bin/lipo", ["-archs", binary]), "x86_64");
const temporary = mkdtempSync(
  join(process.env.RUNNER_TEMP, "osir-intel-smoke-"),
);
const home = join(temporary, "isolated-home");
mkdirSync(home, { mode: 0o700 });
const helper = join(temporary, "window-probe");
run(
  "/usr/bin/xcrun",
  ["swiftc", join(repo, "scripts/osir-macos-window-probe.swift"), "-o", helper],
  60000,
);
const protectedPaths = [
  ".osir_client",
  ".cc-switch",
  ".codex",
  ".claude",
  ".gemini",
  ".config/opencode",
  "Library/Application Support/com.osir.client",
].map((name) => join(homedir(), name));
const sockets = [
  "/private/tmp/com_osir_client_si.sock",
  "/tmp/com_osir_client_si.sock",
];
const filters = (type, paths) =>
  paths.map((path) => "(" + type + " " + JSON.stringify(path) + ")").join(" ");
const policy =
  "(version 1)(allow default)(deny network-outbound " +
  filters("literal", sockets) +
  ")(deny file-write* " +
  filters("literal", sockets) +
  ")(deny file-read* " +
  filters("subpath", protectedPaths) +
  ")(deny file-write* " +
  filters("subpath", protectedPaths) +
  ")";
const env = Object.fromEntries(
  [
    "PATH",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "USER",
    "LOGNAME",
    "SHELL",
    "__CF_USER_TEXT_ENCODING",
  ]
    .filter((name) => process.env[name])
    .map((name) => [name, process.env[name]]),
);
Object.assign(env, {
  HOME: home,
  USERPROFILE: home,
  CC_SWITCH_TEST_HOME: home,
  OSIR_TEST_HOME: home,
  CODEX_HOME: join(home, ".codex"),
  CLAUDE_CONFIG_DIR: join(home, ".claude"),
  OPENCODE_CONFIG_DIR: join(home, ".config/opencode"),
  XDG_CONFIG_HOME: join(home, ".config"),
  XDG_DATA_HOME: join(home, ".local/share"),
});
const report = {
  schemaVersion: 1,
  passed: false,
  target: manifest.target,
  version: manifest.version,
  sourceSha256: manifest.sourceProvenance.sourceSha256,
  gitHead: process.env.GITHUB_SHA,
  executableSha256: digest(binary),
  hostArchitecture: process.arch,
  isolatedHome: true,
  productionDataBlocked: true,
  productionSocketBlocked: true,
  rendererAndNativeIpcCovered: false,
  stages: [],
  startedAt: new Date().toISOString(),
};
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
async function lifecycle(phase) {
  const fd = openSync(join(directory, "macos-" + phase + ".log"), "wx", 0o600);
  const child = spawn("/usr/bin/sandbox-exec", ["-p", policy, binary], {
    env,
    cwd: dirname(app),
    stdio: ["ignore", fd, fd],
  });
  closeSync(fd);
  const stage = { phase, pid: child.pid, passed: false };
  report.stages.push(stage);
  const exited = new Promise((done) =>
    child.once("exit", (code, signal) => done({ code, signal })),
  );
  try {
    let observed;
    for (let attempt = 0; attempt < 80; attempt++) {
      assert.equal(
        child.exitCode,
        null,
        "App exited before a product window appeared",
      );
      assert.equal(
        child.signalCode,
        null,
        "App ended before window observation",
      );
      const current = JSON.parse(
        run(helper, [String(child.pid), manifest.identifier]),
      );
      if (current.windows.length > 0) {
        observed = current;
        break;
      }
      await pause(500);
    }
    assert.ok(observed, "Native product window was not observed");
    assert.equal(observed.bundleIdentifier, manifest.identifier);
    assert.equal(observed.bundlePath, app);
    assert.deepEqual(observed.otherInstancePids, []);
    stage.observation = observed;
    stage.windowObserved = true;
    await pause(3000);
    assert.equal(child.exitCode, null);
    assert.equal(child.signalCode, null);
    stage.remainedRunning = true;
    assert.ok(
      existsSync(join(home, ".osir_client/osir_client.db")),
      "Isolated database was not initialized",
    );
    stage.isolatedDatabaseCreated = true;
    if (observed.screenCaptureAllowed) {
      const name = "macos-" + phase + "-window.png";
      run("/usr/sbin/screencapture", [
        "-x",
        "-l",
        String(observed.windows[0].id),
        join(directory, name),
      ]);
      assert.ok(
        readFileSync(join(directory, name))
          .subarray(0, 8)
          .equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])),
      );
      stage.screenshot = {
        name,
        sha256: digest(join(directory, name)),
        bytes: lstatSync(join(directory, name)).size,
      };
    } else
      stage.screenshotScope = "not-covered-runner-screen-capture-permission";
    stage.passed = true;
  } finally {
    if (child.exitCode === null && child.signalCode === null) {
      assert.equal(
        run("/bin/ps", ["-p", String(child.pid), "-o", "comm="]),
        binary,
      );
      child.kill("SIGTERM");
    }
    const result = await Promise.race([exited, pause(10000).then(() => null)]);
    if (!result) {
      assert.equal(
        run("/bin/ps", ["-p", String(child.pid), "-o", "comm="]),
        binary,
      );
      child.kill("SIGKILL");
      await exited;
    }
    stage.stoppedOnlyVerifiedPid = true;
    stage.stoppedAt = new Date().toISOString();
  }
}
try {
  await lifecycle("launch");
  await lifecycle("reopen");
  run("/usr/bin/codesign", ["--verify", "--deep", "--strict", app]);
  assert.equal(digest(binary), report.executableSha256);
  report.passed = true;
} finally {
  report.finishedAt = new Date().toISOString();
  const name = "macos-packaged-smoke.json";
  writeFileSync(join(directory, name), JSON.stringify(report, null, 2) + "\n");
  if (report.passed) {
    manifest.packagedMacAcceptance = {
      name,
      sha256: digest(join(directory, name)),
      bytes: lstatSync(join(directory, name)).size,
      scope: "native-intel-window-launch-reopen-no-renderer-ipc",
    };
    writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
  }
}
console.log(
  "Native Intel app launch, product window, reopen and owned-process cleanup passed.",
);
