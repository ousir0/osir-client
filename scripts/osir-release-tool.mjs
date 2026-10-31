#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve, join, isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";
import { runCloudRelease } from "./osir-release-cloud.mjs";
import { createReleasePlan } from "./osir-release-publisher.mjs";
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

export async function releaseTool(action, spec) {
  assert.ok(["plan", "publish"].includes(action), "Use plan or publish");
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
  return runCloudRelease(
    [
      spec.channel,
      "--operator-config",
      spec.operatorConfig,
      "--build-directory",
      spec.buildDirectory,
      ...(action === "publish" ? ["--publish"] : []),
    ],
    {
      expectedCurrentRelease: spec.expectedCurrent,
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
    const [action, path, ...extra] = process.argv.slice(2);
    assert.ok(
      path && !extra.length,
      "Usage: osir-release-tool.mjs <plan|publish> <release-spec.json>",
    );
    console.log(
      JSON.stringify(
        await releaseTool(action, JSON.parse(readFileSync(path, "utf8"))),
        null,
        2,
      ),
    );
  } catch (error) {
    console.error("Release stopped: " + error.message);
    process.exitCode = 1;
  }
}
