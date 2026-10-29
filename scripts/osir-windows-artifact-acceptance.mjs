// Revalidate a fixed, existing build artifact. Never invokes Cargo or Tauri build.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { basename, join, resolve } from "node:path";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import { inspectPe } from "./osir-pe-verify.mjs";
import { verifyNsisPayload } from "./osir-nsis-payload.mjs";
const directory = resolve(process.env.OSIR_ARTIFACT_DIRECTORY);
const path = join(directory, "build-manifest.json");
const json = (file) => JSON.parse(readFileSync(file, "utf8"));
const digest = (file) => { const b = readFileSync(file); return { name: basename(file), bytes: b.length, sha256: createHash("sha256").update(b).digest("hex") }; };
const manifest = json(path);
function verifyOriginal() {
  assert.equal(manifest.sourceProvenance.sourceSha256, process.env.EXPECTED_SOURCE_SHA256);
  assert.equal(manifest.sourceProvenance.git.head, process.env.EXPECTED_APP_HEAD);
  assert.equal(manifest.sourceProvenance.verifiedAtEnd, true);
  assert.equal(manifest.sourceProvenance.git.dirty, false);
  assert.equal(manifest.sourceProvenance.git.dirtyAtStart, false);
  assert.equal(manifest.target, "x86_64-pc-windows-msvc");
  assert.equal(manifest.version, process.env.EXPECTED_APP_VERSION);
  assert.equal(manifest.signing, "windows-unsigned-updater-pending");
  assert.equal(manifest.updaterRuntimeConfigured, true);
  assert.equal(manifest.codeSignatureVerified, false);
  assert.equal(manifest.distributionSignatureVerified, false);
  const installer = digest(join(directory, manifest.installerArtifact.name));
  assert.deepEqual(installer, manifest.installerArtifact);
  assert.equal(installer.sha256, process.env.EXPECTED_INSTALLER_SHA256);
}
verifyOriginal();
if (process.argv[2] === "prepare") {
  const compiledPath = join(directory, "osir_client.exe");
  const compiledDigest = digest(compiledPath);
  assert.deepEqual(compiledDigest, manifest.mainExecutableArtifact);
  assert.equal(compiledDigest.sha256, process.env.EXPECTED_COMPILED_SHA256);
  const extractedPath = resolve(process.env.OSIR_EXTRACTED_MAIN);
  const marker = verifyNsisPayload(readFileSync(compiledPath), readFileSync(extractedPath));
  assert.equal(marker.nsisSha256, process.env.EXPECTED_NSIS_MAIN_SHA256);
  const pe = inspectPe(extractedPath, { expectedMachine: "x86_64" });
  assert.equal(pe.authenticodeCertificatePresent, false);
  renameSync(join(directory, "windows-packaged-smoke.json"), join(directory, "original-failed-smoke.json"));
  renameSync(compiledPath, join(directory, "compiled-unbundled.exe"));
  writeFileSync(compiledPath, readFileSync(extractedPath));
  manifest.compiledExecutableArtifact = { ...compiledDigest, name: "compiled-unbundled.exe" };
  manifest.mainExecutableArtifact = digest(compiledPath);
  manifest.mainPe = pe;
  manifest.nsisPayloadVerification = marker;
  manifest.originalCiRun = Number(process.env.SOURCE_RUN_ID);
  manifest.originalArtifactName = process.env.SOURCE_ARTIFACT_NAME;
  manifest.originalArtifactManifestSha256 = digest(path).sha256;
  manifest.packagedLifecycleAcceptance = "pending-CI-smoke";
  writeFileSync(path, JSON.stringify(manifest, null, 2) + "\n");
} else {
  assert.equal(process.argv[2], "accept");
  const reportPath = join(directory, "windows-packaged-smoke.json");
  const report = json(reportPath);
  assert.equal(report.passed, true);
  assert.equal(report.sourceSha256, process.env.EXPECTED_SOURCE_SHA256);
  assert.equal(report.gitHead, process.env.EXPECTED_APP_HEAD);
  assert.equal(report.installerSha256, process.env.EXPECTED_INSTALLER_SHA256);
  assert.equal(report.installerAuthenticodeStatus, "NotSigned");
  for (const stage of ["install", "launch", "upgrade", "uninstall"]) assert.equal(report.stages[stage].passed, true);
  for (const stage of ["install", "upgrade"]) {
    assert.equal(report.stages[stage].sha256, process.env.EXPECTED_NSIS_MAIN_SHA256);
    assert.equal(report.stages[stage].architecture, "x86_64");
    assert.equal(report.stages[stage].authenticodeStatus, "NotSigned");
  }
  assert.equal(report.stages.launch.windowObserved, true);
  assert.equal(report.stages.launch.remainedRunning, true);
  assert.equal(report.stages.launch.windowTitle, "OSIR Client");
  assert.ok(report.stages.launch.windowWidth >= 900 && report.stages.launch.windowHeight >= 600);
  assert.equal(report.stages.launch.excludesSingleInstanceToolWindow, true);
  assert.equal(report.stages.upgrade.kind, "same-version-reinstall-update-path");
  assert.equal(report.stages.uninstall.mainExecutableRemoved, true);
  const toolFiles = ["scripts/osir-windows-artifact-acceptance.mjs", "scripts/osir-nsis-payload.mjs", "scripts/osir-windows-packaged-smoke.ps1", "scripts/osir-pe-verify.mjs", ".github/workflows/osir-windows-artifact-acceptance.yml"].map((file) => ({ path: file, ...digest(file) }));
  const git = spawnSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" });
  assert.equal(git.status, 0);
  assert.equal(git.stdout.trim(), process.env.GITHUB_SHA);
  manifest.packagedLifecycleAcceptance = "passed-CI-smoke";
  manifest.packagedLifecycleReport = digest(reportPath);
  manifest.acceptanceToolProvenance = { schemaVersion: 1, gitHead: git.stdout.trim(),
    sha256: createHash("sha256").update(JSON.stringify(toolFiles)).digest("hex"), files: toolFiles,
    runId: Number(process.env.GITHUB_RUN_ID), runtimeBuildPerformed: false };
  writeFileSync(path, JSON.stringify(manifest, null, 2) + "\n");
  writeFileSync(join(directory, "SHA256SUMS"), [manifest.installerArtifact, manifest.mainExecutableArtifact, manifest.compiledExecutableArtifact, manifest.packagedLifecycleReport].map((x) => `${x.sha256}  ${x.name}`).join("\n") + "\n");
}
