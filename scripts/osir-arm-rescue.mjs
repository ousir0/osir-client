// External control harness. Never edits frozen product sources or release refs.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const reviewed = Object.freeze({
  repository: 'ousir0/osir-client',
  sourceCommit: '8bebc7a884b51336590e0b54313f70f3f4364509',
  sourceSha256: 'a376ca7fc36ad9546b8095276cd736b282833513f22d7bc5dce1e44d75914017',
  inputsManifestSha256: 'fd4b85f2d2963d77245cdd27f2c55fd7270ebb82be7b2d3b07430786619c1fcd',
  target: 'aarch64-apple-darwin',
});
export const filters = Object.freeze(['osir_identity::tests', 'startup::tests', 'database::compatibility::tests', 'anywhere_']);
export function validateSourceIdentity({ commit, sourceSha256, inputsManifestSha256, dirty }, expected = reviewed) {
  assert.equal(commit, expected.sourceCommit, 'Wrong frozen commit');
  assert.equal(sourceSha256, expected.sourceSha256, 'Frozen source bytes changed');
  assert.equal(inputsManifestSha256, expected.inputsManifestSha256, 'Source whitelist changed');
  assert.equal(dirty, false, 'Frozen source checkout must be clean');
}
export function validateTestLog(text, filter, exitCode) {
  assert.ok(filters.includes(filter), 'Unreviewed regression filter');
  assert.equal(exitCode, 0, 'Cargo exited unsuccessfully');
  const summaries = [...text.matchAll(/^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed;.*$/gm)];
  assert.equal(summaries.length, 1, 'Expected exactly one native lib-test summary');
  assert.equal(summaries[0][1], 'ok');
  assert.equal(Number(summaries[0][3]), 0);
  const passed = Number(summaries[0][2]);
  assert.ok(passed > 0, 'Native regression selected no passing tests');
  const cases = [...text.matchAll(/^test (\S+) \.\.\. ok\s*$/gm)].map((match) => match[1]);
  assert.equal(cases.length, passed, 'Summary differs from observed passing test cases');
  assert.ok(cases.every((name) => name.includes(filter)), 'Wrong regression filter output');
  return { filter, passed, failed: 0, exitCode, cases };
}
export function childEnvironment(env, sourceCommit) {
  assert.equal(sourceCommit, reviewed.sourceCommit);
  // Compatibility input ONLY for the frozen source validators. Parent Actions
  // GITHUB_SHA / GITHUB_WORKFLOW_SHA remain unchanged in the separate receipt.
  return { ...env, GITHUB_SHA: sourceCommit };
}
function checked(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, encoding: 'utf8', timeout: 15000 });
  assert.equal(result.status, 0, `${command} failed: ${result.stderr}`);
  return result.stdout.trim();
}
const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
const json = (path) => JSON.parse(readFileSync(path, 'utf8'));
const write = (path, data) => writeFileSync(path, JSON.stringify(data, null, 2) + '\n');

