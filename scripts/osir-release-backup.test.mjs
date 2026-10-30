import test from "node:test";
import assert from "node:assert/strict";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { backupRelease, restoreRelease } from "./osir-release-backup.mjs";

const digest = (value) => createHash("sha256").update(value).digest("hex");
// Public minisign vector; the test never creates or reads a private key.
const pubkey = Buffer.from(
  "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n",
).toString("base64");
const signature = Buffer.from(
  "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==\n",
).toString("base64");
function writable(root) {
  chmodSync(root, 0o700);
  for (const e of readdirSync(root, { withFileTypes: true }))
    if (e.isDirectory()) writable(join(root, e.name));
}
function fixture(t, { channel = "beta", version = "1.2.3-beta.1" } = {}) {
  const root = mkdtempSync(join(tmpdir(), "osir-backup-test-"));
  t.after(() => {
    writable(root);
    rmSync(root, { recursive: true, force: true });
  });
  const source = join(root, "source");
  mkdirSync(source);
  const name = `OSIR-Client-${channel}-${version}-aarch64-apple-darwin`;
  const config = {
    schemaVersion: 1,
    channel,
    pubkey,
    endpoint: `https://osirclaw.com/api/v1/osir-client/updater/${channel}.json`,
    downloadBaseUrl: "https://osirclaw.com/client-release/",
    notes: "fixture",
  };
  const configPath = join(root, "public.json");
  writeFileSync(configPath, JSON.stringify(config));
  writeFileSync(join(source, name + ".app.tar.gz"), "test");
  writeFileSync(join(source, name + ".app.tar.gz.sig"), signature);
  writeFileSync(join(source, name + ".dmg"), "installer");
  writeFileSync(
    join(source, `updater-${channel}.json`),
    JSON.stringify({
      version,
      platforms: {
        "darwin-aarch64": {
          signature,
          url: `https://osirclaw.com/client-release/${name}.app.tar.gz`,
        },
      },
    }),
  );
  writeFileSync(
    join(source, "build-manifest.json"),
    JSON.stringify({
      channel,
      version,
      mode: "release-adhoc-distribution",
      architectures: ["arm64"],
      distributionSignatureVerified: false,
      codeSignatureVerified: true,
      notarization: "not-notarized",
      signing: "ad-hoc-not-notarized",
      updaterEnabled: true,
      updaterRuntimeConfigured: true,
      updaterUpgradeAcceptance: "not-run",
      updaterConfigSha256: digest(JSON.stringify(config)),
      updaterArtifact: {
        sha256: digest("test"),
        bytes: 4,
        signatureVerified: true,
      },
      installerArtifact: {
        name: name + ".dmg",
        sha256: digest("installer"),
        bytes: 9,
      },
      sourceProvenance: {
        verifiedAtEnd: true,
        sourceSha256: "a".repeat(64),
        git: { head: "b".repeat(40), dirtyAtStart: false },
      },
    }),
  );
  writeFileSync(
    join(source, "operator-secret.json"),
    "excluded fixture credential",
  );
  const requests = [];
  const fetchImpl = async (url, options) => {
    requests.push({ url, options });
    const response = new Response(
      readFileSync(join(source, new URL(url).pathname.split("/").at(-1))),
    );
    Object.defineProperty(response, "url", { value: url });
    return response;
  };
  return {
    root,
    source,
    name,
    requests,
    options: {
      channel,
      buildDirectory: source,
      updaterConfigPath: configPath,
      destination: join(root, "backup"),
      fetchImpl,
    },
  };
}
test("cloud bytes are backed up, independently signed, sealed and restored without credentials", async (t) => {
  const f = fixture(t);
  const result = await backupRelease(f.options);
  assert.equal(result.cloud_unique_artifacts, 2);
  assert.equal(lstatSync(result.path).mode & 0o222, 0);
  assert.equal(
    existsSync(join(result.path, "release/operator-secret.json")),
    false,
  );
  for (const { options } of f.requests) {
    assert.equal(options.redirect, "error");
    assert.equal(Object.hasOwn(options.headers, "authorization"), false);
  }
  const restored = restoreRelease({
    backupDirectory: result.path,
    destination: join(f.root, "restored"),
    expectedReceiptSha256: result.receipt_sha256,
  });
  assert.equal(restored.release_id, "beta-1.2.3-beta.1");
  assert.deepEqual(
    readFileSync(join(restored.path, "release", f.name + ".dmg")),
    Buffer.from("installer"),
  );
  assert.throws(
    () =>
      restoreRelease({
        backupDirectory: result.path,
        destination: restored.path,
        expectedReceiptSha256: result.receipt_sha256,
      }),
    /new absolute/,
  );
});
test("public download rejects redirect, corruption and truncation; never leaves a success receipt", async (t) => {
  for (const mode of ["redirect", "corrupt", "truncated", "oversize"]) {
    const f = fixture(t);
    f.options.fetchImpl = async (url) => {
      const r = new Response(
        mode === "truncated"
          ? "t"
          : mode === "oversize"
            ? "test overflow"
            : "fail",
        { status: mode === "redirect" ? 302 : 200 },
      );
      Object.defineProperty(r, "url", { value: url });
      return r;
    };
    await assert.rejects(backupRelease(f.options), /Backup/);
    assert.equal(
      existsSync(join(f.options.destination, "backup-receipt.json")),
      false,
    );
  }
});
test("restore rejects tampered bytes and traversal before creating destination", async (t) => {
  const f = fixture(t);
  const result = await backupRelease(f.options);
  const file = join(f.options.destination, "release", f.name + ".dmg");
  chmodSync(file, 0o600);
  writeFileSync(file, "changed");
  const destination = join(f.root, "restored");
  assert.throws(
    () =>
      restoreRelease({
        backupDirectory: f.options.destination,
        destination,
        expectedReceiptSha256: result.receipt_sha256,
      }),
    /bytes differ/,
  );
  assert.equal(existsSync(destination), false);
  const receiptPath = join(f.options.destination, "backup-receipt.json");
  const receipt = JSON.parse(readFileSync(receiptPath));
  receipt.files[0].path = "../outside";
  chmodSync(receiptPath, 0o600);
  writeFileSync(receiptPath, JSON.stringify(receipt));
  assert.throws(
    () =>
      restoreRelease({
        backupDirectory: f.options.destination,
        destination,
        expectedReceiptSha256: result.receipt_sha256,
      }),
    /independently retained/,
  );
  assert.throws(
    () =>
      restoreRelease({
        backupDirectory: f.options.destination,
        destination,
        expectedReceiptSha256: digest(readFileSync(receiptPath)),
      }),
    /path rejected/,
  );
});
test("backup refuses nested destination and symlink source", async (t) => {
  const f = fixture(t);
  await assert.rejects(
    backupRelease({ ...f.options, destination: join(f.source, "nested") }),
    /separate/,
  );
  const link = join(f.root, "linked");
  symlinkSync(f.source, link);
  await assert.rejects(
    backupRelease({ ...f.options, buildDirectory: link }),
    /without links/,
  );
});
test("Intel artifact signatures containing architecture underscores survive backup and restore", async (t) => {
  const f = fixture(t);
  for (const name of readdirSync(f.source)) {
    if (name.endsWith(".json") && name !== "operator-secret.json") {
      const value = readFileSync(join(f.source, name), "utf8")
        .replaceAll("aarch64-apple-darwin", "x86_64-apple-darwin")
        .replaceAll("darwin-aarch64", "darwin-x86_64")
        .replaceAll('"arm64"', '"x86_64"');
      writeFileSync(join(f.source, name), value);
    } else if (name.includes("aarch64-apple-darwin"))
      renameSync(
        join(f.source, name),
        join(
          f.source,
          name.replace("aarch64-apple-darwin", "x86_64-apple-darwin"),
        ),
      );
  }
  const result = await backupRelease(f.options);
  const restored = restoreRelease({
    backupDirectory: result.path,
    destination: join(f.root, "restored"),
    expectedReceiptSha256: result.receipt_sha256,
  });
  assert.equal(
    existsSync(
      join(
        restored.path,
        "release",
        f.name.replace("aarch64-apple-darwin", "x86_64-apple-darwin") +
          ".app.tar.gz.sig",
      ),
    ),
    true,
  );
});

