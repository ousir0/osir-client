import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { parseMacosCiArguments } from "./osir-build-macos-ci.mjs";
import {
  ciTarget,
  assertNativeCiHost,
  assertCiProvenance,
  assertMacCiManifest,
  assertMacCiSmokeReport,
} from "./osir-ci-artifact.mjs";
import { verifyRustIncludeInputs } from "./osir-build.mjs";
import { validateSnapshotPath } from "./osir-source-snapshot.mjs";
import { macNativeAcceptance } from "./osir-release-finalize.mjs";

const arm = "aarch64-apple-darwin";
const intel = "x86_64-apple-darwin";
const source = {
  sourceSha256: "a".repeat(64),
  inputsManifestSha256: "b".repeat(64),
  git: { head: "c".repeat(40), dirty: false },
};

test("finalizer preserves CI evidence identity and the before-signing boundary for each Mac architecture", () => {
  const report = Object.freeze({
    name: "macos-packaged-smoke.json",
    sha256: "d".repeat(64),
    bytes: 2048,
    scope: "native-window-launch-reopen-no-renderer-ipc",
  });
  for (const [target, scope] of [
    [arm, "native-arm64-ci-before-local-signing"],
    [intel, "native-intel-ci-before-local-signing"],
  ]) {
    assert.deepEqual(macNativeAcceptance(target, report), {
      path: report.name,
      sha256: report.sha256,
      bytes: report.bytes,
      scope,
    });
  }
  for (const target of [
    "universal-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "",
    undefined,
  ])
    assert.throws(
      () => macNativeAcceptance(target, report),
      /Unsupported native Mac acceptance target/,
    );
});

function manifestFor(target) {
  return {
    channel: "prod",
    version: "1.2.3",
    target,
    identifier: "com.osir.client",
    mode: "ci-pending-local-finalization",
    signing: "ad-hoc-pending-local",
    codeSignatureVerified: true,
    distributionSignatureVerified: false,
    updaterEnabled: true,
    updaterRuntimeConfigured: true,
    embeddedUpdaterPublicConfigVerified: true,
    updaterSignature: "pending-local",
    architectures: [target === arm ? "arm64" : "x86_64"],
    sourceProvenance: {
      ...structuredClone(source),
      verifiedAtEnd: true,
      git: { ...source.git, dirtyAtStart: false },
    },
    packagedMacAcceptance: {
      scope:
        target === arm
          ? "native-arm64-window-launch-reopen-no-renderer-ipc"
          : "native-intel-window-launch-reopen-no-renderer-ipc",
    },
  };
}

test("macOS CI target selection keeps Intel compatibility and rejects local-only/cross-platform arguments", () => {
  assert.deepEqual(parseMacosCiArguments(["prod", "--check"]), {
    channel: "prod",
    target: intel,
    check: true,
  });
  assert.deepEqual(
    parseMacosCiArguments(["beta", "--target", arm, "--check"]),
    {
      channel: "beta",
      target: arm,
      check: true,
    },
  );
  for (const args of [
    ["dev"],
    ["prod", "--local-unsigned"],
    ["prod", "--target"],
    ["prod", "--target", "universal-apple-darwin"],
    ["prod", "--target", "x86_64-pc-windows-msvc"],
    ["prod", "--target", arm, "--target", intel],
    ["prod", "--check", "--check"],
  ])
    assert.throws(() => parseMacosCiArguments(args));
});

test("native CI rejects a mismatched runner before accepting any target artifacts", () => {
  for (const [target, platform, arch] of [
    [arm, "darwin", "arm64"],
    [intel, "darwin", "x64"],
    ["x86_64-pc-windows-msvc", "win32", "x64"],
  ]) {
    assertNativeCiHost(target, { platform, arch });
    assert.throws(() =>
      assertNativeCiHost(target, {
        platform,
        arch: arch === "x64" ? "arm64" : "x64",
      }),
    );
    assert.throws(() =>
      assertNativeCiHost(target, { platform: "linux", arch }),
    );
  }
  assert.throws(() => ciTarget("universal-apple-darwin"));
});

test("CI provenance cannot reuse a different commit, source, input manifest or unfinished build", () => {
  const manifest = manifestFor(arm);
  assertCiProvenance(manifest, source);
  for (const change of [
    { sourceSha256: "d".repeat(64) },
    { inputsManifestSha256: "d".repeat(64) },
    { verifiedAtEnd: false },
    { git: { ...manifest.sourceProvenance.git, head: "d".repeat(40) } },
    { git: { ...manifest.sourceProvenance.git, dirty: true } },
    { git: { ...manifest.sourceProvenance.git, dirtyAtStart: true } },
  ])
    assert.throws(() =>
      assertCiProvenance(
        {
          ...manifest,
          sourceProvenance: { ...manifest.sourceProvenance, ...change },
        },
        source,
      ),
    );
});

