import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  chmodSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { verifyReleaseDiagnostics } from "./osir-release-diagnostics.mjs";

const updater = JSON.parse(
  readFileSync(
    new URL("../config/osir-updater-prod.json", import.meta.url),
    "utf8",
  ),
);
const config = {
  identifier: "com.osir.client",
  version: "0.1.1",
  plugins: {
    updater: { pubkey: updater.pubkey, endpoints: [updater.endpoint] },
    "osir-updates": {
      schemaVersion: 1,
      channel: "prod",
      bundleArtifacts: true,
    },
  },
};
const report = {
  schemaVersion: 1,
  identifier: config.identifier,
  version: config.version,
  packageVersion: config.version,
  releaseBuild: true,
  bundleType: "app",
  updater: config.plugins.updater,
  releaseMarker: config.plugins["osir-updates"],
  updateDistribution: { kind: "osir", updatesEnabled: true, reason: null },
};
const expected = { config, updatesEnabled: true, bundleType: "app" };
const success = (value = report) => ({
  status: 0,
  signal: null,
  stdout: JSON.stringify(value) + "\n",
  stderr: "",
});
function fixture(action) {
  const directory = mkdtempSync(join(tmpdir(), "osir-release-diagnostics-"));
  const executable = join(directory, "osir-fixture");
  writeFileSync(executable, "fixture bytes");
  try {
    return action(executable);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("native diagnostics executes the exact flag with a bounded credential-free environment", () =>
  fixture((executable) => {
    const evidence = verifyReleaseDiagnostics(
      executable,
      expected,
      (path, args, options) => {
        assert.equal(path, executable);
        assert.deepEqual(args, ["--osir-release-diagnostics"]);
        assert.equal(options.shell, false);
        assert.deepEqual(options.stdio, ["ignore", "pipe", "pipe"]);
        assert.equal(options.windowsHide, true);
        assert.equal(options.timeout, 10000);
        assert.equal(options.maxBuffer, 65536);
        assert.ok(
          Object.keys(options.env).every((key) =>
            ["SystemRoot", "WINDIR"].includes(key),
          ),
        );
        return success();
      },
    );
    assert.deepEqual(evidence.report, report);
    assert.equal(
      evidence.executableSha256,
      createHash("sha256").update(readFileSync(executable)).digest("hex"),
    );
    assert.match(evidence.reportSha256, /^[a-f0-9]{64}$/);
  }));

test("driver rejects failed, noisy, malformed and oversized output without echoing it", () =>
  fixture((executable) => {
    const secret = "PRIVATE-MUST-NOT-BE-OUTPUT";
    const results = [
      { status: 64, stdout: secret, stderr: secret },
      { status: null, signal: "SIGTERM", error: new Error(secret) },
      { ...success(), stderr: secret },
      { ...success(), stdout: secret },
      { ...success(), stdout: JSON.stringify(report) },
      { ...success(), stdout: JSON.stringify(report) + "\n{}\n" },
      {
        ...success(),
        stdout: " ".repeat(65537) + JSON.stringify(report) + "\n",
      },
    ];
    for (const value of results)
      assert.throws(
        () => verifyReleaseDiagnostics(executable, expected, () => value),
        (error) => !error.message.includes(secret),
      );
    assert.throws(
      () =>
        verifyReleaseDiagnostics(executable, expected, () => {
          throw new Error(secret);
        }),
      (error) => !error.message.includes(secret),
    );
  }));

test("driver compares all compiled fields and real update capability exactly", () =>
  fixture((executable) => {
    const variants = [
      { ...report, schemaVersion: 2 },
      { ...report, identifier: "com.ccswitch.desktop" },
      { ...report, version: "0.1.0" },
      { ...report, packageVersion: "0.1.0" },
      { ...report, releaseBuild: false },
      { ...report, bundleType: "nsis" },
      { ...report, updater: { ...report.updater, pubkey: null } },
      { ...report, updater: { ...report.updater, endpoints: [] } },
      { ...report, releaseMarker: null },
      {
        ...report,
        releaseMarker: { ...report.releaseMarker, bundleArtifacts: false },
      },
      {
        ...report,
        updateDistribution: {
          ...report.updateDistribution,
          updatesEnabled: false,
        },
      },
      {
        ...report,
        updateDistribution: {
          ...report.updateDistribution,
          reason: "unexpected",
        },
      },
      { ...report, metadata: "PRIVATE-MUST-NOT-BE-OUTPUT" },
      {
        ...report,
        updater: { ...report.updater, token: "PRIVATE-MUST-NOT-BE-OUTPUT" },
      },
    ];
    for (const value of variants)
      assert.throws(
        () =>
          verifyReleaseDiagnostics(executable, expected, () => success(value)),
        /Compiled release configuration/,
      );
    for (const bundleType of [null, "nsis"])
      assert.equal(
        verifyReleaseDiagnostics(executable, { ...expected, bundleType }, () =>
          success({ ...report, bundleType }),
        ).report.bundleType,
        bundleType,
      );
  }));

test("driver accepts an intentionally disabled debug build and rejects changed executable bytes", () =>
  fixture((executable) => {
    const disabled = {
      ...report,
      releaseBuild: false,
      updater: { pubkey: null, endpoints: [] },
      releaseMarker: null,
      updateDistribution: {
        kind: "osir",
        updatesEnabled: false,
        reason: "OSIR 更新渠道未配置：尚无可用的 OSIR 签名更新源。",
      },
    };
    const debug = {
      ...config,
      plugins: { updater: { pubkey: "", endpoints: [] }, "osir-updates": null },
    };
    assert.equal(
      verifyReleaseDiagnostics(
        executable,
        {
          ...expected,
          config: debug,
          releaseBuild: false,
          updatesEnabled: false,
        },
        () => success(disabled),
      ).report.releaseBuild,
      false,
    );
    assert.throws(
      () =>
        verifyReleaseDiagnostics(executable, expected, () => {
          writeFileSync(executable, "changed");
          return success();
        }),
      /changed/,
    );
  }));

test("driver rejects relative paths, directories and symlinks before executing", () =>
  fixture((executable) => {
    const never = () => assert.fail("must not execute");
    assert.throws(
      () => verifyReleaseDiagnostics("relative", expected, never),
      /absolute/,
    );
    assert.throws(
      () => verifyReleaseDiagnostics(tmpdir(), expected, never),
      /regular/,
    );
    if (process.platform !== "win32") {
      const link = executable + "-link";
      symlinkSync(executable, link);
      assert.throws(
        () => verifyReleaseDiagnostics(link, expected, never),
        /regular/,
      );
    }
  }));

test(
  "real subprocess returns JSON and nonzero diagnostics fails without starting an application",
  { skip: process.platform === "win32" },
  () =>
    fixture((executable) => {
      const script = (body) => {
        writeFileSync(executable, `#!${process.execPath}\n${body}\n`);
        chmodSync(executable, 0o755);
      };
      script(
        `if (process.argv.slice(2).join() !== '--osir-release-diagnostics' || process.env.OSIR_TEST_SECRET) process.exit(9); process.stdout.write(${JSON.stringify(JSON.stringify(report) + "\n")});`,
      );
      const previous = process.env.OSIR_TEST_SECRET;
      process.env.OSIR_TEST_SECRET = "must-not-be-inherited";
      try {
        assert.deepEqual(
          verifyReleaseDiagnostics(executable, expected).report,
          report,
        );
      } finally {
        if (previous === undefined) delete process.env.OSIR_TEST_SECRET;
        else process.env.OSIR_TEST_SECRET = previous;
      }
      script(
        "process.stderr.write('PRIVATE-MUST-NOT-BE-OUTPUT'); process.exit(64);",
      );
      assert.throws(
        () => verifyReleaseDiagnostics(executable, expected),
        /failed or timed out/,
      );
    }),
);
