// Acceptance-only optimized Dev build. Product sources are never patched.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { appendFileSync, closeSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createServer } from 'node:net';
import { inspectSource, reviewed } from './osir-arm-rescue.mjs';

export const identity = Object.freeze({ identifier: 'com.osir.client.dev', name: 'OSIR Client Dev', scheme: 'osir-client-dev', port: 15724 });
export function assertDevConfig(config) {
  assert.equal(config.identifier, identity.identifier);
  assert.equal(config.productName, identity.name);
  assert.deepEqual(config.plugins['deep-link'].desktop.schemes, [identity.scheme]);
  assert.deepEqual(config.plugins.updater.endpoints, []);
  assert.equal(config.plugins.updater.pubkey, '');
  assert.equal(config.plugins['osir-updates'], undefined);
  assert.equal(config.bundle.createUpdaterArtifacts, false);
  assert.deepEqual(config.bundle.targets, ['app']);
  assert.equal(config.build.devUrl, null);
  assert.equal(config.build.frontendDist, '../dist');
  assert.ok(config.bundle.icon.every((name) => name.startsWith('icons/osir/dev/')));
}
export function nativeArguments(config) {
  assertDevConfig(config);
  return ['build', '--ci', '--no-sign', '--config', JSON.stringify(config), '--target', reviewed.target,
    '--bundles', 'app', '--', '--locked', '--timings', '-j', '1'];
}
export function assertDevManifest(manifest) {
  assert.equal(manifest.mode, 'acceptance-only-genuine-dev');
  assert.equal(manifest.channel, 'dev');
  assert.equal(manifest.identifier, identity.identifier);
  assert.equal(manifest.target, reviewed.target);
  assert.deepEqual(manifest.architectures, ['arm64']);
  assert.equal(manifest.version, '0.1.10');
  assert.equal(manifest.updaterEnabled, false);
  assert.equal(manifest.distributionSignatureVerified, false);
  assert.equal(manifest.sourceProvenance.git.head, reviewed.sourceCommit);
  assert.equal(manifest.sourceProvenance.git.dirtyAtStart, false);
  assert.equal(manifest.sourceProvenance.git.dirty, false);
  assert.equal(manifest.sourceProvenance.sourceSha256, reviewed.sourceSha256);
  assert.equal(manifest.sourceProvenance.inputsManifestSha256, reviewed.inputsManifestSha256);
  assert.equal(manifest.sourceProvenance.verifiedAtEnd, true);
  assert.equal(manifest.releaseDiagnostics.report.identifier, identity.identifier);
  assert.equal(manifest.releaseDiagnostics.report.releaseBuild, true);
  assert.equal(manifest.releaseDiagnostics.report.updateDistribution.updatesEnabled, false);
}
export function assertProxyRows(rows) {
  assert.ok(rows.length > 0, 'Missing real proxy configuration rows');
  assert.ok(rows.some((row) => row.app_type === 'claude'));
  for (const row of rows) {
    assert.equal(row.listen_address, '127.0.0.1');
    assert.equal(row.listen_port, identity.port);
  }
}
const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
const json = (path) => JSON.parse(readFileSync(path, 'utf8'));
const write = (path, data) => writeFileSync(path, JSON.stringify(data, null, 2) + '\n');
function run(command, args, { cwd, env = process.env, timeout = 15000, inherit = false } = {}) {
  const result = spawnSync(command, args, { cwd, env, timeout, encoding: 'utf8', stdio: inherit ? 'inherit' : ['ignore', 'pipe', 'pipe'] });
  assert.equal(result.status, 0, `${command} failed${result.stderr ? ': ' + result.stderr : ''}`);
  return result.stdout?.trim();
}
function output(name, value) {
  assert.ok(!/[\r\n]/.test(value));
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `${name}=${value}\n`);
}
export async function requireFreePort() {
  const socket = createServer();
  await new Promise((done, reject) => { socket.once('error', reject); socket.listen(identity.port, '127.0.0.1', done); });
  await new Promise((done, reject) => socket.close((error) => error ? reject(error) : done()));
}
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
async function smoke({ source, app, evidence, manifest }) {
  const binary = join(app, 'Contents/MacOS/osir_client');
  const temporary = mkdtempSync(join(process.env.RUNNER_TEMP, 'osir-genuine-dev-'));
  const isolated = join(temporary, 'isolated-home');
  mkdirSync(isolated, { mode: 0o700 });
  const helper = join(temporary, 'window-probe');
  run('/usr/bin/xcrun', ['swiftc', join(source, 'scripts/osir-macos-window-probe.swift'), '-o', helper], { timeout: 60000 });
  const protectedPaths = ['.osir_client', '.cc-switch', '.codex', '.claude', '.gemini', '.config/opencode',
    'Library/Application Support/com.osir.client', 'Library/Application Support/com.osir.client.beta'].map((name) => join(homedir(), name));
  const sockets = ['/tmp/com_osir_client_si.sock', '/private/tmp/com_osir_client_si.sock'];
  const rules = (type, paths) => paths.map((path) => `(${type} ${JSON.stringify(path)})`).join(' ');
  const policy = `(version 1)(allow default)(deny file-read* ${rules('subpath', protectedPaths)})(deny file-write* ${rules('subpath', protectedPaths)} ${rules('literal', sockets)})(deny network-outbound ${rules('literal', sockets)})`;
  const env = Object.fromEntries(['PATH', 'LANG', 'LC_ALL', 'TMPDIR', 'USER', 'LOGNAME', 'SHELL', '__CF_USER_TEXT_ENCODING']
    .filter((name) => process.env[name]).map((name) => [name, process.env[name]]));
  // The genuine Dev runtime isolates its own process home from this absolute
  // override. The harness never rewrites parent HOME/USERPROFILE/CODEX_HOME.
  env.CC_SWITCH_TEST_HOME = isolated;
  const report = { schemaVersion: 1, passed: false, scope: 'dev-native-window-home-proxy-config-only',
    sourceCommit: reviewed.sourceCommit, sourceSha256: reviewed.sourceSha256,
    executableSha256: hash(binary), identifier: identity.identifier, hostArchitecture: process.arch,
    isolatedHome: isolated, configuredProxyPort: null, proxyListenerAcceptance: 'not-run',
    bindingE2E: 'not-run', rendererNativeIpc: 'not-run', stages: [] };
  try {
    for (const phase of ['launch', 'reopen']) {
      await requireFreePort();
      const fd = openSync(join(evidence, `dev-${phase}.log`), 'wx', 0o600);
      const child = spawn('/usr/bin/sandbox-exec', ['-p', policy, binary], { cwd: dirname(app), env, stdio: ['ignore', fd, fd] });
      closeSync(fd);
      let spawnError;
      child.once('error', (error) => { spawnError = error; });
      const exited = new Promise((done) => { child.once('exit', done); child.once('error', done); });
      const stage = { phase, pid: child.pid, passed: false };
      report.stages.push(stage);
      try {
        for (let attempt = 0; attempt < 80; attempt++) {
          assert.equal(spawnError, undefined);
          assert.equal(child.exitCode, null, 'Dev app exited before window observation');
          assert.equal(child.signalCode, null);
          const current = JSON.parse(run(helper, [String(child.pid), identity.identifier]));
          if (current.windows.length > 0) { stage.observation = current; break; }
          await pause(500);
        }
        assert.ok(stage.observation, 'Native Dev window was not observed');
        assert.equal(stage.observation.bundleIdentifier, identity.identifier);
        assert.equal(stage.observation.bundlePath, realpathSync(app));
        assert.deepEqual(stage.observation.otherInstancePids, []);
        await pause(3000);
        assert.equal(child.exitCode, null);
        assert.equal(child.signalCode, null);
        const database = join(isolated, '.osir_client/osir_client.db');
        assert.ok(existsSync(database), 'Dev did not initialize isolated database');
        const rows = JSON.parse(run('/usr/bin/sqlite3', ['-readonly', '-json', database,
          'SELECT app_type, listen_address, listen_port FROM proxy_config ORDER BY app_type;']));
        assertProxyRows(rows);
        stage.proxyRows = rows;
        stage.isolatedDatabase = database;
        stage.passed = true;
        report.configuredProxyPort = identity.port;
      } finally {
        if (child.pid && child.exitCode === null && child.signalCode === null && !spawnError) {
          assert.equal(run('/bin/ps', ['-p', String(child.pid), '-o', 'comm=']), binary);
          child.kill('SIGTERM');
          if (!await Promise.race([exited.then(() => true), pause(10000).then(() => false)])) {
            assert.equal(run('/bin/ps', ['-p', String(child.pid), '-o', 'comm=']), binary);
            child.kill('SIGKILL'); await exited;
          }
          stage.stoppedOnlyOwnedPid = true;
        }
      }
    }
    assert.equal(hash(binary), manifest.releaseDiagnostics.executableSha256);
    run('/usr/bin/codesign', ['--verify', '--deep', '--strict', app]);
    report.passed = true;
  } finally { write(join(evidence, 'dev-smoke.json'), report); }
  return report;
}
async function main() {
  const [command] = process.argv.slice(2);
  const source = resolve(process.env.OSIR_DEV_SOURCE || 'source');
  const evidence = resolve(process.env.OSIR_DEV_EVIDENCE || 'dev-evidence');
  const sourceIdentity = await inspectSource(source);
  const builder = await import(pathToFileURL(join(source, 'scripts/osir-build.mjs')).href);
  const config = builder.configuredChannel('dev', null);
  assertDevConfig(config);
  if (command === 'inspect') {
    console.log(JSON.stringify({ sourceIdentity, selection: 'frozen-native-dev-config', profile: 'release',
      configurationOverlay: config, productSourceOverlay: [] }, null, 2));
    return;
  }
  assert.equal(process.env.CI, 'true');
  assert.equal(process.env.GITHUB_ACTIONS, 'true');
  assert.equal(process.env.GITHUB_REPOSITORY, reviewed.repository);
  assert.equal(process.env.OSIR_CHANNEL, 'dev');
  assert.equal(process.platform, 'darwin'); assert.equal(process.arch, 'arm64');
  assert.equal(process.env.OSIR_DEV_SOURCE_COMMIT, reviewed.sourceCommit);
  assert.equal(process.env.OSIR_SOURCE_SHA256, reviewed.sourceSha256);
  assert.equal(process.env.CARGO_BUILD_JOBS, '1');
  assert.equal(process.versions.node, readFileSync(join(source, '.node-version'), 'utf8').trim());
  const receiptPath = join(evidence, 'dev-producer-receipt.json');
  if (command === 'init') {
    assert.ok(!existsSync(evidence)); mkdirSync(evidence);
    const control = resolve(dirname(fileURLToPath(import.meta.url)), '..');
    assert.equal(run('git', ['rev-parse', 'HEAD'], { cwd: control }), process.env.GITHUB_SHA);
    assert.equal(run('git', ['status', '--porcelain=v1'], { cwd: control }), '');
    assert.match(process.env.GITHUB_WORKFLOW_SHA || '', /^[a-f0-9]{40}$/);
    const rust = readFileSync(join(source, 'rust-toolchain.toml'), 'utf8').match(/^channel\s*=\s*"([\d.]+)"/m)?.[1];
    assert.ok(rust);
    appendFileSync(process.env.GITHUB_ENV, `RUSTUP_TOOLCHAIN=${rust}\n`);
    const selectionPath = join(evidence, 'dev-config-selection.json');
    write(selectionPath, config);
    write(receiptPath, { schemaVersion: 1, purpose: 'genuine-dev-binding-e2e-preparation',
      producerWorkflowSha: process.env.GITHUB_WORKFLOW_SHA, dispatchHead: process.env.GITHUB_SHA,
      runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT,
      sourceCommit: reviewed.sourceCommit, sourceSha256: reviewed.sourceSha256,
      inputsManifestSha256: reviewed.inputsManifestSha256, productSourceOverlay: [],
      configurationOverlay: { channel: 'dev', selectedFrozenFile: 'src-tauri/tauri.osir-dev.conf.json',
        selectedFileSha256: hash(join(source, 'src-tauri/tauri.osir-dev.conf.json')), selectionSha256: hash(selectionPath),
        beforeIdentifier: builder.configuredChannel('prod', null).identifier, afterIdentifier: config.identifier,
        cargoProfile: 'release', debugAssertions: false, updater: 'disabled',
        temporaryInfoPlist: 'Dev scheme only', adHocSignature: true },
      harnessSha256: hash(fileURLToPath(import.meta.url)),
      workflowFileSha256: hash(join(control, '.github/workflows/osir-macos-arm-release.yml')),
      acceptance: 'not-run', publication: 'not-attempted-by-this-harness',
    });
    return;
  }
  const receipt = json(receiptPath);
  assert.equal(receipt.dispatchHead, process.env.GITHUB_SHA);
  assert.equal(receipt.producerWorkflowSha, process.env.GITHUB_WORKFLOW_SHA);
  const payload = join(evidence, 'payload');
  const app = join(payload, identity.name + '.app');
  if (command === 'build') {
    assert.ok(!existsSync(payload));
    const target = resolve(source, 'src-tauri/target');
    assert.equal(resolve(process.env.CARGO_TARGET_DIR), target);
    mkdirSync(target, { recursive: true });
    const lock = join(target, '.osir-build-lock'); mkdirSync(lock);
    try {
      writeFileSync(join(lock, 'pid'), String(process.pid));
      const initial = builder.sourceSnapshot(); const git = builder.gitProvenance();
      const { packagingEnvironments } = await import(pathToFileURL(join(source, 'scripts/osir-release-updater.mjs')).href);
      const environments = packagingEnvironments(process.env, { release: false, updater: false });
      run(process.execPath, [join(source, 'node_modules/vite/bin/vite.js'), 'build'], { cwd: source, env: environments.renderer, timeout: 600000, inherit: true });
      builder.verifyRenderer(); builder.verifySourceSnapshot(initial, git);
      const plist = join(evidence, 'dev-channel-info.plist');
      writeFileSync(plist, builder.infoPlist(config));
      config.bundle.macOS.infoPlist = plist; config.bundle.macOS.signingIdentity = '-';
      write(join(evidence, 'effective-dev-config.json'), config);
      receipt.effectiveConfigSha256 = hash(join(evidence, 'effective-dev-config.json'));
      write(receiptPath, receipt);
      // Fresh runner + rust-cache excludes workspace products. Never copy an old
      // production app or binary. Refuse a stale Dev bundle rather than reuse it.
      const generated = join(target, reviewed.target, 'release/bundle/macos', identity.name + '.app');
      assert.ok(!existsSync(generated), 'Unexpected existing Dev bundle');
      run(process.execPath, [join(source, 'node_modules/@tauri-apps/cli/tauri.js'), ...nativeArguments(config)],
        { cwd: source, env: environments.native, timeout: 5400000, inherit: true });
      mkdirSync(payload); cpSync(generated, app, { recursive: true, preserveTimestamps: true, force: false, errorOnExist: true });
      run('/usr/bin/codesign', ['--force', '--deep', '--sign', '-', '--timestamp=none', app]);
      const architectures = builder.verifyApp(app, config, reviewed.target, false);
      assert.deepEqual(architectures, ['arm64']);
      const { verifyReleaseDiagnostics } = await import(pathToFileURL(join(source, 'scripts/osir-release-diagnostics.mjs')).href);
      const diagnostics = verifyReleaseDiagnostics(join(app, 'Contents/MacOS/osir_client'), { config, updatesEnabled: false, bundleType: 'app', releaseBuild: true });
      const manifest = { schemaVersion: 1, mode: 'acceptance-only-genuine-dev', channel: 'dev', identifier: identity.identifier,
        version: config.version, target: reviewed.target, architectures, cargoProfile: 'release', renderer: 'embedded',
        updaterEnabled: false, distributionSignatureVerified: false, signing: 'ad-hoc-not-notarized',
        sourceProvenance: builder.verifySourceSnapshot(initial, git), releaseDiagnostics: diagnostics };
      assertDevManifest(manifest); await inspectSource(source);
      write(join(payload, 'dev-build-manifest.json'), manifest);
      receipt.build = 'verified-compiled-dev'; write(receiptPath, receipt);
      output('directory', payload);
    } finally { rmSync(lock, { recursive: true, force: true }); }
    return;
  }
  assert.equal(command, 'smoke');
  const manifest = json(join(payload, 'dev-build-manifest.json')); assertDevManifest(manifest);
  assert.equal(run('/usr/bin/lipo', ['-archs', join(app, 'Contents/MacOS/osir_client')]), 'arm64');
  const report = await smoke({ source, app, evidence, manifest });
  await inspectSource(source);
  assert.equal(report.passed, true);
  receipt.acceptance = report.scope; receipt.smokeSha256 = hash(join(evidence, 'dev-smoke.json'));
  receipt.bindingE2E = 'not-run'; write(receiptPath, receipt);
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch((error) => { console.error(error); process.exitCode = 1; });