test("legacy Windows 0.1.0 without installer or renderer evidence remains recoverable", async (t) => {
  const f = fixture(t, { channel: "prod", version: "0.1.0" });
  const name = "OSIR-Client-prod-0.1.0-x86_64-pc-windows-msvc-setup.exe";
  renameSync(join(f.source, f.name + ".app.tar.gz"), join(f.source, name));
  renameSync(
    join(f.source, f.name + ".app.tar.gz.sig"),
    join(f.source, name + ".sig"),
  );
  rmSync(join(f.source, f.name + ".dmg"));
  const manifestPath = join(f.source, "build-manifest.json");
  const manifest = JSON.parse(readFileSync(manifestPath));
  Object.assign(manifest, {
    mode: "release-windows-distribution",
    target: "x86_64-pc-windows-msvc",
    architectures: ["x86_64"],
    codeSignatureVerified: false,
    notarization: "not-applicable",
    signing: "windows-unsigned-updater-signed",
    peArchitectureVerified: true,
    installerArtifact: { name, bytes: 4, sha256: digest("test") },
  });
  writeFileSync(manifestPath, JSON.stringify(manifest));
  writeFileSync(
    join(f.source, "updater-prod.json"),
    JSON.stringify({
      version: "0.1.0",
      platforms: {
        "windows-x86_64": {
          signature,
          url: "https://osirclaw.com/client-release/" + name,
        },
      },
    }),
  );
  const result = await backupRelease(f.options);
  const restored = restoreRelease({
    backupDirectory: result.path,
    destination: join(f.root, "restored"),
    expectedReceiptSha256: result.receipt_sha256,
  });
  assert.equal(restored.release_id, "prod-0.1.0");
  assert.equal(
    digest(readFileSync(join(restored.path, "release", name))),
    digest("test"),
  );
  const recovered = JSON.parse(
    readFileSync(join(restored.path, "release/build-manifest.json")),
  );
  assert.equal(Object.hasOwn(recovered, "packagedInstallerScreenshot"), false);
  assert.equal(Object.hasOwn(recovered, "packagedRendererReport"), false);
});

