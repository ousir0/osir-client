import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, renameSync, writeFileSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { assertRendererEvidence } from './osir-packaged-renderer-check.mjs';

const directory = resolve(process.env.OSIR_ARTIFACT_DIRECTORY);
const manifestPath = join(directory, 'build-manifest.json');
const json = path => JSON.parse(readFileSync(path));
const digest = path => { const bytes = readFileSync(path); return { name: basename(path), bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') }; };
const manifest = json(manifestPath);
assert.equal(manifest.version, '0.1.1');
assert.equal(manifest.sourceProvenance.sourceSha256, 'a133b77561f032c21ff6c97d1e98ec9df7d4b1afd7d507c8e44208b846fa746a');
assert.equal(manifest.sourceProvenance.git.head, '414e111c103236047a7bd36a92b2eb2137f23b65');
assert.equal(manifest.sourceProvenance.verifiedAtEnd, true);
assert.equal(manifest.sourceProvenance.git.dirty, false);
assert.equal(manifest.mode, 'ci-pending-local-finalization');
assert.equal(manifest.target, 'x86_64-pc-windows-msvc');
for (const [field, expected] of [['installerArtifact', process.env.EXPECTED_INSTALLER_SHA256], ['mainExecutableArtifact', process.env.EXPECTED_MAIN_SHA256]]) {
  assert.deepEqual(digest(join(directory, manifest[field].name)), manifest[field]);
  assert.equal(manifest[field].sha256, expected);
}
if (process.argv[2] === 'prepare') {
  renameSync(join(directory, 'windows-packaged-smoke.json'), join(directory, 'original-failed-smoke.json'));
  writeFileSync(join(directory, 'original-manifest.json'), readFileSync(manifestPath));
} else {
  assert.equal(process.argv[2], 'accept');
  const reportPath = join(directory, 'windows-packaged-smoke.json');
  const report = json(reportPath);
  assert.equal(report.passed, true);
  assert.equal(report.sourceSha256, manifest.sourceProvenance.sourceSha256);
  assert.equal(report.gitHead, manifest.sourceProvenance.git.head);
  assert.equal(report.installerSha256, manifest.installerArtifact.sha256);
  for (const stage of ['installerOpen', 'install', 'launch', 'reopen', 'startupFailure', 'upgrade', 'uninstall']) assert.equal(report.stages[stage].passed, true);
  for (const key of ['windowObserved', 'usableNavigation', 'nextActionInvoked', 'nextPageObserved']) assert.equal(report.stages.installerOpen[key], true);
  for (const key of ['nativeErrorVisible', 'recoveryMessageVisible', 'diagnosticSaved', 'confirmationInvoked']) assert.equal(report.stages.startupFailure[key], true);
  assert.equal(report.stages.startupFailure.exitCode, 1);
  for (const stage of ['install', 'upgrade']) assert.equal(report.stages[stage].sha256, manifest.mainExecutableArtifact.sha256);
  assert.equal(report.stages.launch.windowTitle, 'OSIR Client');
  assert.equal(report.stages.launch.excludesSingleInstanceToolWindow, true);
  assert.equal(report.stages.reopen.rendererAndNativeIpcPassed, true);
  const rendererPath = join(directory, 'windows-renderer-smoke.json');
  const renderer = json(rendererPath);
  assertRendererEvidence(renderer, manifest);
  manifest.packagedLifecycleAcceptance = 'passed-CI-smoke';
  manifest.packagedLifecycleReport = digest(reportPath);
  manifest.packagedRendererReport = digest(rendererPath);
  manifest.packagedWindowScreenshot = digest(join(directory, 'windows-app-window.png'));
  manifest.packagedInstallerScreenshot = digest(join(directory, 'windows-installer-window.png'));
  manifest.packagedRendererScreenshots = [renderer.launchRun, renderer].flatMap(run => run.viewports.map(view => digest(join(directory, view.screenshot))));
  manifest.acceptanceToolProvenance = { gitHead: process.env.GITHUB_SHA, runId: Number(process.env.GITHUB_RUN_ID), runtimeBuildPerformed: false, originalRun: Number(process.env.SOURCE_RUN_ID), originalManifest: digest(join(directory, 'original-manifest.json')), files: ['scripts/osir-existing-011-acceptance.mjs', 'scripts/osir-windows-packaged-smoke.ps1', 'scripts/osir-packaged-renderer-check.mjs', '.github/workflows/osir-windows-artifact-acceptance.yml'].map(path => ({ path, ...digest(path) })) };
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
}
