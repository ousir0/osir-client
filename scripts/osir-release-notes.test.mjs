import test from "node:test";
import assert from "node:assert/strict";
import {
  chmodSync,
  existsSync,
  linkSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { combineRelease } from "./osir-release-combine.mjs";
import { backupRelease, restoreRelease } from "./osir-release-backup.mjs";
import {
  createReleasePlan,
  createServiceReleaseMetadata,
  verifyPublicReleaseArtifacts,
} from "./osir-release-publisher.mjs";
import {
  createReviewedReleasePlan,
  notesDigest as hash,
  readReviewedNotes,
  releasePlanFingerprint,
} from "./osir-release-notes.mjs";

// Public minisign test vector, never a private key or live product package.
const pubkey = Buffer.from(
  "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n",
).toString("base64");
const signature = Buffer.from(
  "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==\n",
).toString("base64");
function writable(root) {
  chmodSync(root, 0o700);
  for (const entry of readdirSync(root, { withFileTypes: true }))
    if (entry.isDirectory()) writable(join(root, entry.name));
}
function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "osir-notes-test-"));
  t.after(() => {
    writable(root);
    rmSync(root, { recursive: true, force: true });
  });
  const configPath = join(root, "public.json"),
    channel = "beta",
    version = "1.2.3-beta.1";
  const json = (path, value) =>
    writeFileSync(path, JSON.stringify(value, null, 2) + "\n");
  const config = {
    schemaVersion: 1,
    channel,
    pubkey,
    endpoint: "https://osirclaw.com/api/v1/osir-client/updater/beta.json",
    downloadBaseUrl: "https://osirclaw.com/client-release/",
    notes: "Old release copy",
  };
  json(configPath, config);
  const targets = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
  ];
  const inputs = targets.map((target, index) => {
    const directory = join(root, "input-" + index);
    mkdirSync(directory);
    const windows = index === 2,
      name = `OSIR-Client-${channel}-${version}-${target}`;
    const updater = name + (windows ? "-setup.exe" : ".app.tar.gz"),
      installer = windows ? updater : name + ".dmg";
    writeFileSync(join(directory, updater), "test");
    writeFileSync(join(directory, updater + ".sig"), signature);
    if (!windows)
      writeFileSync(join(directory, installer), "synthetic installer");
    const platform = ["darwin-aarch64", "darwin-x86_64", "windows-x86_64"][
      index
    ];
    json(join(directory, "updater-beta.json"), {
      version,
      notes: config.notes,
      platforms: {
        [platform]: {
          signature,
          url: "https://osirclaw.com/client-release/" + updater,
        },
      },
    });
    const report = {
      passed: true,
      version,
      scope: "synthetic-test-only",
      sourceSha256: "a".repeat(64),
      gitHead: "b".repeat(40),
      target,
    };
    json(join(directory, "native.json"), report);
    const raw = readFileSync(join(directory, "native.json")),
      record = { name: "native.json", sha256: hash(raw), bytes: raw.length };
    json(join(directory, "build-manifest.json"), {
      channel,
      version,
      target,
      mode: windows
        ? "release-windows-distribution"
        : "release-adhoc-distribution",
      architectures: [index === 0 ? "arm64" : "x86_64"],
      distributionSignatureVerified: false,
      codeSignatureVerified: !windows,
      signing: windows
        ? "windows-unsigned-updater-signed"
        : "ad-hoc-not-notarized",
      notarization: windows ? "not-applicable" : "not-notarized",
      peArchitectureVerified: windows,
      updaterEnabled: true,
      updaterRuntimeConfigured: true,
      updaterUpgradeAcceptance: "not-run",
      updaterConfigSha256: hash(readFileSync(configPath)),
      updaterArtifact: {
        sha256: hash("test"),
        bytes: 4,
        signatureVerified: true,
      },
      installerArtifact: {
        name: installer,
        sha256: hash(windows ? "test" : "synthetic installer"),
        bytes: windows ? 4 : 19,
      },
      sourceProvenance: {
        verifiedAtEnd: true,
        sourceSha256: "a".repeat(64),
        git: { head: "b".repeat(40), dirtyAtStart: false },
      },
      ...(windows
        ? { packagedLifecycleReport: record }
        : {
            packagedMacAcceptance: record,
            nativeAcceptance: {
              path: "native.json",
              sha256: record.sha256,
              bytes: record.bytes,
            },
          }),
    });
    return directory;
  });
  const buildDirectory = join(root, "combined");
  combineRelease({
    channel,
    inputs,
    output: buildDirectory,
    updaterConfigPath: configPath,
  });
  const options = { channel, buildDirectory, updaterConfigPath: configPath },
    original = createReleasePlan(options);
  const review = {
    schemaVersion: 1,
    releaseId: original.release_id,
    version,
    channel,
    sourceSha256: original.source_sha,
    gitHead: original.git_head,
    originalPlanSha256: releasePlanFingerprint(original),
    originalNotesSha256: hash(original.notes),
    notes: "修复部分客户端无法连接的问题。",
  };
  const path = join(root, "reviewed-notes.json");
  json(path, review);
  const binding = () => ({ path, sha256: hash(readFileSync(path)) });
  const fetchImpl = async (url) => {
    const response = new Response(
      readFileSync(
        join(
          buildDirectory,
          decodeURIComponent(new URL(url).pathname.split("/").at(-1)),
        ),
      ),
    );
    Object.defineProperty(response, "url", { value: url });
    return response;
  };
  return {
    root,
    options,
    original,
    review,
    path,
    binding,
    json,
    fetchImpl,
    config,
  };
}