test("incomplete or changed installer and renderer evidence never produces a successful backup", async (t) => {
  for (const mode of [
    "missing-report",
    "missing-screenshots",
    "one-screenshot",
    "duplicate-screenshot",
    "wrong-extension",
    "missing-file",
    "changed-report",
    "missing-installer-screenshot",
    "changed-installer-screenshot",
  ]) {
    const f = fixture(t);
    const manifestPath = join(f.source, "build-manifest.json");
    const manifest = JSON.parse(readFileSync(manifestPath));
    const binding = (name, value) => {
      writeFileSync(join(f.source, name), value);
      return { name, bytes: Buffer.byteLength(value), sha256: digest(value) };
    };
    manifest.packagedRendererReport = binding(
      "renderer.json",
      "fixture report",
    );
    manifest.packagedRendererScreenshots = [
      binding("renderer-large.png", "fixture large screenshot"),
      binding("renderer-small.png", "fixture small screenshot"),
      binding("renderer-launch-large.png", "fixture launch large screenshot"),
      binding("renderer-launch-small.png", "fixture launch small screenshot"),
    ];
    manifest.packagedInstallerScreenshot = binding(
      "windows-installer-window.png",
      "fixture installer screenshot",
    );
    if (mode === "missing-report") delete manifest.packagedRendererReport;
    if (mode === "missing-screenshots")
      delete manifest.packagedRendererScreenshots;
    if (mode === "one-screenshot") manifest.packagedRendererScreenshots.pop();
    if (mode === "duplicate-screenshot")
      manifest.packagedRendererScreenshots[1] =
        manifest.packagedRendererScreenshots[0];
    if (mode === "wrong-extension")
      manifest.packagedRendererScreenshots[1].name = "renderer-small.jpg";
    if (mode === "missing-file") rmSync(join(f.source, "renderer-small.png"));
    if (mode === "changed-report")
      writeFileSync(join(f.source, "renderer.json"), "changed report");
    if (mode === "missing-installer-screenshot")
      rmSync(join(f.source, "windows-installer-window.png"));
    if (mode === "changed-installer-screenshot")
      writeFileSync(
        join(f.source, "windows-installer-window.png"),
        "changed installer screenshot",
      );
    writeFileSync(manifestPath, JSON.stringify(manifest));
    await assert.rejects(
      backupRelease(f.options),
      mode === "missing-file" || mode === "missing-installer-screenshot"
        ? /ENOENT/
        : mode === "changed-report" || mode === "changed-installer-screenshot"
          ? /Acceptance evidence differs/
          : mode === "duplicate-screenshot"
            ? /Acceptance filename rejected/
            : /Renderer acceptance/,
    );
    assert.equal(
      existsSync(join(f.options.destination, "backup-receipt.json")),
      false,
      mode,
    );
  }
});

