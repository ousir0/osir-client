#!/usr/bin/env node
// Build the real release runtime with public updater configuration, without a
// private signer. CI output is pending local signing and is never distributable.
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { checkProject, configuredChannel, gitProvenance, infoPlist, sourceSnapshot, verifyApp, verifyRenderer, verifySourceSnapshot } from "./osir-build.mjs";
import { loadUpdaterReleaseConfig, packagingEnvironments, verifyUpdaterConfigUnchanged } from "./osir-release-updater.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
function requireThat(condition, message) { if (!condition) throw new Error(message); }
function json(path) { return JSON.parse(readFileSync(path, "utf8")); }
function run(command, args, env = process.env) {
  const result = spawnSync(command, args, { cwd: root, env, stdio: "inherit" });
  requireThat(!result.error && result.status === 0, command + " failed");
}

export function macosCiConfig(channel, updater) {
  const config = configuredChannel(channel, updater);
  // Inject public runtime configuration while deferring artifact signing. The
  // osir-updates marker remains true because the final distribution has updates.
  config.bundle.createUpdaterArtifacts = false;
  config.bundle.macOS.signingIdentity = "-";
  return config;
}

async function main() {
  const [channel, ...options] = process.argv.slice(2);
  requireThat(["prod", "beta"].includes(channel) && options.every((arg) => arg === "--check"), "Usage: node scripts/osir-build-macos-ci.mjs <prod|beta> [--check]");
  const target = "x86_64-apple-darwin";
  const version = checkProject();
  const configPath = join(root, `config/osir-updater-${channel}.json`);
  const publicInput = loadUpdaterReleaseConfig(configPath, { channel, upstreamPublicKey: json(join(root, "src-tauri/tauri.conf.json")).plugins.updater.pubkey });
  const config = macosCiConfig(channel, publicInput.config);
  const source = sourceSnapshot();
  const git = gitProvenance();
  requireThat(!process.env.OSIR_SOURCE_SHA256 || source.provenance.sourceSha256 === process.env.OSIR_SOURCE_SHA256, "Source differs from requested snapshot");
  if (options.includes("--check")) {
    console.log(JSON.stringify({ mode: "ci-pending-local-finalization", channel, target, version,
      sourceSha256: source.provenance.sourceSha256, identifier: config.identifier,
      signing: "ad-hoc-pending-local", updaterRuntimeConfigured: true, updaterSignature: "pending-local" }, null, 2));
    return;
  }
  requireThat(process.platform === "darwin" && process.arch === "x64" && process.env.GITHUB_ACTIONS === "true" && process.env.CI === "true", "Only native Intel macOS GitHub Actions runner is supported");
  const cache = resolve(process.env.CARGO_TARGET_DIR || join(root, "src-tauri/target"));
  mkdirSync(cache, { recursive: true });
  const lock = join(cache, ".osir-build-lock");
  mkdirSync(lock); // Never compete with another native build using the same cache.
  let staging;
  let output;
  let complete = false;
  try {
    writeFileSync(join(lock, "pid"), String(process.pid));
    staging = mkdtempSync(join(cache, ".osir-ci-packaging-"));
    const plist = join(staging, "osir-channel-info.plist");
    writeFileSync(plist, infoPlist(config));
    config.bundle.macOS.infoPlist = plist;
    const environment = packagingEnvironments({ ...process.env, CARGO_TARGET_DIR: cache, CARGO_BUILD_JOBS: "1", CARGO_INCREMENTAL: "0" }, { release: false, updater: false });
    run(process.execPath, [join(root, "node_modules/vite/bin/vite.js"), "build"], environment.renderer);
    verifyRenderer();
    verifySourceSnapshot(source, git);
    const generatedApp = join(cache, target, "release/bundle/macos", config.productName + ".app");
    if (existsSync(generatedApp)) rmSync(generatedApp, { recursive: true });
    run(process.execPath, [join(root, "node_modules/@tauri-apps/cli/tauri.js"), "build", "--ci", "--no-sign", "--config", JSON.stringify(config), "--target", target, "--bundles", "app", "--", "--locked", "-j", "1"], environment.native);
    const parent = join(root, "release/osir", channel, "pending-macos", target);
    mkdirSync(parent, { recursive: true });
    output = mkdtempSync(join(parent, version + "-"));
    const app = join(output, config.productName + ".app");
    cpSync(generatedApp, app, { recursive: true, preserveTimestamps: true, errorOnExist: true, force: false });
    run("/usr/bin/codesign", ["--force", "--deep", "--sign", "-", "--timestamp=none", app]);
    const architectures = verifyApp(app, config, target, false);
    requireThat(JSON.stringify(architectures) === JSON.stringify(["x86_64"]), "Only Intel architecture is permitted");
    const binary = readFileSync(join(app, "Contents/MacOS/osir_client"));
    for (const value of [publicInput.config.pubkey, publicInput.config.endpoint, "osir-updates", "bundleArtifacts"])
      requireThat(binary.includes(Buffer.from(value)), "Compiled updater public configuration could not be verified");
    verifyUpdaterConfigUnchanged(configPath, publicInput.sha256);
    const sourceProvenance = verifySourceSnapshot(source, git);
    writeFileSync(join(output, "build-manifest.json"), JSON.stringify({ schemaVersion: 1, channel, version, target,
      mode: "ci-pending-local-finalization", identifier: config.identifier, scheme: config.plugins["deep-link"].desktop.schemes[0],
      architectures, renderer: "embedded", signing: "ad-hoc-pending-local", distributionSignatureVerified: false,
      codeSignatureVerified: true, notarization: "not-notarized", updaterConfigured: true,
      updaterRuntimeConfigured: true, updaterEnabled: true, updaterSignature: "pending-local",
      updaterConfigSha256: publicInput.sha256, embeddedUpdaterPublicConfigVerified: true,
      sourceProvenance, builtAt: new Date().toISOString() }, null, 2) + "\n");
    complete = true;
    console.log("Intel application pending local final signing: " + output);
  } finally {
    if (!complete && output) rmSync(output, { recursive: true, force: true });
    if (staging) rmSync(staging, { recursive: true, force: true });
    rmSync(lock, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch((error) => { console.error("OSIR Intel CI packaging: " + error.message); process.exitCode = 1; });
