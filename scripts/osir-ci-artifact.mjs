// CI metadata validation only. No signing keys, deployment or release API calls.
import assert from "node:assert/strict";
import { appendFileSync, readFileSync, readdirSync, lstatSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { basename, join, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { sourceSnapshot, gitProvenance } from "./osir-build.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
function json(path) { return JSON.parse(readFileSync(path, "utf8")); }
function output(name, value) {
  assert.ok(!/[\r\n]/.test(value), "Multiline CI output rejected");
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `${name}=${value}\n`);
  console.log(`${name}=${value}`);
}
function digest(path) {
  const stat = lstatSync(path);
  assert.ok(stat.isFile() && !stat.isSymbolicLink() && stat.size > 0);
  return { name: basename(path), bytes: stat.size, sha256: createHash("sha256").update(readFileSync(path)).digest("hex") };
}
function expectedSource() {
  assert.match(process.env.OSIR_SOURCE_SHA256 || "", /^[a-f0-9]{64}$/, "A concrete source fingerprint is required");
  const snapshot = sourceSnapshot();
  const git = gitProvenance();
  assert.equal(snapshot.provenance.sourceSha256, process.env.OSIR_SOURCE_SHA256, "CI checkout differs from the requested source");
  assert.equal(git.dirty, false, "CI source checkout must be clean");
  assert.equal(git.head, process.env.GITHUB_SHA, "CI checkout differs from the dispatched commit");
  return { ...snapshot.provenance, git };
}
function validateProvenance(manifest) {
  const source = expectedSource();
  assert.equal(manifest.sourceProvenance.verifiedAtEnd, true);
  assert.equal(manifest.sourceProvenance.sourceSha256, source.sourceSha256);
  assert.equal(manifest.sourceProvenance.inputsManifestSha256, source.inputsManifestSha256);
  assert.equal(manifest.sourceProvenance.git.head, source.git.head);
  assert.equal(manifest.sourceProvenance.git.dirtyAtStart, false);
  assert.equal(manifest.sourceProvenance.git.dirty, false);
}
function artifactDirectory(path) {
  const directory = resolve(path);
  assert.ok(directory.startsWith(resolve(root, "release/osir") + sep), "Artifact must be inside the generated release tree");
  const stat = lstatSync(directory);
  assert.ok(stat.isDirectory() && !stat.isSymbolicLink());
  return directory;
}

export function acceptWindowsSmoke(directory) {
  directory = artifactDirectory(directory);
  const path = join(directory, "build-manifest.json");
  const manifest = json(path);
  validateProvenance(manifest);
  assert.equal(manifest.mode, "ci-pending-local-finalization");
  assert.equal(manifest.target, "x86_64-pc-windows-msvc");
  assert.deepEqual(manifest.architectures, ["x86_64"]);
  assert.equal(manifest.signing, "windows-unsigned-updater-pending");
  assert.equal(manifest.codeSignatureVerified, false);
  assert.equal(manifest.distributionSignatureVerified, false);
  for (const key of ["installerArtifact", "mainExecutableArtifact"])
    assert.deepEqual(digest(join(directory, manifest[key].name)), manifest[key], `Changed ${key}`);
  const reportPath = join(directory, "windows-packaged-smoke.json");
  const report = json(reportPath);
  assert.equal(report.passed, true, "Packaged lifecycle did not pass");
  assert.equal(report.target, manifest.target);
  assert.equal(report.version, manifest.version);
  assert.equal(report.sourceSha256, manifest.sourceProvenance.sourceSha256);
  assert.equal(report.gitHead, manifest.sourceProvenance.git.head);
  assert.equal(report.installerSha256, manifest.installerArtifact.sha256);
  assert.equal(report.installerAuthenticodeStatus, "NotSigned");
  for (const stage of ["install", "launch", "upgrade", "uninstall"])
    assert.equal(report.stages[stage].passed, true, `Incomplete ${stage}`);
  for (const stage of ["install", "upgrade"]) {
    assert.equal(report.stages[stage].sha256, manifest.mainExecutableArtifact.sha256);
    assert.equal(report.stages[stage].architecture, "x86_64");
    assert.equal(report.stages[stage].authenticodeStatus, "NotSigned");
  }
  assert.equal(report.stages.launch.windowObserved, true);
  assert.equal(report.stages.launch.remainedRunning, true);
  assert.equal(report.stages.upgrade.kind, "same-version-reinstall-update-path");
  assert.equal(report.stages.uninstall.mainExecutableRemoved, true);
  manifest.packagedLifecycleAcceptance = "passed-CI-smoke";
  manifest.packagedLifecycleReport = digest(reportPath);
  writeFileSync(path, JSON.stringify(manifest, null, 2) + "\n");
  const files = [manifest.installerArtifact, manifest.mainExecutableArtifact, manifest.packagedLifecycleReport];
  writeFileSync(join(directory, "SHA256SUMS"), files.map((file) => `${file.sha256}  ${file.name}`).join("\n") + "\n");
}

function main() {
  const [command, channel, target] = process.argv.slice(2);
  assert.ok(["prod", "beta"].includes(channel));
  assert.ok(["x86_64-pc-windows-msvc", "x86_64-apple-darwin"].includes(target));
  if (command === "preflight") {
    const source = expectedSource();
    const pkg = json(join(root, "package.json"));
    assert.equal(process.versions.node, readFileSync(join(root, ".node-version"), "utf8").trim());
    assert.equal(process.arch, "x64", "Native x64 runner required");
    assert.equal(process.platform, target.includes("windows") ? "win32" : "darwin");
    const rust = readFileSync(join(root, "rust-toolchain.toml"), "utf8").match(/^channel\s*=\s*"(\d+\.\d+(?:\.\d+)?)"/m)?.[1];
    assert.ok(rust, "Repository must pin Rust");
    output("version", pkg.version);
    output("rust", rust);
    output("sourceSha256", source.sourceSha256);
    if (process.env.GITHUB_ENV) appendFileSync(process.env.GITHUB_ENV, `RUSTUP_TOOLCHAIN=${rust}\n`);
    return;
  }
  if (command === "windows-output" || command === "accept-windows") {
    const directory = artifactDirectory(json(join(root, "release/osir/windows-latest-output.json")).directory);
    const manifest = json(join(directory, "build-manifest.json"));
    assert.equal(manifest.channel, channel);
    assert.equal(manifest.target, target);
    validateProvenance(manifest);
    if (command === "accept-windows") acceptWindowsSmoke(directory);
    output("directory", directory);
    return;
  }
  assert.equal(command, "mac-output", "Unknown CI metadata command");
  assert.equal(target, "x86_64-apple-darwin");
  const parent = join(root, "release/osir", channel, "pending-macos", target);
  const outputs = readdirSync(parent, { withFileTypes: true }).filter((entry) => entry.isDirectory());
  assert.equal(outputs.length, 1, "Expected one output on a fresh CI runner");
  const directory = artifactDirectory(join(parent, outputs[0].name));
  const manifest = json(join(directory, "build-manifest.json"));
  validateProvenance(manifest);
  assert.equal(manifest.channel, channel);
  assert.equal(manifest.mode, "ci-pending-local-finalization");
  assert.equal(manifest.signing, "ad-hoc-pending-local");
  assert.equal(manifest.distributionSignatureVerified, false);
  assert.equal(manifest.updaterEnabled, true);
  assert.equal(manifest.updaterRuntimeConfigured, true);
  assert.equal(manifest.embeddedUpdaterPublicConfigVerified, true);
  assert.equal(manifest.updaterSignature, "pending-local");
  assert.deepEqual(manifest.architectures, ["x86_64"]);
  const apps = readdirSync(directory).filter((name) => name.endsWith(".app"));
  assert.equal(apps.length, 1);
  output("directory", directory);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try { main(); }
  catch (error) { console.error("OSIR CI artifacts: " + error.message); process.exitCode = 1; }
}