test("Windows shared payload, installer and renderer evidence survive combined backup and restore", async (t) => {
  const f = fixture(t);
  const name = "OSIR-Client-beta-1.2.3-beta.1-x86_64-pc-windows-msvc-setup.exe";
  renameSync(join(f.source, f.name + ".app.tar.gz"), join(f.source, name));
  renameSync(
    join(f.source, f.name + ".app.tar.gz.sig"),
    join(f.source, name + ".sig"),
  );
  rmSync(join(f.source, f.name + ".dmg"));
  const manifest = JSON.parse(
    readFileSync(join(f.source, "build-manifest.json")),
  );
  Object.assign(manifest, {
    mode: "release-windows-distribution",
    target: "x86_64-pc-windows-msvc",
    architectures: ["x86_64"],
    codeSignatureVerified: false,
    notarization: "not-applicable",
    signing: "windows-unsigned-updater-signed",
    peArchitectureVerified: true,
    installerArtifact: { name, bytes: 4, sha256: digest("test") },
  });
  const binding = (name, value) => {
    writeFileSync(join(f.source, name), value);
    return { name, bytes: Buffer.byteLength(value), sha256: digest(value) };
  };
  manifest.mainExecutableArtifact = binding(
    "osir_client.exe",
    "fixture executable",
  );
  manifest.packagedLifecycleReport = binding(
    "windows-packaged-smoke.json",
    '{"passed":true}',
  );
  manifest.packagedInstallerScreenshot = binding(
    "windows-installer-window.png",
    "fixture installer wizard screenshot",
  );
  manifest.packagedWindowScreenshot = binding(
    "windows-app-window.png",
    "fixture original screenshot",
  );
  manifest.packagedRendererReport = binding(
    "windows-renderer-smoke.json",
    '{"passed":true,"nativeIPC":true}',
  );
  manifest.packagedRendererScreenshots = [
    binding("windows-renderer-1100x720.png", "fixture large screenshot"),
    binding("windows-renderer-900x600.png", "fixture small screenshot"),
    binding(
      "windows-renderer-launch-1100x720.png",
      "fixture launch large screenshot",
    ),
    binding(
      "windows-renderer-launch-900x600.png",
      "fixture launch small screenshot",
    ),
  ];
  writeFileSync(
    join(f.source, "build-manifest.json"),
    JSON.stringify(manifest),
  );
  writeFileSync(
    join(f.source, "updater-beta.json"),
    JSON.stringify({
      version: manifest.version,
      platforms: {
        "windows-x86_64": {
          signature,
          url: "https://osirclaw.com/client-release/" + name,
        },
      },
    }),
  );
  mkdirSync(join(f.source, "windows-x64"));
  for (const file of readdirSync(f.source, { withFileTypes: true }))
    if (file.isFile() && file.name !== "operator-secret.json")
      copyFileSync(
        join(f.source, file.name),
        join(f.source, "windows-x64", file.name),
      );
  writeFileSync(
    join(f.source, "build-manifest.json"),
    JSON.stringify({
      mode: "release-multiplatform-distribution",
      builds: ["windows-x64"],
    }),
  );
  const result = await backupRelease(f.options);
  assert.equal(result.cloud_unique_artifacts, 1);
  const restored = restoreRelease({
    backupDirectory: result.path,
    destination: join(f.root, "restored"),
    expectedReceiptSha256: result.receipt_sha256,
  });
  for (const item of [
    manifest.mainExecutableArtifact,
    manifest.packagedLifecycleReport,
    manifest.packagedInstallerScreenshot,
    manifest.packagedWindowScreenshot,
    manifest.packagedRendererReport,
    ...manifest.packagedRendererScreenshots,
  ]) {
    assert.equal(
      digest(
        readFileSync(join(restored.path, "release/windows-x64", item.name)),
      ),
      item.sha256,
    );
  }
  for (const item of [
    manifest.packagedInstallerScreenshot,
    manifest.packagedRendererScreenshots[0],
  ]) {
    const screenshot = join(
      f.options.destination,
      "release/windows-x64",
      item.name,
    );
    const original = readFileSync(screenshot);
    chmodSync(screenshot, 0o600);
    writeFileSync(screenshot, "changed screenshot");
    const destination = join(f.root, "tampered-" + item.name);
    assert.throws(
      () =>
        restoreRelease({
          backupDirectory: result.path,
          destination,
          expectedReceiptSha256: result.receipt_sha256,
        }),
      /bytes differ/,
    );
    assert.equal(existsSync(destination), false);
    writeFileSync(screenshot, original);
    chmodSync(screenshot, 0o400);
  }
});
