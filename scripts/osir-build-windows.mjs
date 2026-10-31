#!/usr/bin/env node
// CI produces unsigned NSIS payloads. Independent updater signing is local only.
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { basename, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import {
  sourceSnapshot,
  verifySourceSnapshot,
  gitProvenance,
} from "./osir-build.mjs";
import {
  loadUpdaterReleaseConfig,
  packagingEnvironments,
  updaterConfigPatch,
  verifyUpdaterConfigUnchanged,
} from "./osir-release-updater.mjs";
import { inspectPe } from "./osir-pe-verify.mjs";
import { nsisPayload, verifyNsisPayload } from "./osir-nsis-payload.mjs";
import { verifyReleaseDiagnostics } from "./osir-release-diagnostics.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const native = join(root, "src-tauri");
function requireThat(value, message) {
  if (!value) throw new Error(message);
}
function json(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}
function merge(a, b) {
  const out = { ...a };
  for (const [key, value] of Object.entries(b))
    out[key] =
      value &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      a?.[key] &&
      typeof a[key] === "object"
        ? merge(a[key], value)
        : value;
  return out;
}
function run(args, env, label) {
  console.log(label);
  const result = spawnSync(process.execPath, args, {
    cwd: root,
    env,
    stdio: "inherit",
    windowsHide: true,
  });
  requireThat(!result.error && result.status === 0, label + " failed");
}
function digest(path) {
  const content = readFileSync(path);
  return {
    name: basename(path),
    bytes: content.length,
    sha256: createHash("sha256").update(content).digest("hex"),
  };
}

export function windowsConfig(channel, updater) {
  const filename =
    channel === "prod" ? "tauri.osir.conf.json" : "tauri.osir-beta.conf.json";
  let config = merge(
    merge(
      json(join(native, "tauri.conf.json")),
      json(join(native, "tauri.windows.conf.json")),
    ),
    json(join(native, filename)),
  );
  requireThat(
    config.identifier ===
      (channel === "prod" ? "com.osir.client" : "com.osir.client.beta"),
    "OSIR identity mismatch",
  );
  config = merge(config, updaterConfigPatch(updater));
  config.build = {
    frontendDist: "../dist",
    devUrl: null,
    beforeDevCommand: null,
    beforeBuildCommand: null,
    beforeBundleCommand: null,
  };
  config.app.windows = [
    {
      ...config.app.windows[0],
      title: config.productName,
      width: 1100,
      height: 720,
      minWidth: 900,
      minHeight: 600,
    },
  ];
  config.bundle = merge(config.bundle, {
    active: true,
    targets: ["nsis"],
    createUpdaterArtifacts: false,
    windows: {
      wix: null,
      certificateThumbprint: null,
      signCommand: null,
      nsis: {
        installMode: "currentUser",
        languages: ["English", "SimpChinese"],
        displayLanguageSelector: false,
        startMenuFolder: config.productName,
      },
      // Keep the app payload self-contained, but fetch the shared runtime only
      // on machines that need it instead of embedding the full redistributable.
      webviewInstallMode: { type: "embedBootstrapper", silent: true },
    },
  });
  return config;
}

async function main() {
  const args = process.argv.slice(2);
  const channel = args[0];
  requireThat(
    ["prod", "beta"].includes(channel),
    "Usage: node scripts/osir-build-windows.mjs <prod|beta> [--check] [--target x86_64-pc-windows-msvc]",
  );
  const targetIndex = args.indexOf("--target");
  const target =
    targetIndex < 0 ? "x86_64-pc-windows-msvc" : args[targetIndex + 1];
  requireThat(
    target === "x86_64-pc-windows-msvc",
    "Only Windows x64 is supported by this verified lifecycle workflow",
  );
  requireThat(
    args.slice(1).every((arg) => ["--check", "--target", target].includes(arg)),
    "Unknown Windows build option",
  );
  const version = json(join(root, "package.json")).version;
  requireThat(
    json(join(native, "tauri.conf.json")).version === version &&
      readFileSync(join(native, "Cargo.toml"), "utf8").match(
        /^version\s*=\s*"([^"]+)"/m,
      )?.[1] === version,
    "Versions must match",
  );
  const publicPath = join(root, `config/osir-updater-${channel}.json`);
  const publicInput = loadUpdaterReleaseConfig(publicPath, {
    channel,
    upstreamPublicKey: json(join(native, "tauri.conf.json")).plugins.updater
      .pubkey,
  });
  const config = windowsConfig(channel, publicInput.config);
  const source = sourceSnapshot();
  const git = gitProvenance();
  requireThat(
    !process.env.OSIR_SOURCE_SHA256 ||
      process.env.OSIR_SOURCE_SHA256 === source.provenance.sourceSha256,
    "CI source fingerprint differs from requested snapshot",
  );
  if (args.includes("--check")) {
    console.log(
      JSON.stringify(
        {
          channel,
          version,
          target,
          signing: "unsigned",
          updaterSigning: "pending-local",
          sourceSha256: source.provenance.sourceSha256,
          git,
          identifier: config.identifier,
        },
        null,
        2,
      ),
    );
    return;
  }
  requireThat(
    process.platform === "win32" && process.arch === "x64",
    "Windows x64 host required; no cross-build runtime claims",
  );
  const outputParent = join(
    root,
    "release/osir",
    channel,
    "pending-windows",
    target,
  );
  mkdirSync(outputParent, { recursive: true });
  const output = mkdtempSync(join(outputParent, version + "-"));
  const cache = resolve(process.env.CARGO_TARGET_DIR || join(native, "target"));
  const environment = packagingEnvironments(
    {
      ...process.env,
      CARGO_TARGET_DIR: cache,
      CARGO_BUILD_JOBS: "1",
      CARGO_INCREMENTAL: "0",
    },
    { release: false, updater: false },
  );
  let complete = false;
  try {
    // Reuse compiled dependencies, but never select a previous NSIS installer.
    rmSync(join(cache, target, "release/bundle/nsis"), {
      recursive: true,
      force: true,
    });
    run(
      [join(root, "node_modules/vite/bin/vite.js"), "build"],
      environment.renderer,
      "Build embedded Windows renderer",
    );
    run(
      [
        join(root, "node_modules/@tauri-apps/cli/tauri.js"),
        "build",
        "--ci",
        "--no-sign",
        "--config",
        JSON.stringify(config),
        "--target",
        target,
        "--bundles",
        "nsis",
        "--",
        "--locked",
        "--timings",
        "-j",
        "1",
      ],
      environment.native,
      "Build unsigned Windows NSIS installer",
    );
    const binary = join(cache, target, "release/osir_client.exe");
    const mainPe = inspectPe(binary, { expectedMachine: "x86_64" });
    requireThat(
      !mainPe.authenticodeCertificatePresent,
      "CI payload unexpectedly has an Authenticode certificate",
    );
    const nsis = join(cache, target, "release/bundle/nsis");
    const installers = readdirSync(nsis).filter((name) =>
      name.endsWith("-setup.exe"),
    );
    requireThat(
      installers.length === 1,
      "Expected exactly one fresh NSIS installer",
    );
    const installer = join(nsis, installers[0]);
    const installerPe = inspectPe(installer);
    requireThat(
      !installerPe.authenticodeCertificatePresent,
      "CI installer unexpectedly has an Authenticode certificate",
    );
    const installerName = `OSIR-Client-${channel}-${version}-${target}-setup.exe`;
    cpSync(installer, join(output, installerName), {
      errorOnExist: true,
      force: false,
    });
    const compiled = readFileSync(binary);
    const packagedMain = nsisPayload(compiled).bytes;
    const nsisPayloadDerivation = verifyNsisPayload(compiled, packagedMain);
    // Tauri restores Cargo's UNK marker after embedding an NSS-patched copy in
    // NSIS. Installed-byte verification must use that final payload as baseline.
    cpSync(binary, join(output, "compiled-unbundled.exe"), {
      errorOnExist: true,
      force: false,
    });
    writeFileSync(join(output, "osir_client.exe"), packagedMain, {
      flag: "wx",
    });
    const compiledReleaseDiagnostics = verifyReleaseDiagnostics(
      join(output, "compiled-unbundled.exe"),
      { config, updatesEnabled: true, bundleType: null },
    );
    const releaseDiagnostics = verifyReleaseDiagnostics(
      join(output, "osir_client.exe"),
      { config, updatesEnabled: true, bundleType: "nsis" },
    );
    verifyUpdaterConfigUnchanged(publicPath, publicInput.sha256);
    const sourceProvenance = verifySourceSnapshot(source, git);
    const manifest = {
      schemaVersion: 1,
      channel,
      version,
      target,
      architectures: ["x86_64"],
      mode: "ci-pending-local-finalization",
      identifier: config.identifier,
      scheme: config.plugins["deep-link"].desktop.schemes[0],
      renderer: "embedded",
      signing: "windows-unsigned-updater-pending",
      authenticode: "unsigned",
      distributionSignatureVerified: false,
      codeSignatureVerified: false,
      notarization: "not-applicable",
      peArchitectureVerified: true,
      mainPe,
      installerPe,
      updaterConfigured: true,
      updaterRuntimeConfigured: true,
      updaterEnabled: true,
      updaterSignature: "pending-local",
      updaterConfigSha256: publicInput.sha256,
      embeddedUpdaterPublicConfigVerified: true,
      compiledReleaseDiagnostics,
      releaseDiagnostics,
      installerArtifact: digest(join(output, installerName)),
      updaterPayload: digest(join(output, installerName)),
      mainExecutableArtifact: digest(join(output, "osir_client.exe")),
      compiledExecutableArtifact: digest(
        join(output, "compiled-unbundled.exe"),
      ),
      nsisPayloadDerivation,
      sourceProvenance,
      packagedLifecycleAcceptance: "pending-CI-smoke",
      builtAt: new Date().toISOString(),
    };
    writeFileSync(
      join(output, "build-manifest.json"),
      JSON.stringify(manifest, null, 2) + "\n",
    );
    writeFileSync(
      join(output, "SHA256SUMS"),
      [
        manifest.installerArtifact,
        manifest.mainExecutableArtifact,
        manifest.compiledExecutableArtifact,
      ]
        .map((x) => `${x.sha256}  ${x.name}`)
        .join("\n") + "\n",
    );
    writeFileSync(
      join(root, "release/osir/windows-latest-output.json"),
      JSON.stringify({ directory: output }) + "\n",
    );
    complete = true;
    console.log(
      "Unsigned Windows artifacts ready for CI smoke and local updater signing: " +
        output,
    );
  } finally {
    if (!complete) rmSync(output, { recursive: true, force: true });
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
  main().catch((error) => {
    console.error("OSIR Windows packaging: " + error.message);
    process.exitCode = 1;
  });