test("reviewed copy changes only final notes/audit, retaining disclosures, original signatures and frozen inputs", (t) => {
  const f = fixture(t),
    before = hash(readFileSync(f.options.updaterConfigPath));
  const plan = createReviewedReleasePlan({
    ...f.options,
    notesOverride: f.binding(),
  });
  const { notes, notes_review, ...rest } = plan,
    { notes: oldNotes, ...old } = f.original;
  assert.deepEqual(rest, old);
  assert.match(notes, /未经 Apple 公证/);
  assert.match(notes, /Authenticode/);
  assert.match(notes, /修复部分客户端/);
  assert.doesNotMatch(notes, /Old release/);
  assert.equal(notes_review.override_sha256, f.binding().sha256);
  assert.equal(notes_review.final_notes_sha256, hash(notes));
  assert.equal(hash(readFileSync(f.options.updaterConfigPath)), before);
  assert.deepEqual(createReviewedReleasePlan(f.options), f.original);
  const relocated = {
    ...f.original,
    artifacts: f.original.artifacts.map((a) => ({
      ...a,
      path: "/relocated/" + a.file_name,
    })),
  };
  assert.equal(
    releasePlanFingerprint(relocated),
    releasePlanFingerprint(f.original),
  );
  for (const patch of [
    { notes: "different" },
    { notarization: "fake" },
    {
      artifacts: f.original.artifacts.map((a) => ({
        ...a,
        signature: "changed",
      })),
    },
  ])
    assert.notEqual(
      releasePlanFingerprint({ ...relocated, ...patch }),
      releasePlanFingerprint(f.original),
    );
});

test("raw override and all release/source/plan/notes bindings reject stale or expanded reviews", (t) => {
  const f = fixture(t),
    binding = f.binding();
  writeFileSync(f.path, readFileSync(f.path) + " ");
  assert.throws(
    () => createReviewedReleasePlan({ ...f.options, notesOverride: binding }),
    /raw hash/,
  );
  for (const [field, value] of Object.entries({
    schemaVersion: 2,
    releaseId: "beta-other",
    version: "1.2.4-beta.1",
    channel: "prod",
    sourceSha256: "c".repeat(64),
    gitHead: "c".repeat(40),
    originalPlanSha256: "c".repeat(64),
    originalNotesSha256: "c".repeat(64),
    notes: "\u0000bad",
    unknown: true,
  })) {
    f.json(f.path, { ...f.review, [field]: value });
    assert.throws(() =>
      createReviewedReleasePlan({ ...f.options, notesOverride: f.binding() }),
    );
  }
  f.json(f.path, f.review);
  assert.throws(
    () => readReviewedNotes({ ...f.binding(), path: "relative" }),
    /absolute/,
  );
  const link = join(f.root, "link.json");
  symlinkSync(f.path, link);
  assert.throws(
    () => readReviewedNotes({ ...f.binding(), path: link }),
    /ordinary/,
  );
  const hard = join(f.root, "hard.json");
  linkSync(f.path, hard);
  assert.throws(() => readReviewedNotes(f.binding()), /ordinary/);
});

