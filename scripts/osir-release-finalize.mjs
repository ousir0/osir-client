#!/usr/bin/env node
// Finalize only previously built CI outputs. Private material remains in the
// native signer/keychain; CI artifacts never contain release private keys.
import {
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { basename, join, resolve } from "node:path";
import { homedir } from "node:os";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { prepareDevSigning } from "./osir-dev-signing.mjs";
import { buildDmg } from "./osir-dmg.mjs";
import {
  inspectArtifact,
  createReleasePlan,
} from "./osir-release-publisher.mjs";
import {
  createUpdaterManifest,
  loadUpdaterReleaseConfig,
  requireUpdaterSigner,
  verifyUpdaterArtifact,
} from "./osir-release-updater.mjs";

const ROOT = fileURLToPath(new URL("../", import.meta.url));
function ensure(value, message) {
  if (!value) throw new Error(message);
}
function run(command, args, env = process.env) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 180000,
  });
  ensure(
    !result.error && result.status === 0,
    `${basename(command)} failed while finalizing; no release is approved.`,
  );
  return `${result.stdout || ""}${result.stderr || ""}`;
}
const digest = (path) => {
  const value = inspectArtifact(path);
  return {
    name: basename(path),
    sha256: value.sha256,
    bytes: value.size_bytes,
  };
};
export function peMachine(path) {
  const data = readFileSync(path);
  ensure(
    data.length >= 64 && data.toString("ascii", 0, 2) === "MZ",
    "Invalid PE artifact.",
  );
  const offset = data.readUInt32LE(0x3c);
  ensure(
    offset >= 64 &&
      offset + 6 <= data.length &&
      data.toString("ascii", offset, offset + 4) === "PE\0\0",
    "Invalid PE header.",
  );
  return data.readUInt16LE(offset + 4);
}
export async function finalize({ input, output, channel, target }) {
  ensure(["beta", "prod"].includes(channel), "Invalid channel.");
  input = resolve(input);
  output = resolve(output);
  ensure(
    lstatSync(input).isDirectory() &&
      !lstatSync(input).isSymbolicLink() &&
      !existsSync(output),
    "Input must be ordinary and output must be new.",
  );
  const windows = target.endsWith("windows-msvc");
  ensure(
    [
      "x86_64-apple-darwin",
      "x86_64-pc-windows-msvc",
      "aarch64-pc-windows-msvc",
    ].includes(target),
    "Unsupported finalization target.",
  );
  const pending = JSON.parse(
    readFileSync(join(input, "build-manifest.json"), "utf8"),
  );
  ensure(
    pending.channel === channel &&
      /^\d+\.\d+\.\d+$/.test(pending.version) &&
      pending.sourceProvenance?.verifiedAtEnd === true,
    "CI source provenance is incomplete.",
  );
  const configInput = loadUpdaterReleaseConfig(
    join(ROOT, "config", `osir-updater-${channel}.json`),
    { channel },
  );
  const config = configInput.config;
  const key = join(
    homedir(),
    ".config/osir-client/release-signing",
    channel,
    "updater.key",
  );
  const signerEnv = {
    ...process.env,
    TAURI_SIGNING_PRIVATE_KEY: key,
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD:
      process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD || "",
  };
  requireUpdaterSigner(signerEnv);
  mkdirSync(output, { recursive: true, mode: 0o700 });
  const stem = `OSIR-Client-${channel}-${pending.version}-${target}`;
  let app, installer, localSigningIdentity, localSigningRequirement;
  if (windows) {
    ensure(
      pending.peArchitectureVerified === true &&
        pending.mode === "ci-pending-local-finalization" &&
        pending.signing === "windows-unsigned-updater-pending" &&
        pending.updaterRuntimeConfigured === true &&
        pending.updaterConfigSha256 === configInput.sha256 &&
        pending.packagedLifecycleAcceptance === "passed-CI-smoke",
      "CI PE architecture was not verified.",
    );
    const executable = join(
      input,
      pending.mainExecutable?.name || "osir_client.exe",
    );
    const expected = target.startsWith("aarch64") ? 0xaa64 : 0x8664;
    ensure(
      peMachine(executable) === expected,
      "Main PE architecture differs from target.",
    );
    ensure(
      digest(executable).sha256 === pending.mainExecutableArtifact?.sha256,
      "Main executable differs from the CI manifest.",
    );
    const smoke = join(
      input,
      pending.packagedLifecycleReport?.name ?? "windows-packaged-smoke.json",
    );
    ensure(
      digest(smoke).sha256 === pending.packagedLifecycleReport?.sha256,
      "Windows lifecycle report differs from its CI manifest.",
    );
    const setup = join(input, pending.installerArtifact.name);
    ensure(
      [0x14c, 0x8664, 0xaa64].includes(peMachine(setup)),
      "NSIS installer is not a supported PE.",
    );
    ensure(
      digest(setup).sha256 === pending.installerArtifact.sha256,
      "CI installer checksum differs.",
    );
    installer = join(output, stem + "-setup.exe");
    cpSync(setup, installer, { errorOnExist: true, force: false });
  } else {
    const apps = readdirSync(input).filter((name) => name.endsWith(".app"));
    ensure(
      apps.length === 1 &&
        pending.mode === "ci-pending-local-finalization" &&
        pending.signing === "ad-hoc-pending-local" &&
        pending.updaterRuntimeConfigured === true &&
        pending.embeddedUpdaterPublicConfigVerified === true &&
        pending.updaterConfigSha256 === configInput.sha256,
      "Expected one verified CI app.",
    );
    app = join(output, apps[0]);
    cpSync(join(input, apps[0]), app, {
      recursive: true,
      preserveTimestamps: true,
      errorOnExist: true,
      force: false,
    });
    const signing = prepareDevSigning();
    ensure(
      signing.identity !== "-",
      "Stable local signing certificate required.",
    );
    run("/usr/bin/codesign", [
      "--force",
      "--deep",
      "--options",
      "runtime",
      "--timestamp=none",
      ...(signing.keychain ? ["--keychain", signing.keychain] : []),
      "--sign",
      signing.identity,
      app,
    ]);
    const requirement = `identifier "${pending.identifier}" and certificate leaf = H"${signing.identity}"`;
    run("/usr/bin/codesign", [
      "--verify",
      "--deep",
      "--strict",
      "-R=" + requirement,
      app,
    ]);
    localSigningIdentity = signing.identity;
    localSigningRequirement = run("/usr/bin/codesign", ["-dr", "-", app])
      .split(/\r?\n/)
      .find((line) => line.includes("designated =>"))
      ?.split("=>")
      .slice(1)
      .join("=>")
      .trim();
    ensure(
      localSigningRequirement && !localSigningRequirement.includes("cdhash"),
      "Unstable code identity.",
    );
    const binary = join(app, "Contents/MacOS/osir_client");
    ensure(
      run("/usr/bin/lipo", ["-archs", binary]).trim() === "x86_64",
      "Intel app architecture mismatch.",
    );
  }
  const updater = windows ? installer : join(output, stem + ".app.tar.gz");
  if (!windows)
    run("/usr/bin/tar", ["-czf", updater, "-C", output, basename(app)]);
  run(
    join(ROOT, "node_modules/.bin/tauri"),
    ["signer", "sign", updater],
    signerEnv,
  );
  const signature = readFileSync(updater + ".sig", "utf8").trim();
  const verified = verifyUpdaterArtifact(updater, signature, config.pubkey);
  const metadata = {
    ...pending,
    channel,
    target,
    mode: windows
      ? "release-windows-distribution"
      : "release-self-signed-distribution",
    signing: windows
      ? "windows-unsigned-updater-signed"
      : "local-certificate-not-notarized",
    distributionSignatureVerified: false,
    codeSignatureVerified: !windows,
    notarization: windows ? "not-applicable" : "not-notarized",
    updaterEnabled: true,
    updaterConfigured: true,
    updaterRuntimeConfigured: true,
    updaterUpgradeAcceptance: "not-run",
    updaterConfigSha256: configInput.sha256,
    updaterArtifact: verified,
    ...(windows
      ? { peArchitectureVerified: true, installerArtifact: digest(installer) }
      : { localSigningIdentity, localSigningRequirement }),
  };
  if (!windows)
    Object.assign(
      metadata,
      await buildDmg({ buildDirectory: output, manifest: metadata, target }),
    );
  const feed = createUpdaterManifest({
    config,
    version: pending.version,
    target,
    artifactName: basename(updater),
    signature,
    pubDate: new Date().toISOString(),
  });
  writeFileSync(
    join(output, `updater-${channel}.json`),
    JSON.stringify(feed, null, 2) + "\n",
  );
  writeFileSync(
    join(output, "build-manifest.json"),
    JSON.stringify(metadata, null, 2) + "\n",
  );
  const plan = createReleasePlan({
    channel,
    buildDirectory: output,
    updaterConfigPath: join(ROOT, "config", `osir-updater-${channel}.json`),
  });
  return {
    directory: output,
    platforms: [...new Set(plan.artifacts.map((a) => a.platform))],
    source_sha: plan.source_sha,
  };
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const [channel, target, input, output, ...extra] = process.argv.slice(2);
  try {
    ensure(
      !extra.length && output,
      "Usage: osir-release-finalize.mjs <prod|beta> <target> <input> <new-output>",
    );
    console.log(
      JSON.stringify(
        await finalize({ channel, target, input, output }),
        null,
        2,
      ),
    );
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
