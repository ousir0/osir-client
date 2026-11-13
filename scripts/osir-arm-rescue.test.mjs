import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import { childEnvironment, filters, reviewed, validateSourceIdentity, validateTestLog } from './osir-arm-rescue.mjs';

const identity = () => ({ commit: reviewed.sourceCommit, sourceSha256: reviewed.sourceSha256, inputsManifestSha256: reviewed.inputsManifestSha256, dirty: false });
const log = (filter, count = 1) => Array.from({ length: count }, (_, i) => `test module::${filter}::case_${i} ... ok`).join('\n') + `\n\ntest result: ok. ${count} passed; 0 failed; 0 ignored; 0 measured; 4915 filtered out; finished in 0.20s\n`;
test('r2 contract rejects the superseded r1 source explicitly', () => {
  assert.equal(reviewed.sourceCommit, '8bebc7a884b51336590e0b54313f70f3f4364509');
  assert.equal(reviewed.sourceSha256, 'a376ca7fc36ad9546b8095276cd736b282833513f22d7bc5dce1e44d75914017');
  assert.throws(() => validateSourceIdentity({ ...identity(), commit: 'a5b49ece14c0abee1599fae8e9e5f47e3012d25b' }));
});
test('exact source must retain commit, file bytes, whitelist and clean checkout', () => {
  validateSourceIdentity(identity());
  for (const key of ['commit', 'sourceSha256', 'inputsManifestSha256']) {
    const changed = identity(); changed[key] = '0'.repeat(changed[key].length);
    assert.throws(() => validateSourceIdentity(changed));
  }
  assert.throws(() => validateSourceIdentity({ ...identity(), dirty: true }));
});
test('all original filters accept actual nonzero native passing cases', () => {
  for (const filter of filters) assert.equal(validateTestLog(log(filter, 13), filter, 0).passed, 13);
});
test('zero tests, failures, missing summary, wrong names and duplicate summaries are rejected', () => {
  const filter = filters[0];
  for (const text of [log(filter, 0), log(filter).replace('0 failed', '1 failed'),
    log(filter).replace('result: ok', 'result: FAILED'), 'running 13 tests\n',
    log('other::tests'), log(filter) + log(filter),
    log(filter).replace('1 passed', '13 passed')]) {
    assert.throws(() => validateTestLog(text, filter, 0));
  }
  assert.throws(() => validateTestLog(log(filter), filter, 1));
  assert.throws(() => validateTestLog(log(filter), filter, null));
  assert.throws(() => validateTestLog(log(filter), 'unreviewed', 0));
});
test('source compatibility SHA is child-only, actual producer identity is retained', () => {
  const producer = { GITHUB_SHA: '1'.repeat(40), GITHUB_WORKFLOW_SHA: '2'.repeat(40), GITHUB_REF: 'refs/heads/codex/arm-rescue' };
  const child = childEnvironment(producer, reviewed.sourceCommit);
  assert.equal(child.GITHUB_SHA, reviewed.sourceCommit);
  assert.equal(producer.GITHUB_SHA, '1'.repeat(40));
  assert.equal(child.GITHUB_WORKFLOW_SHA, producer.GITHUB_WORKFLOW_SHA);
  assert.equal(child.GITHUB_REF, producer.GITHUB_REF);
  assert.throws(() => childEnvironment(producer, '3'.repeat(40)));
});
test('workflow keeps separate checkouts, original gates, single Cargo, failure cache and no global SHA rewrite', () => {
  const workflow = readFileSync(new URL('../.github/workflows/osir-macos-arm-release.yml', import.meta.url), 'utf8');
  assert.match(workflow, /runs-on: macos-15/);
  assert.match(workflow, /path: control/);
  assert.match(workflow, /path: source/);
  assert.match(workflow, /cache-on-failure: true/);
  assert.match(workflow, /OSIR_RESCUE_EVIDENCE: \$\{\{ github.workspace \}\}\/osir-arm-rescue-evidence/);
  assert.doesNotMatch(workflow, /\$\{\{ runner.temp \}\}/);
  assert.match(workflow, /CARGO_BUILD_JOBS: '1'/);
  assert.doesNotMatch(workflow, /\brg\b|GITHUB_SHA:|continue-on-error|git push|workflow run|--local-unsigned/);
  for (const step of ['preflight', 'check', 'build', 'output', 'smoke', 'accept'])
    assert.match(workflow, new RegExp(`source-script ${step}\\b`));
});