test("ARM and Intel release candidates require the real updater runtime and exact architecture", () => {
  for (const target of [arm, intel]) {
    const manifest = manifestFor(target);
    assertMacCiManifest(manifest, "prod", target);
    for (const change of [
      { mode: "local-unsigned" },
      { signing: "ad-hoc-local-only" },
      { target: target === arm ? intel : arm },
      { channel: "beta" },
      { architectures: ["arm64", "x86_64"] },
      { architectures: [target === arm ? "x86_64" : "arm64"] },
      { updaterEnabled: false },
      { updaterRuntimeConfigured: false },
      { embeddedUpdaterPublicConfigVerified: false },
      { codeSignatureVerified: false },
      { distributionSignatureVerified: true },
      { updaterSignature: "claimed-signed" },
    ])
      assert.throws(() =>
        assertMacCiManifest({ ...manifest, ...change }, "prod", target),
      );
  }
});

test("Mac native evidence binds launch/reopen, native host, isolation and exact executable without claiming IPC", () => {
  for (const target of [arm, intel]) {
    const manifest = manifestFor(target);
    const binding = {
      executableSha256: "e".repeat(64),
      appPath: "/fixture/OSIR Client.app",
    };
    const report = {
      passed: true,
      target,
      version: manifest.version,
      sourceSha256: source.sourceSha256,
      gitHead: source.git.head,
      executableSha256: binding.executableSha256,
      hostArchitecture: target === arm ? "arm64" : "x64",
      isolatedHome: true,
      productionDataBlocked: true,
      productionSocketBlocked: true,
      rendererAndNativeIpcCovered: false,
      stages: ["launch", "reopen"].map((phase) => ({
        phase,
        passed: true,
        windowObserved: true,
        remainedRunning: true,
        isolatedDatabaseCreated: true,
        stoppedOnlyVerifiedPid: true,
        observation: {
          bundleIdentifier: manifest.identifier,
          bundlePath: binding.appPath,
          otherInstancePids: [],
        },
      })),
    };
    assertMacCiSmokeReport(report, manifest, binding);
    for (const change of [
      { passed: false },
      { target: target === arm ? intel : arm },
      { version: "1.2.2" },
      { sourceSha256: "f".repeat(64) },
      { gitHead: "f".repeat(40) },
      { executableSha256: "f".repeat(64) },
      { hostArchitecture: target === arm ? "x64" : "arm64" },
      { productionDataBlocked: false },
      { productionSocketBlocked: false },
      { isolatedHome: false },
      { rendererAndNativeIpcCovered: true },
      { stages: report.stages.slice(0, 1) },
    ])
      assert.throws(() =>
        assertMacCiSmokeReport({ ...report, ...change }, manifest, binding),
      );
    for (const phase of [0, 1]) {
      for (const key of [
        "passed",
        "windowObserved",
        "remainedRunning",
        "isolatedDatabaseCreated",
        "stoppedOnlyVerifiedPid",
      ]) {
        const changed = structuredClone(report);
        changed.stages[phase][key] = false;
        assert.throws(() => assertMacCiSmokeReport(changed, manifest, binding));
      }
      for (const observation of [
        { bundleIdentifier: "com.other.app" },
        { bundlePath: "/Applications/OSIR Client.app" },
        { otherInstancePids: [123] },
      ]) {
        const changed = structuredClone(report);
        Object.assign(changed.stages[phase].observation, observation);
        assert.throws(() => assertMacCiSmokeReport(changed, manifest, binding));
      }
    }
    assert.throws(() =>
      assertMacCiSmokeReport(
        report,
        { ...manifest, packagedMacAcceptance: { scope: "local-only" } },
        binding,
      ),
    );
  }
});

test("frozen Rust inputs require schema SQL, public key and product metadata individually", (t) => {
  const root = mkdtempSync(join(tmpdir(), "osir-ci-inputs-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const rust = {
    "src-tauri/src/database/compatibility.rs":
      'const SCHEMA: &str = include_str!("upstream_v26_schema.sql");',
    "src-tauri/src/commands/osir_anywhere_install_manifest.rs":
      'const KEY: &str = include_str!("../../resources/products/osir-anywhere/release-public.pub");',
    "src-tauri/src/commands/osir_tools_macos.rs":
      'const PRODUCT: &str = include_str!("../../resources/products/osir-anywhere/manifest.dev.json");',
  };
  const resources = {
    "src-tauri/src/database/upstream_v26_schema.sql":
      "CREATE TABLE fixture(id INTEGER);",
    "src-tauri/resources/products/osir-anywhere/release-public.pub":
      "synthetic-public-key",
    "src-tauri/resources/products/osir-anywhere/manifest.dev.json":
      '{"install_enabled":false,"download_url":null}',
  };
  for (const [relative, contents] of Object.entries({
    ...rust,
    ...resources,
  })) {
    validateSnapshotPath(relative);
    mkdirSync(dirname(join(root, relative)), { recursive: true });
    writeFileSync(join(root, relative), contents);
  }
  const files = Object.keys({ ...rust, ...resources });
  assert.equal(verifyRustIncludeInputs(root, files).references, 3);
  for (const path of Object.keys(resources))
    assert.throws(
      () =>
        verifyRustIncludeInputs(
          root,
          files.filter((file) => file !== path),
        ),
      /missing from release inputs/,
    );
  assert.throws(() =>
    validateSnapshotPath(
      "src-tauri/resources/products/osir-anywhere/release-private.key",
    ),
  );
});
