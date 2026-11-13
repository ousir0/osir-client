import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import { assertDevConfig, assertDevManifest, assertProxyRows, nativeArguments } from './osir-dev-arm.mjs';
import { reviewed } from './osir-arm-rescue.mjs';

const config = () => ({ identifier: 'com.osir.client.dev', productName: 'OSIR Client Dev',
  plugins: { 'deep-link': { desktop: { schemes: ['osir-client-dev'] } }, updater: { endpoints: [], pubkey: '' } },
  bundle: { createUpdaterArtifacts: false, targets: ['app'], icon: ['icons/osir/dev/icon.icns'] },
  build: { devUrl: null, frontendDist: '../dist' } });
const manifest = () => ({ mode: 'acceptance-only-genuine-dev', channel: 'dev', identifier: 'com.osir.client.dev',
  version: '0.1.10', target: reviewed.target, architectures: ['arm64'], updaterEnabled: false, distributionSignatureVerified: false,
  sourceProvenance: { git: { head: reviewed.sourceCommit, dirty: false, dirtyAtStart: false }, sourceSha256: reviewed.sourceSha256,
    inputsManifestSha256: reviewed.inputsManifestSha256, verifiedAtEnd: true },
  releaseDiagnostics: { report: { identifier: 'com.osir.client.dev', releaseBuild: true, updateDistribution: { updatesEnabled: false } } } });
test('native compile selects genuine Dev identity, embedded renderer and no updater', () => {
  assertDevConfig(config());
  const args = nativeArguments(config());
  assert.ok(args.includes('--locked')); assert.ok(args.includes('--no-sign'));
  assert.deepEqual(args.slice(-2), ['-j', '1']);
  assert.ok(!args.includes('--debug')); assert.ok(args.includes('aarch64-apple-darwin'));
  assert.equal(JSON.parse(args[args.indexOf('--config') + 1]).identifier, 'com.osir.client.dev');
});
test('reject production identity, scheme, updater, icons and live Vite configurations', () => {
  const mutate = [c => c.identifier = 'com.osir.client', c => c.plugins['deep-link'].desktop.schemes = ['osir-client'],
    c => c.plugins.updater.endpoints = ['https://production.invalid/feed'], c => c.plugins.updater.pubkey = 'key',
    c => c.plugins['osir-updates'] = {}, c => c.bundle.createUpdaterArtifacts = true,
    c => c.bundle.icon = ['icons/osir/prod/icon.icns'], c => c.build.devUrl = 'http://localhost:1420'];
  for (const change of mutate) { const c = config(); change(c); assert.throws(() => assertDevConfig(c)); }
});
test('a renamed production app or diagnostic report cannot pass Dev manifest validation', () => {
  assertDevManifest(manifest());
  for (const change of [m => m.identifier = 'com.osir.client', m => m.releaseDiagnostics.report.identifier = 'com.osir.client',
    m => m.sourceProvenance.git.head = '0'.repeat(40), m => m.sourceProvenance.sourceSha256 = '0'.repeat(64),
    m => m.sourceProvenance.git.dirty = true, m => m.architectures = ['arm64', 'x86_64'],
    m => m.updaterEnabled = true, m => m.releaseDiagnostics.report.releaseBuild = false,
    m => m.distributionSignatureVerified = true]) {
    const m = manifest(); change(m); assert.throws(() => assertDevManifest(m));
  }
});
test('real database rows must select loopback Dev port across all tools', () => {
  const row = { app_type: 'claude', listen_address: '127.0.0.1', listen_port: 15724 };
  assertProxyRows([row]);
  assert.throws(() => assertProxyRows([]));
  assert.throws(() => assertProxyRows([{ ...row, listen_port: 15721 }]));
  assert.throws(() => assertProxyRows([{ ...row, listen_address: '0.0.0.0' }]));
  assert.throws(() => assertProxyRows([row, { ...row, app_type: 'codex', listen_port: 15721 }]));
});
test('workflow keeps independent source/build directories and exact rescue dependency cache settings', () => {
  const workflow = readFileSync(new URL('../.github/workflows/osir-macos-arm-release.yml', import.meta.url), 'utf8');
  for (const required of ['runs-on: macos-15', 'path: control', 'path: source', 'workspaces: source/src-tauri -> target',
    'prefix-key: osir-native-v1', 'key: macos-15-arm64-prod', 'cache-workspace-crates: false',
    'env-vars: CARGO CC CFLAGS CXX CMAKE RUST ImageOS ImageVersion', "CARGO_BUILD_JOBS: '1'"])
    assert.ok(workflow.includes(required));
  assert.doesNotMatch(workflow, /\$\{\{ runner.temp \}\}|git push|workflow run|continue-on-error/);
});
test('genuine Dev is bound to r2 and rejects the superseded r1 package', () => {
  assert.equal(reviewed.sourceCommit, '8bebc7a884b51336590e0b54313f70f3f4364509');
  assert.equal(reviewed.sourceSha256, 'a376ca7fc36ad9546b8095276cd736b282833513f22d7bc5dce1e44d75914017');
  const old = manifest();
  old.sourceProvenance.git.head = 'a5b49ece14c0abee1599fae8e9e5f47e3012d25b';
  assert.throws(() => assertDevManifest(old));
});