test("override cannot bypass frozen config, original artifact signature or native evidence checks", (t) => {
  const f = fixture(t);
  const native = join(f.options.buildDirectory, "mac-arm64/native.json");
  writeFileSync(native, "{}");
  assert.throws(
    () =>
      createReviewedReleasePlan({ ...f.options, notesOverride: f.binding() }),
    /Native evidence hash/,
  );
  f.json(f.options.updaterConfigPath, {
    ...f.config,
    notes: "edited frozen notes",
  });
  assert.throws(
    () =>
      createReviewedReleasePlan({ ...f.options, notesOverride: f.binding() }),
    /Public updater configuration/,
  );
  const g = fixture(t),
    artifact = g.original.artifacts.find(
      (a) => a.platform === "darwin-aarch64" && a.kind === "updater",
    );
  writeFileSync(
    join(g.options.buildDirectory, "mac-arm64", artifact.file_name + ".sig"),
    "tampered signature",
  );
  assert.throws(
    () =>
      createReviewedReleasePlan({ ...g.options, notesOverride: g.binding() }),
    /signature differs/,
  );
  const h = fixture(t),
    directory = join(h.options.buildDirectory, "mac-arm64");
  const manifestPath = join(directory, "build-manifest.json"),
    manifest = JSON.parse(readFileSync(manifestPath));
  const nativePath = join(directory, "native.json"),
    report = JSON.parse(readFileSync(nativePath));
  h.json(nativePath, { ...report, passed: false });
  const record = {
    sha256: hash(readFileSync(nativePath)),
    bytes: readFileSync(nativePath).length,
  };
  h.json(manifestPath, {
    ...manifest,
    packagedMacAcceptance: { ...manifest.packagedMacAcceptance, ...record },
    nativeAcceptance: { ...manifest.nativeAcceptance, ...record },
  });
  assert.throws(
    () =>
      createReviewedReleasePlan({ ...h.options, notesOverride: h.binding() }),
    /Native acceptance result/,
  );
});

test("metadata and public readback use reviewed copy; manually patched metadata is still rejected", async (t) => {
  const f = fixture(t),
    plan = createReviewedReleasePlan({
      ...f.options,
      notesOverride: f.binding(),
    });
  const metadataPath = join(f.root, "metadata.json");
  f.json(metadataPath, createServiceReleaseMetadata(plan));
  assert.notEqual(
    createServiceReleaseMetadata(plan).idempotency_key,
    createServiceReleaseMetadata(f.original).idempotency_key,
  );
  const result = await verifyPublicReleaseArtifacts(plan, {
    metadataPath,
    fetchImpl: f.fetchImpl,
  });
  assert.equal(result.readback_verified, true);
  f.json(metadataPath, createServiceReleaseMetadata(f.original));
  await assert.rejects(
    verifyPublicReleaseArtifacts(plan, {
      metadataPath,
      fetchImpl: f.fetchImpl,
    }),
    /metadata differs/,
  );
});

test("independent backup/restore retains exact reviewed sidecar and consistent final notes without original file", async (t) => {
  const f = fixture(t),
    binding = f.binding(),
    plan = createReviewedReleasePlan({ ...f.options, notesOverride: binding });
  const backup = await backupRelease({
    ...f.options,
    notesOverride: binding,
    destination: join(f.root, "backup"),
    fetchImpl: f.fetchImpl,
  });
  assert.deepEqual(backup.missing_evidence, []);
  const receipt = JSON.parse(
    readFileSync(join(backup.path, "backup-receipt.json")),
  );
  assert.equal(receipt.plan.notes, plan.notes);
  assert.deepEqual(receipt.plan.notes_review, plan.notes_review);
  assert.equal(
    hash(readFileSync(join(backup.path, "reviewed-notes.json"))),
    binding.sha256,
  );
  rmSync(f.path);
  const restored = restoreRelease({
    backupDirectory: backup.path,
    destination: join(f.root, "restored"),
    expectedReceiptSha256: backup.receipt_sha256,
  });
  const recovered = createReviewedReleasePlan({
    channel: f.options.channel,
    buildDirectory: join(restored.path, "release"),
    updaterConfigPath: join(restored.path, "public-updater.json"),
    notesOverride: {
      path: join(restored.path, "reviewed-notes.json"),
      sha256: binding.sha256,
    },
  });
  assert.equal(releasePlanFingerprint(recovered), releasePlanFingerprint(plan));
  chmodSync(join(backup.path, "reviewed-notes.json"), 0o600);
  writeFileSync(join(backup.path, "reviewed-notes.json"), "{}");
  await assert.rejects(
    async () =>
      restoreRelease({
        backupDirectory: backup.path,
        destination: join(f.root, "bad-restore"),
        expectedReceiptSha256: backup.receipt_sha256,
      }),
    /Backup bytes/,
  );
  assert.equal(existsSync(join(f.root, "bad-restore")), false);
});
