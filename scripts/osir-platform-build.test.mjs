import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  rmSync,
  writeFileSync,
  readFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inspectPe } from "./osir-pe-verify.mjs";
import { windowsConfig } from "./osir-build-windows.mjs";
import { macosCiConfig } from "./osir-build-macos-ci.mjs";
import { validateSnapshotPath } from "./osir-source-snapshot.mjs";
import { sourceSnapshot, verifyRustIncludeInputs } from "./osir-build.mjs";
import { nsisPayload, verifyNsisPayload } from "./osir-nsis-payload.mjs";

test("NSIS baseline permits only one exact Tauri marker patch", () => {
  const compiled = Buffer.from(
    "PE fixture __TAURI_BUNDLE_TYPE_VAR_UNK payload",
  );
  const final = nsisPayload(compiled).bytes;
  assert.equal(verifyNsisPayload(compiled, final).changedBytes, 3);
  const damaged = Buffer.from(final);
  damaged[0] ^= 1;
  assert.throws(() => verifyNsisPayload(compiled, damaged));
  assert.throws(() => nsisPayload(Buffer.concat([compiled, compiled])));
  assert.throws(() => nsisPayload(Buffer.alloc(100)));
});

function fixture({
  machine = 0x8664,
  certificate = false,
  optionalSize = 240,
} = {}) {
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
    assert.equal(
      inspectPe(path, { expectedMachine: "x86_64" }).architecture,
      "x86_64",
    );
    assert.equal(inspectPe(path).authenticodeCertificatePresent, false);
    assert.throws(
      () => inspectPe(path, { expectedMachine: "aarch64" }),
      /architecture/,
    );
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
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("CI configurations contain real updater runtime but defer artifact signing", () => {
  const updater = JSON.parse(
    readFileSync(
      new URL("../config/osir-updater-prod.json", import.meta.url),
      "utf8",
    ),
  );
  for (const config of [
    windowsConfig("prod", updater),
    macosCiConfig("prod", updater),
  ]) {
    assert.equal(config.identifier, "com.osir.client");
    assert.equal(config.bundle.createUpdaterArtifacts, false);
    assert.deepEqual(config.plugins.updater.endpoints, [updater.endpoint]);
    assert.equal(config.plugins.updater.pubkey, updater.pubkey);
    assert.deepEqual(config.plugins["osir-updates"], {
      schemaVersion: 1,
      channel: "prod",
      bundleArtifacts: true,
    });
    assert.deepEqual(config.plugins["deep-link"].desktop.schemes, [
      "osir-client",
    ]);
    assert.equal(config.build.devUrl, null);
    assert.equal(config.build.beforeBuildCommand, null);
    assert.ok(config.bundle.icon.every((path) => path.includes("icons/osir/")));
  }
  const windows = windowsConfig("prod", updater);
  assert.equal(windows.bundle.windows.nsis.installMode, "currentUser");
  assert.equal(windows.bundle.windows.certificateThumbprint, null);
  assert.equal(windows.bundle.windows.signCommand, null);
  assert.equal(windows.bundle.windows.wix, null);
  assert.deepEqual(windows.bundle.windows.webviewInstallMode, {
    type: "embedBootstrapper",
    silent: true,
  });
  assert.deepEqual(
    [
      windows.app.windows[0].width,
      windows.app.windows[0].height,
      windows.app.windows[0].minWidth,
      windows.app.windows[0].minHeight,
    ],
    [1100, 720, 900, 600],
  );
});

test("private source export rejects credential files and personal homes", () => {
  for (const path of [
    ".env",
    "config/.env.prod",
    "keys/signing.pem",
    "apple.p12",
    ".git/config",
    "src/../.osir-dev-home/account.json",
    ".osir-dev-home/state",
    "node_modules/package/index.js",
  ])
    assert.throws(() => validateSnapshotPath(path));
  for (const path of [
    "src/features/osir/account.ts",
    "src-tauri/vendor/codex-mac-engine/src/lib.rs",
    "config/osir-updater-prod.json",
    ".github/workflows/osir-windows-release.yml",
  ])
    assert.equal(validateSnapshotPath(path), path);
});

test("release source manifest includes all Rust compile-time fixtures", () => {
  const snapshot = sourceSnapshot();
  assert.ok(
    snapshot.provenance.inputs.directories.includes("tests/fixtures/sessions"),
  );
  for (const path of [
    "tests/config/claudeKeyFields.json",
    "tests/fixtures/copilot-endpoint-cases.json",
  ])
    assert.ok(snapshot.provenance.inputs.files.includes(path));
});

test("Rust include preflight rejects omitted files before a snapshot is exported", () => {
  const directory = mkdtempSync(join(tmpdir(), "osir-includes-"));
  try {
    mkdirSync(join(directory, "src"));
    writeFileSync(
      join(directory, "src/lib.rs"),
      `
      // include_str!("ignored-comment")
      /* outer /* nested */ include_bytes!("ignored-block") */
      const EXAMPLE: &str = "include_str!(\\\"ignored-string\\\")";
      const RAW: &str = r#"include_bytes!("ignored-raw")"#;
      const QUOTE: char = '"';
      const ESCAPED: char = '\\\\';
      fn lifetime<'a>(value: &'a str) -> &'a str { value }
      const VALUE: &str = include_str!(
        "../fixture.json",
      );
      const BYTES: &[u8] = include_bytes!(r#"../fixture.json"#);
    `,
    );
    writeFileSync(join(directory, "fixture.json"), "{}");
    assert.throws(
      () => verifyRustIncludeInputs(directory, ["src/lib.rs"]),
      /missing from release inputs/,
    );
    assert.equal(
      verifyRustIncludeInputs(directory, ["src/lib.rs", "fixture.json"])
        .references,
      2,
    );
    writeFileSync(
      join(directory, "src/lib.rs"),
      'include_str!(concat!("../", "fixture.json"))',
    );
    assert.throws(
      () => verifyRustIncludeInputs(directory, ["src/lib.rs", "fixture.json"]),
      /reviewed literal/,
    );
    writeFileSync(
      join(directory, "src/lib.rs"),
      'include_bytes!("../../outside.key")',
    );
    assert.throws(
      () => verifyRustIncludeInputs(directory, ["src/lib.rs"]),
      /missing from release inputs/,
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