export async function inspectSource(source) {
  // Check Git HEAD before importing/executing any product script.
  const commit = checked('git', ['rev-parse', '--verify', 'HEAD'], source);
  assert.equal(commit, reviewed.sourceCommit);
  const dirty = checked('git', ['status', '--porcelain=v1', '--untracked-files=normal'], source) !== '';
  const { sourceSnapshot } = await import(pathToFileURL(join(source, 'scripts/osir-build.mjs')).href);
  const { provenance } = sourceSnapshot();
  const identity = { commit, dirty, sourceSha256: provenance.sourceSha256, inputsManifestSha256: provenance.inputsManifestSha256 };
  validateSourceIdentity(identity);
  return { ...identity, fileCount: provenance.fileCount };
}
async function execute(command, args, { cwd, env, log }) {
  let resultText = '';
  const child = spawn(command, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
  for (const [stream, destination] of [[child.stdout, process.stdout], [child.stderr, process.stderr]]) {
    stream.on('data', (chunk) => {
      destination.write(chunk);
      if (log) { appendFileSync(log, chunk); resultText += chunk.toString(); }
    });
  }
  const result = await new Promise((done, reject) => {
    child.once('error', reject);
    child.once('close', (exitCode, signal) => done({ exitCode, signal }));
  });
  return { ...result, text: resultText };
}
async function main() {
  const [command, ...args] = process.argv.slice(2);
  const control = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const source = resolve(process.env.OSIR_RESCUE_SOURCE || 'source');
  const evidence = resolve(process.env.OSIR_RESCUE_EVIDENCE || 'evidence');
  if (command === 'inspect') {
    console.log(JSON.stringify(await inspectSource(source), null, 2));
    return;
  }
  assert.equal(process.env.GITHUB_ACTIONS, 'true', 'Execution requires disposable GitHub runner');
  assert.equal(process.env.CI, 'true');
  assert.equal(process.platform, 'darwin');
  assert.equal(process.arch, 'arm64', 'Native ARM process required');
  assert.equal(process.env.OSIR_RESCUE_SOURCE_COMMIT, reviewed.sourceCommit);
  assert.equal(process.env.OSIR_SOURCE_SHA256, reviewed.sourceSha256);
  assert.equal(process.env.OSIR_CHANNEL, 'prod');
  assert.equal(process.env.GITHUB_REPOSITORY, reviewed.repository);
  const identity = await inspectSource(source);
  const receiptPath = join(evidence, 'producer-receipt.json');
  if (command === 'init') {
    assert.ok(!existsSync(evidence), 'Evidence must start empty');
    assert.match(process.env.GITHUB_SHA || '', /^[a-f0-9]{40}$/);
    assert.match(process.env.GITHUB_WORKFLOW_SHA || '', /^[a-f0-9]{40}$/);
    const controlHead = checked('git', ['rev-parse', 'HEAD'], control);
    assert.equal(controlHead, process.env.GITHUB_SHA, 'Control checkout differs from dispatch');
    assert.equal(checked('git', ['status', '--porcelain=v1'], control), '');
    mkdirSync(evidence, { recursive: true });
    write(receiptPath, {
      schemaVersion: 1, purpose: 'frozen-client-native-arm-rescue',
      producerWorkflowSha: process.env.GITHUB_WORKFLOW_SHA,
      dispatchHead: process.env.GITHUB_SHA, controlHead,
      dispatchRef: process.env.GITHUB_REF, workflowRef: process.env.GITHUB_WORKFLOW_REF,
      runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT,
      repository: reviewed.repository, sourceCommit: identity.commit,
      sourceSha256: identity.sourceSha256, inputsManifestSha256: identity.inputsManifestSha256,
      sourceDirectory: source, sourceDirty: identity.dirty, productOverlay: [],
      harnessSha256: hash(fileURLToPath(import.meta.url)),
      workflowFileSha256: hash(join(control, '.github/workflows/osir-macos-arm-release.yml')),
      sourceScriptGithubShaCompatibilityInput: identity.commit,
      previousFailedRun: '43510587331', startedAt: new Date().toISOString(), steps: [],
      acceptance: 'not-run', publication: 'not-attempted-by-this-harness',
    });
    return;
  }
  const receipt = json(receiptPath);
  assert.equal(receipt.dispatchHead, process.env.GITHUB_SHA);
  assert.equal(receipt.producerWorkflowSha, process.env.GITHUB_WORKFLOW_SHA);
  assert.equal(receipt.sourceCommit, identity.commit);
  if (command === 'regressions') {
    for (const filter of filters) {
      const log = join(evidence, `native-${filter.replaceAll(':', '_')}.log`);
      assert.ok(!existsSync(log));
      const result = await execute('cargo', ['test', '--release', '--locked', '--target', reviewed.target,
        '--manifest-path', 'src-tauri/Cargo.toml', '--lib', filter, '--', '--test-threads=1'], {
        cwd: source, env: process.env, log,
      });
      const step = { command: 'native-regression', filter, exitCode: result.exitCode, signal: result.signal, logSha256: hash(log), passed: false };
      receipt.steps.push(step);
      try {
        const { passed: passedCount, ...validated } = validateTestLog(result.text, filter, result.exitCode);
        Object.assign(step, validated, { passedCount, passed: true });
      }
      finally { write(receiptPath, receipt); }
    }
    return;
  }
  assert.equal(command, 'source-script');
  const operations = {
    preflight: ['osir-ci-artifact.mjs', 'preflight', 'prod', reviewed.target],
    check: ['osir-build-macos-ci.mjs', 'prod', '--target', reviewed.target, '--check'],
    build: ['osir-build-macos-ci.mjs', 'prod', '--target', reviewed.target],
    output: ['osir-ci-artifact.mjs', 'mac-output', 'prod', reviewed.target],
    smoke: ['osir-macos-packaged-smoke.mjs', process.env.OSIR_ARTIFACT_DIRECTORY],
    accept: ['osir-ci-artifact.mjs', 'accept-mac', 'prod', reviewed.target],
  };
  assert.ok(Object.hasOwn(operations, args[0]) && args.length === 1);
  if (args[0] === 'smoke') assert.ok(process.env.OSIR_ARTIFACT_DIRECTORY);
  if (args[0] === 'build' || args[0] === 'accept') {
    for (const filter of filters) assert.ok(receipt.steps.some((step) => step.filter === filter && step.passed === true), `Missing successful tests: ${filter}`);
  }
  if (args[0] === 'accept') assert.ok(receipt.steps.some((step) => step.command === 'smoke' && step.exitCode === 0));
  const [script, ...scriptArgs] = operations[args[0]];
  const result = await execute(process.execPath, [join(source, 'scripts', script), ...scriptArgs], {
    cwd: source, env: childEnvironment(process.env, identity.commit),
  });
  receipt.steps.push({ command: args[0], exitCode: result.exitCode, signal: result.signal, finishedAt: new Date().toISOString() });
  write(receiptPath, receipt);
  assert.equal(result.exitCode, 0, `Frozen source operation failed: ${args[0]}`);
  const after = await inspectSource(source);
  receipt.sourceVerifiedAfterLastOperation = after;
  if (args[0] === 'accept') receipt.acceptance = 'original-frozen-source-native-arm-gate-passed';
  write(receiptPath, receipt);
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch((error) => { console.error(error); process.exitCode = 1; });
