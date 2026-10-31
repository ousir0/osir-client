#!/usr/bin/env node
import assert from "node:assert/strict";
import {
  constants,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  createReleasePlan,
  inspectArtifact,
} from "./osir-release-publisher.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const platforms = ["darwin-aarch64", "darwin-x86_64", "windows-x86_64"];
const names = ["mac-arm64", "mac-intel", "windows-x64"];
const json = (path) => JSON.parse(readFileSync(path, "utf8"));
export function checkedName(name) {
  assert.ok(
    typeof name === "string" &&
      /^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(name) &&
      !name.includes(".."),
    "Unsafe artifact filename",
  );
  return name;
}
function copy(source, target) {
  const before = inspectArtifact(source);
  if (!existsSync(target))
    copyFileSync(source, target, constants.COPYFILE_EXCL);
  const after = inspectArtifact(target);
  assert.equal(before.sha256, after.sha256);
  assert.equal(before.size_bytes, after.size_bytes);
}
export function combineRelease({
  channel,
  inputs,
  output,
  updaterConfigPath = join(root, "config", `osir-updater-${channel}.json`),
}) {
  assert.ok(["prod", "beta"].includes(channel));
  assert.equal(inputs.length, 3);
  output = resolve(output);
  assert.ok(!existsSync(output), "Output must be new");
  const prepared = inputs.map((input, index) => {
    const directory = resolve(input);
    const plan = createReleasePlan({
      channel,
      buildDirectory: directory,
      updaterConfigPath,
    });
    assert.deepEqual(
      plan.artifacts.map((a) => `${a.platform}/${a.kind}`).sort(),
      ["installer", "updater"].map((kind) => `${platforms[index]}/${kind}`),
    );
    const manifest = json(join(directory, "build-manifest.json"));
    const records = [
      manifest.mainExecutableArtifact,
      manifest.packagedLifecycleReport,
      manifest.packagedInstallerScreenshot,
      manifest.packagedWindowScreenshot,
      manifest.packagedRendererReport,
      manifest.packagedMacAcceptance,
      ...(manifest.packagedRendererScreenshots ?? []),
      ...(manifest.packagedMacScreenshots ?? []),
    ].filter(Boolean);
    if (index < 2)
      assert.ok(
        manifest.nativeAcceptance,
        "Mac native acceptance binding is required",
      );
    if (manifest.nativeAcceptance)
      records.push({
        ...manifest.nativeAcceptance,
        name: manifest.nativeAcceptance.path,
      });
    for (const record of records) {
      const info = inspectArtifact(join(directory, checkedName(record.name)));
      assert.equal(info.sha256, record.sha256, "Evidence hash differs");
      if (record.bytes !== undefined)
        assert.equal(info.size_bytes, record.bytes);
    }
    return { directory, plan, manifest, records };
  });
  for (const { plan } of prepared)
    for (const key of ["version", "release_id", "source_sha", "git_head"])
      assert.equal(plan[key], prepared[0].plan[key], `Platform ${key} differs`);
  mkdirSync(output, { mode: 0o700 });
  prepared.forEach(({ directory, plan, records }, index) => {
    const child = join(output, names[index]);
    mkdirSync(child, { mode: 0o700 });
    const files = new Set([
      "build-manifest.json",
      `updater-${channel}.json`,
      ...records.map((r) => r.name),
    ]);
    for (const artifact of plan.artifacts) {
      const artifactFiles = [
        artifact.file_name,
        ...(artifact.kind === "updater" ? [artifact.file_name + ".sig"] : []),
      ];
      for (const name of artifactFiles) {
        checkedName(name);
        files.add(name);
        copy(join(directory, name), join(output, name));
      }
    }
    for (const name of files)
      copy(join(directory, checkedName(name)), join(child, name));
  });
  writeFileSync(
    join(output, "build-manifest.json"),
    JSON.stringify(
      { mode: "release-multiplatform-distribution", builds: names },
      null,
      2,
    ),
    { flag: "wx", mode: 0o600 },
  );
  return createReleasePlan({
    channel,
    buildDirectory: output,
    updaterConfigPath,
  });
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    const [channel, output, ...inputs] = process.argv.slice(2);
    assert.ok(
      output && inputs.length === 3,
      "Usage: osir-release-combine.mjs <prod|beta> <new-output> <ARM> <Intel> <Windows>",
    );
    console.log(
      JSON.stringify(combineRelease({ channel, output, inputs }), null, 2),
    );
  } catch (error) {
    console.error("Combine stopped: " + error.message);
    process.exitCode = 1;
  }
}
