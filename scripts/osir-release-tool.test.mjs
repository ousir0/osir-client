import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  chmodSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { checkReleaseSpec, verifyResumeReceipt } from "./osir-release-tool.mjs";
import { createServiceReleaseMetadata } from "./osir-release-publisher.mjs";
import { checkedName } from "./osir-release-combine.mjs";
test("combine rejects traversal and ambiguous evidence names", () => {
  assert.equal(
    checkedName("macos-launch-window.png"),
    "macos-launch-window.png",
  );
  for (const name of [
    "../secret",
    "/tmp/secret",
    "a/b",
    "a\\b",
    "..",
    "",
    null,
  ])
    assert.throws(() => checkedName(name));
});
const spec = {
  channel: "prod",
  version: "1.2.3",
  sourceSha256: "a".repeat(64),
  gitHead: "b".repeat(40),
  expectedCurrent: "prod-1.2.2",
  buildDirectory: "/tmp/build",
  operatorConfig: "/tmp/operator",
  backupDirectory: "/tmp/backup",
  restoreDirectory: "/tmp/restore",
};
const plan = {
  channel: spec.channel,
  version: spec.version,
  source_sha: spec.sourceSha256,
  release_id: "prod-1.2.3",
  notes: "fixture release",
  git_head: spec.gitHead,
  artifacts: ["darwin-aarch64", "darwin-x86_64", "windows-x86_64"].flatMap(
    (platform) =>
      ["installer", "updater"].map((kind, index) => ({
        platform,
        kind,
        file_name: `${platform}-${kind}`,
        url: `https://osirclaw.com/api/v1/osir-client/artifacts/prod-1.2.3/${platform}-${kind}`,
        sha256: String(index + 1).repeat(64),
        size_bytes: index + 1,
        ...(kind === "updater" ? { signature: "fixture-signature" } : {}),
      })),
  ),
};
test("reviewed release requires exact source, version and six platform roles", () => {
  checkReleaseSpec(spec, plan);
  for (const changed of [
    { version: "1.2.4" },
    { source_sha: "c".repeat(64) },
    { git_head: "c".repeat(40) },
    { artifacts: plan.artifacts.slice(1) },
    { artifacts: [...plan.artifacts, plan.artifacts[0]] },
  ])
    assert.throws(() => checkReleaseSpec(spec, { ...plan, ...changed }));
  assert.throws(() =>
    checkReleaseSpec({ ...spec, expectedCurrent: undefined }, plan),
  );
  assert.throws(() =>
    checkReleaseSpec({ ...spec, backupDirectory: "relative" }, plan),
  );
});

test("resume accepts only a matching pre-publication failure after upload", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "osir-resume-test-"));
  chmodSync(directory, 0o700);
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const metadata = Buffer.from(
    JSON.stringify(createServiceReleaseMetadata(plan)) + "\n",
  );
  const writeJson = (name, value) =>
    writeFileSync(join(directory, name), JSON.stringify(value), {
      mode: 0o600,
    });
  writeFileSync(join(directory, "metadata.json"), metadata, { mode: 0o600 });
  writeJson("plan.json", plan);
  writeJson("upload-result.json", {
    release_id: plan.release_id,
    immutable: true,
    uploaded: new Set(plan.artifacts.map(({ file_name }) => file_name)).size,
  });
  const steps = [
    "read-current",
    "validate-prebuilt",
    "plan-and-metadata",
    "inspect-live-and-draft",
    "register-draft",
    "upload-immutable-artifacts",
    "verify-full-public-bytes",
  ].map((name, index) => ({
    name,
    status: index === 6 ? "failed" : "complete",
  }));
  const receiptPath = join(directory, "cloud-receipt.json");
  const remoteScriptSha256 = "e".repeat(64);
  writeJson("cloud-receipt.json", {
    schema_version: 1,
    status: "stopped",
    channel: spec.channel,
    release_id: plan.release_id,
    expected_current_release_id: spec.expectedCurrent,
    build_directory: resolve(spec.buildDirectory),
    metadata_sha256: createHash("sha256").update(metadata).digest("hex"),
    operator: {
      binary_sha256: "c".repeat(64),
      live_release: "prod-live",
      live_sha: "d".repeat(40),
    },
    operation_receipts: {
      "inspect-before": join(directory, "inspect-before.json"),
      register: join(directory, "register.json"),
    },
    steps,
  });
  for (const [suffix, action, status] of [
    ["inspect-before", "inspect", "not_registered"],
    ["register", "register", "draft"],
  ]) {
    writeJson(`${suffix}.json`, {
      remote_script_sha256: remoteScriptSha256,
      request: {
        action,
        metadata_sha256: createHash("sha256").update(metadata).digest("hex"),
        binary_sha256: "c".repeat(64),
        live_release: "prod-live",
        live_sha: "d".repeat(40),
      },
      remote_result: {
        exit_code: 0,
        result: {
          release_id: plan.release_id,
          status,
          observed_current_release_id: spec.expectedCurrent,
        },
      },
      exit_code: 0,
    });
  }
  assert.deepEqual(verifyResumeReceipt(receiptPath, spec, plan).operator, {
    binary_sha256: "c".repeat(64),
    live_release: "prod-live",
    live_sha: "d".repeat(40),
  });
  assert.equal(
    verifyResumeReceipt(receiptPath, spec, plan).remoteScriptSha256,
    remoteScriptSha256,
  );
  const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
  const registered = JSON.parse(
    readFileSync(join(directory, "register.json"), "utf8"),
  );
  writeJson("register.json", {
    ...registered,
    request: { ...registered.request, metadata_sha256: "f".repeat(64) },
  });
  assert.throws(() => verifyResumeReceipt(receiptPath, spec, plan));
  writeJson("register.json", registered);
  assert.throws(() =>
    verifyResumeReceipt(
      receiptPath,
      { ...spec, expectedCurrent: "prod-1.2.1" },
      plan,
    ),
  );
  assert.throws(() =>
    verifyResumeReceipt(receiptPath, spec, {
      ...plan,
      source_sha: "f".repeat(64),
    }),
  );
  receipt.publish_attempted = true;
  writeJson("cloud-receipt.json", receipt);
  assert.throws(() => verifyResumeReceipt(receiptPath, spec, plan));
});
