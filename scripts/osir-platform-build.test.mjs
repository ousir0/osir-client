import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inspectPe } from "./osir-pe-verify.mjs";
import { windowsConfig } from "./osir-build-windows.mjs";
import { macosCiConfig } from "./osir-build-macos-ci.mjs";
import { validateSnapshotPath } from "./osir-source-snapshot.mjs";

function fixture({ machine = 0x8664, certificate = false, optionalSize = 240 } = {}) {
  const pe = Buffer.alloc(1024);
  pe.writeUInt16LE(0x5a4d, 0);
  pe.writeUInt32LE(128, 60);
  pe.writeUInt32LE(0x4550, 128);
  pe.writeUInt16LE(machine, 132);
  pe.writeUInt16LE(optionalSize, 148);
  pe.writeUInt16LE(machine === 0x14c ? 0x10b : 0x20b, 152);
  const directory = machine === 0x14c ? 96 : 112;
  if (certificate) {
    pe.writeUInt32LE(900, 152 + directory + 32);
    pe.writeUInt32LE(16, 152 + directory + 36);
  }
  return pe;
}

test("PE detection rejects malformed artifacts and wrong architecture", () => {
  const directory = mkdtempSync(join(tmpdir(), "osir-pe-test-"));
  const path = join(directory, "artifact.exe");
  try {
    writeFileSync(path, fixture());
    assert.equal(inspectPe(path, { expectedMachine: "x86_64" }).architecture, "x86_64");
    assert.equal(inspectPe(path).authenticodeCertificatePresent, false);
    assert.throws(() => inspectPe(path, { expectedMachine: "aarch64" }), /architecture/);
    writeFileSync(path, fixture({ machine: 0x14c }));
    assert.equal(inspectPe(path).architecture, "x86");
    writeFileSync(path, fixture({ certificate: true }));
    assert.equal(inspectPe(path).authenticodeCertificatePresent, true);
    const badCertificate = fixture({ certificate: true });
    badCertificate.writeUInt32LE(2000, 152 + 112 + 32);
    writeFileSync(path, badCertificate);
    assert.throws(() => inspectPe(path), /certificate/);
    writeFileSync(path, fixture({ optionalSize: 1 }));
    assert.throws(() => inspectPe(path), /optional/);
    writeFileSync(path, fixture().subarray(0, 160));
    assert.throws(() => inspectPe(path), /outside/);
    writeFileSync(path, Buffer.alloc(1024));
    assert.throws(() => inspectPe(path), /MZ/);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test("CI configurations contain real updater runtime but defer artifact signing", () => {
  const updater = JSON.parse(readFileSync(new URL("../config/osir-updater-prod.json", import.meta.url), "utf8"));
  for (const config of [windowsConfig("prod", updater), macosCiConfig("prod", updater)]) {
    assert.equal(config.identifier, "com.osir.client");
    assert.equal(config.bundle.createUpdaterArtifacts, false);
    assert.deepEqual(config.plugins.updater.endpoints, [updater.endpoint]);
    assert.equal(config.plugins.updater.pubkey, updater.pubkey);
    assert.deepEqual(config.plugins["osir-updates"], { schemaVersion: 1, channel: "prod", bundleArtifacts: true });
    assert.deepEqual(config.plugins["deep-link"].desktop.schemes, ["osir-client"]);
    assert.equal(config.build.devUrl, null);
    assert.equal(config.build.beforeBuildCommand, null);
    assert.ok(config.bundle.icon.every((path) => path.includes("icons/osir/")));
  }
  const windows = windowsConfig("prod", updater);
  assert.equal(windows.bundle.windows.nsis.installMode, "currentUser");
  assert.equal(windows.bundle.windows.certificateThumbprint, null);
  assert.equal(windows.bundle.windows.signCommand, null);
  assert.equal(windows.bundle.windows.wix, null);
  assert.deepEqual([windows.app.windows[0].width, windows.app.windows[0].height, windows.app.windows[0].minWidth, windows.app.windows[0].minHeight], [1100, 720, 900, 600]);
});

test("private source export rejects credential files and personal homes", () => {
  for (const path of [".env", "config/.env.prod", "keys/signing.pem", "apple.p12", ".git/config", "src/../.osir-dev-home/account.json", ".osir-dev-home/state", "node_modules/package/index.js"])
    assert.throws(() => validateSnapshotPath(path));
  for (const path of ["src/features/osir/account.ts", "src-tauri/vendor/codex-mac-engine/src/lib.rs", "config/osir-updater-prod.json", ".github/workflows/osir-windows-release.yml"])
    assert.equal(validateSnapshotPath(path), path);
});
