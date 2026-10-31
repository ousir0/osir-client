import test from "node:test";
import assert from "node:assert/strict";
import { checkReleaseSpec } from "./osir-release-tool.mjs";
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
  git_head: spec.gitHead,
  artifacts: ["darwin-aarch64", "darwin-x86_64", "windows-x86_64"].flatMap(
    (platform) => ["installer", "updater"].map((kind) => ({ platform, kind })),
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
