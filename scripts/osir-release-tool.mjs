#!/usr/bin/env node
import assert from "node:assert/strict";
import {
  constants,
  closeSync,
  fstatSync,
  lstatSync,
  openSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { resolve, join, isAbsolute, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { runCloudRelease } from "./osir-release-cloud.mjs";
import { createReleasePlan } from "./osir-release-publisher.mjs";
import { createServiceReleaseMetadata } from "./osir-release-publisher.mjs";
import { backupRelease, restoreRelease } from "./osir-release-backup.mjs";

const ROOT = fileURLToPath(new URL("../", import.meta.url));
export function checkReleaseSpec(spec, plan) {
  assert.ok(["prod", "beta"].includes(spec.channel));
  assert.equal(plan.channel, spec.channel);
  assert.equal(plan.version, spec.version);
  assert.match(spec.sourceSha256 ?? "", /^[a-f0-9]{64}$/);
  assert.equal(plan.source_sha, spec.sourceSha256);
  assert.match(spec.gitHead ?? "", /^[a-f0-9]{40,64}$/);
  assert.equal(plan.git_head, spec.gitHead);
  assert.ok(
    spec.expectedCurrent === null ||
      (typeof spec.expectedCurrent === "string" &&
        spec.expectedCurrent.startsWith(spec.channel + "-")),
  );
  for (const key of [
    "buildDirectory",
    "operatorConfig",
    "backupDirectory",
    "restoreDirectory",
  ])
    assert.ok(
      typeof spec[key] === "string" && isAbsolute(spec[key]),
      `${key} must be absolute`,
    );
  assert.deepEqual(
    plan.artifacts.map((a) => `${a.platform}/${a.kind}`).sort(),
    ["darwin-aarch64", "darwin-x86_64", "windows-x86_64"]
      .flatMap((p) => [`${p}/installer`, `${p}/updater`])
      .sort(),
  );
}

function readPrivateReceipt(path) {
  assert.ok(isAbsolute(path), "Resume receipt path must be absolute");
  path = resolve(path);
  const parent = lstatSync(dirname(path));
  assert.ok(
    parent.isDirectory() &&
      !parent.isSymbolicLink() &&
      (parent.mode & 0o077) === 0,
    "Resume receipt directory must be private and ordinary.",
  );
  const stat = lstatSync(path);
  assert.ok(
    stat.isFile() &&
      !stat.isSymbolicLink() &&
      stat.nlink === 1 &&
      stat.size > 0 &&
      stat.size <= 1024 * 1024,
    "Resume receipt must be one bounded regular file.",
  );
  const fd = openSync(
    path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  try {
    const before = fstatSync(fd);
    assert.ok(
      before.isFile() &&
        before.nlink === 1 &&
        before.ino === stat.ino &&
        before.dev === stat.dev &&
        before.size > 0 &&
        before.size <= 1024 * 1024,
      "Resume receipt changed before reading.",
    );
    const bytes = readFileSync(fd);
    const after = fstatSync(fd);
    assert.ok(
      bytes.length === before.size &&
        before.size === after.size &&
        before.mtimeMs === after.mtimeMs &&
        before.ctimeMs === after.ctimeMs,
      "Resume receipt changed while reading.",
    );
    return {
      receipt: JSON.parse(bytes.toString("utf8")),
      sha256: createHash("sha256").update(bytes).digest("hex"),
      path,
    };
  } finally {
    closeSync(fd);
  }
}

export function verifyResumeReceipt(path, spec, plan) {
  const { receipt, sha256, path: receiptPath } = readPrivateReceipt(path);
  const runDirectory = dirname(receiptPath);
  assert.equal(basename(receiptPath), "cloud-receipt.json");
  assert.equal(receipt.schema_version, 1);
  assert.equal(receipt.status, "stopped");
  assert.notEqual(receipt.publish_attempted, true);
  assert.equal(receipt.channel, spec.channel);
  assert.equal(receipt.release_id, plan.release_id);
  assert.equal(receipt.expected_current_release_id, spec.expectedCurrent);
  assert.equal(receipt.build_directory, resolve(spec.buildDirectory));
  assert.match(receipt.operator?.binary_sha256 ?? "", /^[a-f0-9]{64}$/);
  assert.match(receipt.operator?.live_sha ?? "", /^[a-f0-9]{40}$/);
  const expectedSteps = [
    "read-current",
    "validate-prebuilt",
    "plan-and-metadata",
    "inspect-live-and-draft",
    "register-draft",
    "upload-immutable-artifacts",
    "verify-full-public-bytes",
  ];
  assert.deepEqual(
    receipt.steps?.map(({ name }) => name),
    expectedSteps,
  );
  assert.ok(
    receipt.steps.slice(0, -1).every(({ status }) => status === "complete"),
  );
  assert.equal(receipt.steps.at(-1).status, "failed");
  assert.equal(receipt.operation_receipts?.publish, undefined);

  function readReceiptFile(name, maxBytes = 2 * 1024 * 1024) {
    const file = join(runDirectory, name);
    const stat = lstatSync(file);
    assert.ok(
      stat.isFile() &&
        !stat.isSymbolicLink() &&
        stat.nlink === 1 &&
        stat.size > 0 &&
        stat.size <= maxBytes,
      "Resume evidence must be bounded regular files.",
    );
    const fd = openSync(
      file,
      constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
    );
    try {
      const before = fstatSync(fd);
      assert.ok(
        before.isFile() &&
          before.nlink === 1 &&
          before.size > 0 &&
          before.size <= maxBytes &&
          before.ino === stat.ino &&
          before.dev === stat.dev,
        "Resume evidence changed before reading.",
      );
      const bytes = readFileSync(fd);
      const after = fstatSync(fd);
      assert.ok(
        bytes.length === before.size &&
          before.size === after.size &&
          before.mtimeMs === after.mtimeMs &&
          before.ctimeMs === after.ctimeMs,
        "Resume evidence changed while reading.",
      );
      return bytes;
    } finally {
      closeSync(fd);
    }
  }
  const metadataBytes = readReceiptFile("metadata.json", 65536);
  assert.equal(
    createHash("sha256").update(metadataBytes).digest("hex"),
    receipt.metadata_sha256,
  );
  assert.deepEqual(
    JSON.parse(metadataBytes.toString("utf8")),
    createServiceReleaseMetadata(plan),
  );
  assert.deepEqual(
    JSON.parse(readReceiptFile("plan.json").toString("utf8")),
    plan,
  );
  let remoteScriptSha256;
  for (const [suffix, action] of [
    ["inspect-before", "inspect"],
    ["register", "register"],
  ]) {
    assert.equal(
      receipt.operation_receipts?.[suffix],
      join(runDirectory, `${suffix}.json`),
    );
    const operation = JSON.parse(
      readReceiptFile(`${suffix}.json`, 65536).toString("utf8"),
    );
    assert.equal(operation.request?.action, action);
    assert.match(operation.remote_script_sha256 ?? "", /^[a-f0-9]{64}$/);
    remoteScriptSha256 ??= operation.remote_script_sha256;
    assert.equal(operation.remote_script_sha256, remoteScriptSha256);
    assert.equal(operation.request?.metadata_sha256, receipt.metadata_sha256);
    assert.equal(
      operation.request?.binary_sha256,
      receipt.operator.binary_sha256,
    );
    assert.equal(
      operation.request?.live_release,
      receipt.operator.live_release,
    );
    assert.equal(operation.request?.live_sha, receipt.operator.live_sha);
    assert.equal(operation.remote_result?.result?.release_id, plan.release_id);
    assert.ok(
      (action === "inspect" ? ["not_registered", "draft"] : ["draft"]).includes(
        operation.remote_result?.result?.status,
      ),
    );
    assert.equal(
      operation.remote_result?.result?.observed_current_release_id,
      spec.expectedCurrent,
    );
    assert.equal(operation.exit_code, 0);
    assert.equal(operation.remote_result?.exit_code, 0);
  }
  const upload = JSON.parse(
    readReceiptFile("upload-result.json").toString("utf8"),
  );
  assert.equal(upload.release_id, plan.release_id);
  assert.equal(upload.immutable, true);
  assert.equal(
    upload.uploaded,
    new Set(plan.artifacts.map(({ file_name }) => file_name)).size,
  );
  return {
    sha256,
    operator: receipt.operator,
    remoteScriptSha256,
  };
}

export async function releaseTool(action, spec, resumeReceiptPath) {
  assert.ok(
    ["plan", "publish", "resume"].includes(action),
    "Use plan, publish or resume",
  );
  assert.equal(
    action === "resume",
    typeof resumeReceiptPath === "string",
    "Resume requires its previous cloud receipt",
  );
  const updaterConfigPath = join(
    ROOT,
    "config",
    `osir-updater-${spec.channel}.json`,
  );
  const plan = createReleasePlan({
    channel: spec.channel,
    buildDirectory: spec.buildDirectory,
    updaterConfigPath,
  });
  checkReleaseSpec(spec, plan);
  const resume =
    action === "resume"
      ? verifyResumeReceipt(resumeReceiptPath, spec, plan)
      : undefined;
  return runCloudRelease(
    [
      spec.channel,
      "--operator-config",
      spec.operatorConfig,
      "--build-directory",
      spec.buildDirectory,
      ...(action !== "plan" ? ["--publish"] : []),
    ],
    {
      expectedCurrentRelease: spec.expectedCurrent,
      resumeReceiptSha256: resume?.sha256,
      resumeReceiptOperator: resume?.operator,
      resumeRemoteScriptSha256: resume?.remoteScriptSha256,
      beforePublish: async ({
        plan: current,
        buildDirectory,
        runDirectory,
      }) => {
        checkReleaseSpec(spec, current);
        const backup = await backupRelease({
          channel: spec.channel,
          buildDirectory,
          updaterConfigPath,
          destination: spec.backupDirectory,
        });
        assert.equal(backup.available, true);
        assert.deepEqual(
          backup.missing_evidence,
          [],
          "Backup acceptance evidence is incomplete",
        );
        writeFileSync(
          join(runDirectory, "independent-backup.json"),
          JSON.stringify(backup, null, 2),
          { flag: "wx", mode: 0o600 },
        );
        const restored = restoreRelease({
          backupDirectory: spec.backupDirectory,
          destination: spec.restoreDirectory,
          expectedReceiptSha256: backup.receipt_sha256,
        });
        const restoredPlan = createReleasePlan({
          channel: spec.channel,
          buildDirectory: join(spec.restoreDirectory, "release"),
          updaterConfigPath,
        });
        checkReleaseSpec(
          { ...spec, buildDirectory: join(spec.restoreDirectory, "release") },
          restoredPlan,
        );
        writeFileSync(
          join(runDirectory, "independent-restore.json"),
          JSON.stringify(restored, null, 2),
          { flag: "wx", mode: 0o600 },
        );
      },
    },
  );
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    const [action, path, resumeReceiptPath, ...extra] = process.argv.slice(2);
    assert.ok(
      path &&
        !extra.length &&
        (action === "resume" ? resumeReceiptPath : !resumeReceiptPath),
      "Usage: osir-release-tool.mjs <plan|publish|resume> <release-spec.json> [failed-cloud-receipt.json]",
    );
    console.log(
      JSON.stringify(
        await releaseTool(
          action,
          JSON.parse(readFileSync(path, "utf8")),
          resumeReceiptPath,
        ),
        null,
        2,
      ),
    );
  } catch (error) {
    console.error("Release stopped: " + error.message);
    process.exitCode = 1;
  }
}
