#!/usr/bin/env node
// Packaging only: never launch an app, change HOME, or provision a Keychain.
import {
  accessSync,
  closeSync,
  constants,
  cpSync,
  existsSync,
  fstatSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readdirSync,
  readSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { arch } from "node:os";
import { basename, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { buildDmg } from "./osir-dmg.mjs";
import {
  readDevSigningConfig,
  prepareDevSigning,
} from "./osir-dev-signing.mjs";
import {
  createUpdaterManifest,
  loadUpdaterReleaseConfig,
  packagingEnvironments,
  requireUpdaterSigner,
  updaterConfigPatch,
  verifyUpdaterArtifact,
  verifyUpdaterConfigUnchanged,
} from "./osir-release-updater.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const native = join(root, "src-tauri");
const pnpm = process.platform === "win32" ? "pnpm.cmd" : "pnpm";
const channels = {
  dev: [
    "OSIR Client Dev",
    "com.osir.client.dev",
    "osir-client-dev",
    "tauri.osir-dev.conf.json",
  ],
  beta: [
    "OSIR Client Beta",
    "com.osir.client.beta",
    "osir-client-beta",
    "tauri.osir-beta.conf.json",
  ],
  prod: [
    "OSIR Client",
    "com.osir.client",
    "osir-client",
    "tauri.osir.conf.json",
  ],
};
const help = [
  "OSIR macOS packaging (never launches an app)",
  "  node scripts/osir-build.mjs --check",
  "  node scripts/osir-build.mjs <dev|beta|prod> --local-unsigned [--dry-run|--check]",
  "  node scripts/osir-build.mjs <beta|prod> --release [--dry-run|--check]",
  "  node scripts/osir-build.mjs <beta|prod> --release-adhoc --updater-config /absolute/public.json [--dry-run|--check]",
  "  Local stable identity: --release-self-signed --updater-config /absolute/public.json",
  "  Optional: --target aarch64-apple-darwin|x86_64-apple-darwin|universal-apple-darwin",
  "  Release only: --updater-config /absolute/path/to/public-osir-updater.json",
  "check/dry-run: config and environment preflight only; no builds, Keychain, writes or subprocesses.",
  "release: requires an existing Developer ID identity and notarization credentials; no import/unlock.",
  "release-adhoc: explicit distribution without Apple notarization; independent updater signing required.",
  "local-unsigned: explicit local-only ad-hoc app; never a signed distribution.",
].join("\n");

function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}
function json(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function archiveDigest(path) {
  const fd = openSync(
    path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  try {
    const before = fstatSync(fd);
    requireThat(
      before.isFile() && before.size > 0,
      "Installer archive must be a nonempty regular file.",
    );
    const hash = createHash("sha256");
    const buffer = Buffer.allocUnsafe(64 * 1024);
    let bytes = 0;
    for (;;) {
      const size = readSync(fd, buffer, 0, buffer.length, null);
      if (!size) break;
      hash.update(buffer.subarray(0, size));
      bytes += size;
    }
    const after = fstatSync(fd);
    requireThat(
      bytes === before.size &&
        after.size === before.size &&
        after.mtimeMs === before.mtimeMs &&
        after.ctimeMs === before.ctimeMs,
      "Installer archive changed while hashing.",
    );
    return { name: basename(path), sha256: hash.digest("hex"), bytes };
  } finally {
    closeSync(fd);
  }
}

// Only declared build inputs are traversed. Never inspect a tool HOME, build
// output or .git contents, and never follow a symlink within the input tree.
const inputDirectories = [
  "assets/brand/osir-client",
  "src",
  "public",
  "icons",
  "vendor",
  "src-tauri/src",
  "src-tauri/capabilities",
  "src-tauri/icons",
  "src-tauri/vendor",
];
const inputFiles = [
  ".gitattributes",
  ".gitignore",
  "src-tauri/.gitignore",
  "package.json",
  "pnpm-lock.yaml",
  "pnpm-workspace.yaml",
  ".node-version",
  "rust-toolchain.toml",
  "scripts/osir-build.mjs",
  "scripts/osir-build-windows.mjs",
  "scripts/osir-build-macos-ci.mjs",
  "scripts/osir-pe-verify.mjs",
  "scripts/osir-ci-artifact.mjs",
  "scripts/osir-windows-packaged-smoke.ps1",
  "scripts/osir-source-snapshot.mjs",
  "scripts/osir-platform-build.test.mjs",
  ".github/workflows/osir-windows-release.yml",
  "scripts/osir-dev-signing.mjs",
  "scripts/osir-icons.py",
  "scripts/requirements-icons.txt",
  "scripts/osir-release-updater.mjs",
  "scripts/osir-release-publisher.mjs",
  "scripts/osir-release-publisher.test.mjs",
  "scripts/osir-dmg.mjs",
  "scripts/osir-release-local.mjs",
  "scripts/osir-release-finalize.mjs",
  "scripts/osir-release-cloud.mjs",
  "config/osir-updater-beta.json",
  "config/osir-updater-prod.json",
  "components.json",
  ".cargo/config",
  ".cargo/config.toml",
  "src-tauri/.cargo/config",
  "src-tauri/.cargo/config.toml",
  "src-tauri/Cargo.toml",
  "src-tauri/Cargo.lock",
  "src-tauri/build.rs",
  "src-tauri/Info.plist",
  "src-tauri/Entitlements.plist",
  "src-tauri/common-controls.manifest",
];
const inputPatterns = {
  ".": /^(?:vite\.config\.(?:ts|js|mts|mjs)|tsconfig(?:\.[\w-]+)*\.json|(?:tailwind|postcss)\.config\.(?:js|cjs|mjs|ts))$/,
  "src-tauri":
    /^(?:tauri(?:\.[\w-]+)*\.conf\.json5?|.*\.entitlements(?:\.plist)?)$/,
};
const excludedInputDirectories = new Set([
  ".git",
  "node_modules",
  "target",
  "release",
  "dist",
  ".osir-dev-home",
  ".cc-switch",
  ".codex",
  ".claude",
  ".gemini",
]);

export function sourceSnapshot() {
  const sourceRoot = realpathSync(root);
  function statInput(relative) {
    let absolute = sourceRoot;
    let stat;
    for (const component of relative.split("/")) {
      if (component === ".") continue;
      absolute = join(absolute, component);
      try {
        stat = lstatSync(absolute, { bigint: true });
      } catch (error) {
        if (error.code === "ENOENT") return;
        throw error;
      }
      requireThat(
        !stat.isSymbolicLink(),
        "Build input symlinks are not supported; no symlink target was read.",
      );
    }
    return stat;
  }
  const files = new Set(inputFiles);
  for (const [directory, pattern] of Object.entries(inputPatterns)) {
    if (directory !== ".") statInput(directory);
    for (const name of readdirSync(join(sourceRoot, directory)))
      if (pattern.test(name))
        files.add(directory === "." ? name : directory + "/" + name);
  }
  const inputs = {
    directories: inputDirectories,
    files: [...files].sort(),
    patterns: Object.fromEntries(
      Object.entries(inputPatterns).map(([path, pattern]) => [
        path,
        pattern.source,
      ]),
    ),
    excludedDirectoryNames: [...excludedInputDirectories].sort(),
    symlinks: "rejected-without-following",
  };
  const inputsManifestSha256 = createHash("sha256")
    .update(JSON.stringify(inputs))
    .digest("hex");
  const content = createHash("sha256").update(inputsManifestSha256);
  const state = createHash("sha256");
  const buffer = Buffer.allocUnsafe(64 * 1024);
  let fileCount = 0;
  function metadata(stat) {
    return [
      stat.dev,
      stat.ino,
      stat.mode,
      stat.size,
      stat.mtimeNs,
      stat.ctimeNs,
    ].map(String);
  }
  function visit(relative) {
    const stat = statInput(relative);
    if (!stat) {
      content.update(JSON.stringify(["missing", relative]) + "\0");
      return;
    }
    state.update(JSON.stringify([relative, ...metadata(stat)]) + "\0");
    if (stat.isDirectory()) {
      content.update(JSON.stringify(["directory", relative]) + "\0");
      for (const name of readdirSync(join(sourceRoot, relative)).sort()) {
        if (!excludedInputDirectories.has(name)) visit(relative + "/" + name);
      }
      return;
    }
    requireThat(
      stat.isFile(),
      "Build inputs must be regular files or directories.",
    );
    const fd = openSync(
      join(sourceRoot, relative),
      constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
    );
    try {
      const before = fstatSync(fd, { bigint: true });
      requireThat(
        before.isFile() &&
          JSON.stringify(metadata(before)) === JSON.stringify(metadata(stat)),
        "Build inputs changed while hashing; retry from stable sources.",
      );
      content.update(
        JSON.stringify(["file", relative, String(stat.size)]) + "\0",
      );
      let total = 0n;
      for (;;) {
        const size = readSync(fd, buffer, 0, buffer.length, null);
        if (!size) break;
        content.update(buffer.subarray(0, size));
        total += BigInt(size);
      }
      requireThat(
        total === before.size &&
          JSON.stringify(metadata(before)) ===
            JSON.stringify(metadata(fstatSync(fd, { bigint: true }))),
        "Build inputs changed while hashing; retry from stable sources.",
      );
      content.update("\0");
      fileCount++;
    } finally {
      closeSync(fd);
    }
  }
  for (const directory of inputDirectories) visit(directory);
  for (const file of inputs.files) visit(file);
  return {
    provenance: {
      schemaVersion: 1,
      algorithm: "sha256",
      sourceSha256: content.digest("hex"),
      inputsManifestSha256,
      fileCount,
      inputs,
    },
    stateSha256: state.digest("hex"),
  };
}

export function gitProvenance() {
  function git(args) {
    const result = spawnSync(
      "git",
      ["--no-optional-locks", "-c", "core.fsmonitor=false", ...args],
      {
        cwd: root,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
        env: { ...process.env, GIT_OPTIONAL_LOCKS: "0" },
        timeout: 15000,
      },
    );
    requireThat(
      !result.error && result.status === 0,
      "Cannot establish Git provenance; no artifact is approved.",
    );
    return result.stdout;
  }
  const head = git(["rev-parse", "--verify", "HEAD"]).trim();
  requireThat(
    /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(head),
    "Invalid Git HEAD; no artifact is approved.",
  );
  return {
    head,
    dirty:
      git(["status", "--porcelain=v1", "-z", "--untracked-files=normal"])
        .length > 0,
  };
}

export function verifySourceSnapshot(start, gitStart) {
  const end = sourceSnapshot();
  const gitEnd = gitProvenance();
  requireThat(
    start.provenance.sourceSha256 === end.provenance.sourceSha256 &&
      start.provenance.inputsManifestSha256 ===
        end.provenance.inputsManifestSha256 &&
      start.stateSha256 === end.stateSha256 &&
      gitStart.head === gitEnd.head,
    "Build inputs or Git HEAD changed during packaging; incomplete output removed. Stop source edits and rebuild.",
  );
  return {
    ...start.provenance,
    git: { ...gitEnd, dirtyAtStart: gitStart.dirty },
    verifiedAtEnd: true,
  };
}
// Match the JSON Merge Patch used by Tauri: null removes inherited values.
function merge(base, patch) {
  if (!patch || typeof patch !== "object" || Array.isArray(patch)) return patch;
  const result = { ...base };
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete result[key];
    else result[key] = merge(result[key], value);
  }
  return result;
}
function parse(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (arg === "--") continue; // pnpm argument separator only, never arbitrary passthrough.
    if (Object.hasOwn(channels, arg) && !options.channel) options.channel = arg;
    else if (arg === "--target" && !options.target)
      options.target = argv[++index];
    else if (arg === "--updater-config" && !options.updaterConfig)
      options.updaterConfig = argv[++index];
    else if (
      [
        "--check",
        "--dry-run",
        "--local-unsigned",
        "--release",
        "--release-adhoc",
        "--release-self-signed",
        "--help",
      ].includes(arg)
    ) {
      requireThat(!options[arg], "Duplicate option: " + arg);
      options[arg] = true;
    } else throw new Error("Unknown or duplicate argument.\n" + help);
  }
  if (options["--help"]) return options;
  requireThat(
    !(options["--check"] && options["--dry-run"]),
    "Choose --check or --dry-run.",
  );
  requireThat(
    [
      "--local-unsigned",
      "--release",
      "--release-adhoc",
      "--release-self-signed",
    ].filter((key) => options[key]).length <= 1,
    "Choose exactly one packaging mode.",
  );
  requireThat(
    options.channel || options["--check"],
    "A channel is required.\n" + help,
  );
  requireThat(
    options["--check"] ||
      options["--local-unsigned"] ||
      options["--release"] ||
      options["--release-adhoc"] ||
      options["--release-self-signed"],
    "An explicit packaging mode is required.\n" + help,
  );
  requireThat(
    !(
      options["--release"] ||
      options["--release-adhoc"] ||
      options["--release-self-signed"]
    ) ||
      (options.channel && options.channel !== "dev"),
    "Release is only available for beta or prod.",
  );
  requireThat(
    !argv.includes("--updater-config") ||
      (options.updaterConfig &&
        (options["--release"] ||
          options["--release-adhoc"] ||
          options["--release-self-signed"]) &&
        options.channel !== "dev"),
    "--updater-config requires an explicit beta/prod distribution mode and a public configuration file.",
  );
  requireThat(
    !(options["--release-adhoc"] || options["--release-self-signed"]) ||
      options.updaterConfig,
    "Ad-hoc distribution requires independent signed OSIR updates.",
  );
  const targets = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "universal-apple-darwin",
  ];
  requireThat(
    !argv.includes("--target") || targets.includes(options.target),
    "Unsupported or missing macOS --target.",
  );
  return options;
}

function channelConfig(channel) {
  const [name, identifier, scheme, filename] = channels[channel];
  const base = json(join(native, "tauri.conf.json"));
  const platform = join(native, "tauri.macos.conf.json");
  const merged = merge(
    existsSync(platform) ? merge(base, json(platform)) : base,
    json(join(native, filename)),
  );
  requireThat(
    merged.productName === name && merged.identifier === identifier,
    "Channel name/identifier mismatch: " + channel,
  );
  requireThat(
    JSON.stringify(merged.plugins?.["deep-link"]?.desktop?.schemes) ===
      JSON.stringify([scheme]),
    "Channel scheme mismatch: " + channel,
  );
  requireThat(
    merged.bundle?.createUpdaterArtifacts === false &&
      merged.plugins?.updater?.endpoints?.length === 0,
    "Updater must remain disabled: " + channel,
  );
  // The wrapper builds the renderer once and embeds it, even for a debug package.
  const config = merge(merged, {
    build: {
      frontendDist: "../dist",
      devUrl: null,
      beforeDevCommand: null,
      beforeBuildCommand: null,
      beforeBundleCommand: null,
    },
    bundle: {
      active: true,
      targets: ["app"],
      createUpdaterArtifacts: false,
      macOS: { hardenedRuntime: true },
    },
    plugins: { updater: { pubkey: "", endpoints: [] } },
  });
  // Keep explicit tombstones in the CLI patch; omitting them would inherit the
  // base dev URL / build hook again when Tauri merges this JSON a second time.
  Object.assign(config.build, {
    devUrl: null,
    beforeDevCommand: null,
    beforeBuildCommand: null,
    beforeBundleCommand: null,
  });
  return config;
}

export function configuredChannel(channel, updater) {
  return merge(channelConfig(channel), updaterConfigPatch(updater));
}

function buildArgs(options, config) {
  const args = [
    "exec",
    "tauri",
    "build",
    "--ci",
    "--config",
    JSON.stringify(config),
    "--bundles",
    "app",
  ];
  if (options.channel === "dev") args.push("--debug");
  if (options.target) args.push("--target", options.target);
  if (
    !options["--release"] &&
    !options["--release-adhoc"] &&
    !options["--release-self-signed"]
  )
    args.push("--no-sign");
  args.push("--", "--locked", "-j", "1");
  return args;
}

export function checkProject() {
  const pkg = json(join(root, "package.json"));
  const base = json(join(native, "tauri.conf.json"));
  const cargoVersion = readFileSync(join(native, "Cargo.toml"), "utf8").match(
    /^version\s*=\s*"([^"]+)"/m,
  )?.[1];
  requireThat(
    pkg.version === base.version && pkg.version === cargoVersion,
    "package.json, Tauri and Cargo versions must match.",
  );
  requireThat(
    pkg.scripts["build:renderer"] === "vite build",
    "Review the changed renderer build command before packaging.",
  );
  requireThat(
    existsSync(join(native, "Cargo.lock")),
    "Missing Cargo.lock; locked build required.",
  );
  const cli = json(join(root, "node_modules/@tauri-apps/cli/package.json"));
  requireThat(
    cli.version === "2.10.1",
    "Packaging flags were validated with Tauri CLI 2.10.1; review wrapper when upgrading.",
  );
  const identities = [],
    names = [],
    schemes = [];
  for (const channel of Object.keys(channels)) {
    const config = channelConfig(channel);
    identities.push(config.identifier);
    names.push(config.productName);
    schemes.push(config.plugins["deep-link"].desktop.schemes[0]);
    requireThat(
      config.build.frontendDist === "../dist" &&
        !config.build.devUrl &&
        !config.build.beforeDevCommand &&
        !config.build.beforeBuildCommand,
      "Packaged renderer configuration invalid.",
    );
    requireThat(
      config.plugins.updater.pubkey === "",
      "Upstream updater key must not be inherited.",
    );
    for (const icon of config.bundle.icon ?? [])
      requireThat(
        existsSync(resolve(native, icon)),
        "Missing bundle icon: " + icon,
      );
  }
  for (const values of [identities, names, schemes])
    requireThat(
      new Set(values).size === 3,
      "Channel identities, names and schemes must be distinct.",
    );
  return base.version;
}

// Only presence/file accessibility checks here. Never read a private key or query Keychain.
function releasePreflight() {
  const env = process.env;
  const missing = ["APPLE_SIGNING_IDENTITY", "APPLE_TEAM_ID"].filter(
    (key) => !env[key]?.trim(),
  );
  requireThat(
    !missing.length,
    "Release blocked; missing " +
      missing.join(", ") +
      ". No signing/notarization attempted.",
  );
  const identityTeam = env.APPLE_SIGNING_IDENTITY.match(
    /^Developer ID Application: .+ \(([A-Z0-9]{10})\)$/,
  )?.[1];
  requireThat(
    identityTeam && identityTeam === env.APPLE_TEAM_ID,
    "Release requires a Developer ID Application identity matching APPLE_TEAM_ID (ad-hoc signing is rejected).",
  );
  requireThat(
    !env.APPLE_CERTIFICATE && !env.APPLE_CERTIFICATE_PASSWORD,
    "Certificate import is not supported; provision the signing identity outside this wrapper.",
  );
  const api = ["APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH"];
  const account = ["APPLE_ID", "APPLE_PASSWORD"];
  const useApi = api.some((key) => env[key]);
  const required = useApi ? api : account;
  requireThat(
    required.every((key) => env[key]?.trim()),
    "Release blocked; notarization requires " + required.join(", ") + ".",
  );
  requireThat(
    !(useApi && account.some((key) => env[key])),
    "Choose exactly one notarization credential method.",
  );
  if (useApi) {
    requireThat(
      lstatSync(env.APPLE_API_KEY_PATH).isFile(),
      "APPLE_API_KEY_PATH must be a readable regular file.",
    );
    accessSync(env.APPLE_API_KEY_PATH, constants.R_OK);
  }
  requireThat(
    !env.TAURI_SKIP_NOTARIZATION && !env.APPLE_SKIP_NOTARIZATION,
    "Notarization bypass variables are not allowed for release.",
  );
  return useApi ? "app-store-connect-api" : "apple-id-app-password";
}

function checkedTool(command, args, label) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 180000,
  });
  requireThat(
    !result.error && result.status === 0,
    label + " failed; no distribution output is approved.",
  );
  return (result.stdout || "") + (result.stderr || "");
}

let child;
let stopping = false;
let killTimer;
function signalChild(signal) {
  if (!child?.pid) return;
  try {
    process.kill(-child.pid, signal);
  } catch (error) {
    if (error.code === "ESRCH") return;
    if (error.code === "EPERM") {
      // macOS can report EPERM for a group that disappeared while its
      // leader's exit callback ran. Confirm absence before releasing its lock.
      const groups = spawnSync("/bin/ps", ["-axo", "pgid="], {
        encoding: "utf8",
      });
      if (
        !groups.error &&
        groups.status === 0 &&
        /^\s*\d+(?:\s+\d+)*\s*$/.test(groups.stdout) &&
        !groups.stdout.trim().split(/\s+/).includes(String(child.pid))
      )
        return;
    }
    throw error;
  }
}
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    stopping = true;
    signalChild(signal);
    if (!killTimer)
      killTimer = setTimeout(() => signalChild("SIGKILL"), 5000).unref();
  });
}
async function run(command, args, env, label) {
  requireThat(!stopping, "Packaging interrupted.");
  console.log(label);
  await new Promise((resolveRun, reject) => {
    child = spawn(command, args, {
      cwd: root,
      env,
      stdio: "inherit",
      detached: true,
    });
    child.once("error", () => reject(new Error(label + " could not start.")));
    child.once("exit", (code, signal) => {
      // A terminated pnpm leader may leave Cargo/tauri in its process group.
      // Finish cleanup before releasing the packaging lock.
      if (code !== 0 || signal || stopping) signalChild("SIGKILL");
      child = undefined;
      if (code === 0 && !signal && !stopping) resolveRun();
      else reject(new Error(label + " failed or was interrupted."));
    });
  });
}

export function verifyRenderer() {
  const dist = join(root, "dist");
  for (const entry of ["index.html", "tray-panel.html"]) {
    const html = readFileSync(join(dist, entry), "utf8");
    requireThat(
      !/@vite\/client|@react-refresh|localhost:3000|\/src\/main\./.test(html),
      "Renderer contains development server references.",
    );
    const sources = [...html.matchAll(/<script\b[^>]*\bsrc="([^"]+)"/g)].map(
      (match) => match[1],
    );
    requireThat(
      sources.length > 0,
      "Renderer entry has no packaged script: " + entry,
    );
    for (const source of sources) {
      const asset = resolve(dist, source);
      requireThat(
        !source.includes(":") &&
          asset.startsWith(dist + sep) &&
          existsSync(asset),
        "Renderer script must be a local packaged asset.",
      );
    }
  }
  for (const entry of readdirSync(join(dist, "assets"))) {
    if (entry.endsWith(".js"))
      requireThat(
        !/\/@vite\/client|\/@react-refresh/.test(
          readFileSync(join(dist, "assets", entry), "utf8"),
        ),
        "Vite development runtime found in packaged renderer.",
      );
  }
}

export function infoPlist(config) {
  // Replace upstream Info.plist: it explicitly registers ccswitch://.
  // Channel values are hard-coded/validated above and contain no XML metacharacters.
  const scheme = config.plugins["deep-link"].desktop.schemes[0];
  return (
    '<?xml version="1.0" encoding="UTF-8"?>\n' +
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n' +
    '<plist version="1.0"><dict><key>CFBundleURLTypes</key><array><dict>' +
    "<key>CFBundleURLName</key><string>" +
    config.identifier +
    "</string>" +
    "<key>CFBundleURLSchemes</key><array><string>" +
    scheme +
    "</string></array>" +
    "</dict></array><key>NSLocalNetworkUsageDescription</key>" +
    "<string>OSIR Client uses the local network to connect to your configured API endpoints.</string></dict></plist>\n"
  );
}

export function verifyApp(app, config, target, release) {
  requireThat(
    lstatSync(app).isDirectory(),
    "Expected app bundle was not produced.",
  );
  const plist = JSON.parse(
    checkedTool(
      "/usr/bin/plutil",
      ["-convert", "json", "-o", "-", join(app, "Contents/Info.plist")],
      "Bundle plist verification",
    ),
  );
  requireThat(
    plist.CFBundleIdentifier === config.identifier &&
      plist.CFBundleShortVersionString === config.version,
    "Bundle identity/version mismatch.",
  );
  requireThat(
    (plist.CFBundleDisplayName || plist.CFBundleName) === config.productName,
    "Bundle display name mismatch.",
  );
  const schemes =
    plist.CFBundleURLTypes?.flatMap((type) => type.CFBundleURLSchemes ?? []) ??
    [];
  requireThat(
    schemes.length > 0 &&
      schemes.every(
        (scheme) => scheme === config.plugins["deep-link"].desktop.schemes[0],
      ),
    "Bundle registered another channel or upstream URL scheme.",
  );
  requireThat(
    plist.CFBundleExecutable &&
      basename(plist.CFBundleExecutable) === plist.CFBundleExecutable &&
      plist.CFBundleExecutable === "osir_client",
    "Invalid bundle executable.",
  );
  const executable = join(app, "Contents/MacOS", plist.CFBundleExecutable);
  accessSync(executable, constants.X_OK);
  const binaryArchs = checkedTool(
    "/usr/bin/lipo",
    ["-archs", executable],
    "Architecture verification",
  )
    .trim()
    .split(/\s+/);
  const expected =
    target === "universal-apple-darwin"
      ? ["arm64", "x86_64"]
      : [target.startsWith("aarch64") ? "arm64" : "x86_64"];
  requireThat(
    expected.every((value) => binaryArchs.includes(value)),
    "Bundle architecture does not match requested target.",
  );
  checkedTool(
    "/usr/bin/codesign",
    ["--verify", "--deep", "--strict", app],
    "Code signature verification",
  );
  if (release) {
    const details = checkedTool(
      "/usr/bin/codesign",
      ["--display", "--verbose=4", app],
      "Signing details verification",
    );
    requireThat(
      details.includes(
        "Authority=" + process.env.APPLE_SIGNING_IDENTITY + "\n",
      ) &&
        details.includes(
          "TeamIdentifier=" + process.env.APPLE_TEAM_ID + "\n",
        ) &&
        /flags=0x[^\n]*\bruntime\b/.test(details),
      "Release signature must match Developer ID/team and enable hardened runtime.",
    );
    checkedTool(
      "/usr/bin/xcrun",
      ["stapler", "validate", app],
      "Stapled notarization verification",
    );
    checkedTool(
      "/usr/sbin/spctl",
      ["--assess", "--type", "execute", "--verbose=2", app],
      "Gatekeeper verification",
    );
  }
  return binaryArchs;
}

function ensureDevelopmentIdle() {
  const pidPath = join(root, ".osir-dev-home/.dev-launcher-lock/pid");
  if (!existsSync(pidPath)) return;
  const pid = Number(readFileSync(pidPath, "utf8").trim());
  requireThat(
    Number.isSafeInteger(pid) && pid > 0,
    "Development launcher lock is unreadable; diagnose/stop development before packaging.",
  );
  try {
    process.kill(pid, 0);
  } catch (error) {
    if (error.code === "ESRCH") return;
    throw error;
  }
  throw new Error(
    "Development launcher is active; stop it before packaging to avoid competing builds.",
  );
}

async function main() {
  const options = parse(process.argv.slice(2));
  if (options["--help"]) {
    console.log(help);
    return;
  }
  if (options.updaterConfig)
    options.updaterConfig = resolve(root, options.updaterConfig);
  const source = sourceSnapshot();
  const git =
    options["--check"] || options["--dry-run"] ? null : gitProvenance();
  const version = checkProject();
  const release = Boolean(options["--release"]);
  const adhoc = Boolean(options["--release-adhoc"]);
  const selfSigned = Boolean(options["--release-self-signed"]);
  const distribution = release || adhoc || selfSigned;
  const localIdentity = selfSigned
    ? options["--check"] || options["--dry-run"]
      ? readDevSigningConfig()
      : prepareDevSigning()
    : null;
  if (selfSigned)
    requireThat(
      localIdentity && localIdentity.identity !== "-",
      "Stable local certificate is required; no fallback to ad-hoc.",
    );
  const updaterInput = options.updaterConfig
    ? loadUpdaterReleaseConfig(options.updaterConfig, {
        channel: options.channel,
        upstreamPublicKey: json(join(native, "tauri.conf.json")).plugins.updater
          .pubkey,
      })
    : null;
  const updater = updaterInput?.config ?? null;
  if (updater) requireUpdaterSigner(process.env);
  const authMethod = release ? releasePreflight() : null;
  const cachePath = resolve(
    root,
    process.env.CARGO_TARGET_DIR || "src-tauri/target",
  );
  const cache = existsSync(cachePath) ? realpathSync(cachePath) : cachePath;
  const target =
    options.target ||
    (arch() === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin");
  const selected = options.channel ? [options.channel] : Object.keys(channels);
  const mode = release
    ? "release-distribution"
    : selfSigned
      ? "release-self-signed-distribution"
      : adhoc
        ? "release-adhoc-distribution"
        : options["--local-unsigned"]
          ? "local-unsigned"
          : "validation-only";
  const plan = {
    mode,
    version,
    target,
    cargoTargetDir: cache,
    cargoJobs: 1,
    toolchain: process.env.RUSTUP_TOOLCHAIN || "stable",
    channels: selected.map((channel) => {
      const config = configuredChannel(channel, updater);
      const preview = structuredClone(config);
      preview.bundle.macOS.infoPlist = "<temporary-channel-only-Info.plist>";
      preview.bundle.macOS.signingIdentity = release
        ? "<required Developer ID identity>"
        : selfSigned
          ? localIdentity.identity
          : "-";
      return {
        channel,
        name: config.productName,
        identifier: config.identifier,
        scheme: config.plugins["deep-link"].desktop.schemes[0],
        profile: channel === "dev" ? "debug" : "release",
        renderer: "embedded dist (no Vite server)",
        updater: updater
          ? "signed artifacts and release runtime configured; upgrade acceptance pending"
          : "disabled",
        ...(options["--dry-run"]
          ? { command: [pnpm, ...buildArgs({ ...options, channel }, preview)] }
          : {}),
      };
    }),
    steps: [
      "pnpm run build:renderer (once)",
      "validate packaged renderer",
      "tauri build --ci --bundles app -- --locked -j 1",
      release
        ? "verify Developer ID, runtime, stapled ticket and Gatekeeper; create zip"
        : adhoc
          ? "verify ad-hoc app and independent updater signature; create distribution archive (not notarized)"
          : "ad-hoc sign for local execution; verify app",
    ],
    notarization: authMethod || "not invoked",
    launchesApp: false,
    provisionsKeychain: false,
    source: {
      sourceSha256: source.provenance.sourceSha256,
      inputsManifestSha256: source.provenance.inputsManifestSha256,
      fileCount: source.provenance.fileCount,
      git: "queried only for actual builds",
    },
    output: "release/osir/<channel>/<mode>/<target>/<version>-<unique>/",
    ...(updater
      ? {
          updaterConfigSha256: updaterInput.sha256,
          updaterRuntimeConfigured: true,
          updaterUpgradeAcceptance: "not-run",
        }
      : {}),
  };
  if (options["--check"]) {
    console.log("OSIR channels OK | version " + version + " | " + mode);
    for (const channel of plan.channels)
      console.log(
        channel.channel +
          ": " +
          channel.name +
          " | " +
          channel.identifier +
          " | " +
          channel.scheme +
          ":// | " +
          channel.profile,
      );
    console.log(
      "Renderer embedded; updater " +
        (updater
          ? "artifacts and release runtime configured; upgrade acceptance pending"
          : "disabled") +
        "; Cargo jobs=1; target=" +
        target,
    );
    console.log("Cargo cache: " + cache);
    console.log(
      "Source SHA-256: " +
        source.provenance.sourceSha256 +
        " (" +
        source.provenance.fileCount +
        " files)",
    );
    console.log(
      "Read-only preflight; no builds, subprocesses, Keychain access or writes.",
    );
  } else console.log(JSON.stringify(plan, null, 2));
  if (options["--check"] || options["--dry-run"]) return;
  requireThat(
    process.platform === "darwin" && ["arm64", "x64"].includes(arch()),
    "Actual packaging is supported only on macOS arm64/x64.",
  );
  ensureDevelopmentIdle();
  if (release) {
    const identities = checkedTool(
      "/usr/bin/security",
      ["find-identity", "-v", "-p", "codesigning"],
      "Read-only signing identity preflight",
    );
    requireThat(
      identities.includes('"' + process.env.APPLE_SIGNING_IDENTITY + '"'),
      "Release blocked; the requested Developer ID identity is not installed. No certificate import/unlock attempted.",
    );
  }
  mkdirSync(cache, { recursive: true });
  const lock = join(cache, ".osir-build-lock");
  try {
    mkdirSync(lock);
  } catch (error) {
    if (error.code !== "EEXIST") throw error;
    throw new Error(
      "Another packaging run owns " +
        lock +
        ". Check its pid file; remove only a confirmed stale lock.",
    );
  }
  let staging;
  let output;
  let generatedApp;
  let complete = false;
  try {
    writeFileSync(join(lock, "pid"), String(process.pid), { mode: 0o600 });
    // The dev launcher checks this packaging lock after acquiring its own lock.
    // Recheck its lock here to close the simultaneous-start race.
    ensureDevelopmentIdle();
    verifySourceSnapshot(source, git);
    staging = mkdtempSync(join(cache, ".osir-packaging-"));
    const config = configuredChannel(options.channel, updater);
    const plist = join(staging, "osir-channel-info.plist");
    writeFileSync(plist, infoPlist(config));
    config.bundle.macOS.infoPlist = plist;
    // The signer uses an existing identity only. The local path cannot inherit a signer.
    config.bundle.macOS.signingIdentity = release
      ? process.env.APPLE_SIGNING_IDENTITY
      : "-";
    const environment = packagingEnvironments(
      {
        ...process.env,
        CARGO_TARGET_DIR: cache,
        CARGO_BUILD_JOBS: "1",
        RUSTUP_TOOLCHAIN: process.env.RUSTUP_TOOLCHAIN || "stable",
      },
      {
        release: distribution,
        appleSigning: release,
        updater: Boolean(updater),
      },
    );
    const env = environment.native;
    const rendererEnv = environment.renderer;
    await run(pnpm, ["run", "build:renderer"], rendererEnv, "Build renderer");
    verifyRenderer();
    verifySourceSnapshot(source, git);
    const profile = options.channel === "dev" ? "debug" : "release";
    const bundleRoot = join(
      cache,
      ...(options.target ? [options.target] : []),
      profile,
      "bundle/macos",
    );
    const sourceApp = join(bundleRoot, config.productName + ".app");
    const sourceUpdater = sourceApp + ".tar.gz";
    // Remove only this generated app, never Cargo caches or other channels.
    rmSync(sourceApp, { recursive: true, force: true });
    if (updater) {
      for (const path of [sourceUpdater, sourceUpdater + ".sig"]) {
        if (!existsSync(path)) continue;
        const stat = lstatSync(path);
        requireThat(
          stat.isFile() && !stat.isSymbolicLink(),
          "Existing updater output must be a generated regular file.",
        );
        rmSync(path);
      }
      verifyUpdaterConfigUnchanged(options.updaterConfig, updaterInput.sha256);
    }
    generatedApp = sourceApp;
    await run(
      pnpm,
      buildArgs(options, config),
      env,
      "Build native app (one Cargo job)",
    );
    if (selfSigned) {
      // Tauri does not pass our dedicated keychain to codesign. Select it
      // explicitly after bundling, then regenerate the updater from that app.
      // Never let a default-keychain search prompt for another private key.
      const signing = prepareDevSigning();
      requireThat(
        signing.identity === localIdentity.identity &&
          signing.keychain === localIdentity.keychain,
        "Local signing selection changed during compilation.",
      );
      checkedTool(
        "/usr/bin/codesign",
        [
          "--force",
          "--deep",
          "--options",
          "runtime",
          "--timestamp=none",
          ...(signing.keychain ? ["--keychain", signing.keychain] : []),
          "--sign",
          signing.identity,
          sourceApp,
        ],
        "Sign app with selected stable local certificate",
      );
      if (updater) {
        checkedTool(
          "/usr/bin/tar",
          ["-czf", sourceUpdater, "-C", bundleRoot, basename(sourceApp)],
          "Archive finalized stable-signed app",
        );
        checkedTool(
          process.execPath,
          [
            join(root, "node_modules/@tauri-apps/cli/tauri.js"),
            "signer",
            "sign",
            sourceUpdater,
          ],
          "Sign finalized updater payload",
        );
      }
    }
    const parent = join(root, "release/osir", options.channel, mode, target);
    mkdirSync(parent, { recursive: true });
    output = mkdtempSync(join(parent, version + "-"));
    const app = join(output, config.productName + ".app");
    cpSync(sourceApp, app, {
      recursive: true,
      preserveTimestamps: true,
      errorOnExist: true,
      force: false,
    });
    if (!distribution)
      checkedTool(
        "/usr/bin/codesign",
        ["--force", "--deep", "--sign", "-", "--timestamp=none", app],
        "Local ad-hoc signing (no certificate)",
      );
    const binaryArchs = verifyApp(app, config, target, release);
    let localSigningRequirement;
    if (selfSigned) {
      const required = `identifier "${config.identifier}" and certificate leaf = H"${localIdentity.identity}"`;
      checkedTool(
        "/usr/bin/codesign",
        ["--verify", "--deep", "--strict", "-R=" + required, app],
        "Stable local certificate verification",
      );
      const details = checkedTool(
        "/usr/bin/codesign",
        ["-dr", "-", app],
        "Stable designated requirement verification",
      );
      localSigningRequirement = details
        .split(/\r?\n/)
        .find((line) => line.includes("designated =>"))
        ?.split("=>")
        .slice(1)
        .join("=>")
        .trim();
      requireThat(
        localSigningRequirement && !localSigningRequirement.includes("cdhash"),
        "Local signing identity must remain stable across binary changes.",
      );
    }
    if (adhoc) {
      const details = checkedTool(
        "/usr/bin/codesign",
        ["--display", "--verbose=4", app],
        "Ad-hoc signing details verification",
      );
      requireThat(
        details.includes("Signature=adhoc") && !details.includes("Authority="),
        "Ad-hoc distribution must not claim an Apple signing identity.",
      );
    }
    const installerPath = join(
      output,
      `OSIR-Client-${options.channel}-${version}-${target}.zip`,
    );
    if (distribution)
      checkedTool(
        "/usr/bin/ditto",
        ["-c", "-k", "--sequesterRsrc", "--keepParent", app, installerPath],
        release
          ? "Create notarized app archive"
          : "Create non-notarized app archive",
      );
    let updaterArtifact;
    if (updater) {
      const signatureStat = lstatSync(sourceUpdater + ".sig");
      requireThat(
        signatureStat.isFile() &&
          !signatureStat.isSymbolicLink() &&
          signatureStat.size <= 16384,
        "Expected a small regular updater signature file.",
      );
      const signature = readFileSync(sourceUpdater + ".sig", "utf8").trim();
      const artifactName = `OSIR-Client-${options.channel}-${version}-${target}.app.tar.gz`;
      const artifactPath = join(output, artifactName);
      cpSync(sourceUpdater, artifactPath, { errorOnExist: true, force: false });
      updaterArtifact = verifyUpdaterArtifact(
        artifactPath,
        signature,
        updater.pubkey,
      );
      writeFileSync(artifactPath + ".sig", signature + "\n");
      const manifest = createUpdaterManifest({
        config: updater,
        version,
        target,
        artifactName,
        signature,
        pubDate: new Date().toISOString(),
      });
      writeFileSync(
        join(output, `updater-${options.channel}.json`),
        JSON.stringify(manifest, null, 2) + "\n",
      );
      verifyUpdaterConfigUnchanged(options.updaterConfig, updaterInput.sha256);
    }
    const sourceProvenance = verifySourceSnapshot(source, git);
    const distributionMetadata = {
      channel: options.channel,
      mode,
      version,
      identifier: config.identifier,
      scheme: config.plugins["deep-link"].desktop.schemes[0],
      architectures: binaryArchs,
      renderer: "embedded",
      updaterEnabled: Boolean(updater),
      distributionSignatureVerified: release,
      codeSignatureVerified: true,
      notarization: release ? "stapled-verified" : "not-notarized",
      signing: release
        ? "developer-id-notarized"
        : selfSigned
          ? "local-certificate-not-notarized"
          : adhoc
            ? "ad-hoc-not-notarized"
            : "ad-hoc-local-only",
    };
    // DMG and updater contain the same finalized app; the helper never re-signs it.
    const dmg =
      adhoc || selfSigned
        ? await buildDmg({
            buildDirectory: output,
            manifest: distributionMetadata,
            target,
          })
        : {};
    verifySourceSnapshot(source, git);
    writeFileSync(
      join(output, "build-manifest.json"),
      JSON.stringify(
        {
          ...distributionMetadata,
          ...(selfSigned
            ? {
                localSigningIdentity: localIdentity.identity,
                localSigningRequirement,
              }
            : {}),
          renderer: "embedded",
          updaterEnabled: Boolean(updater),
          ...(updater
            ? {
                updaterConfigured: true,
                updaterRuntimeConfigured: true,
                updaterUpgradeAcceptance: "not-run",
                updaterConfigSha256: updaterInput.sha256,
                updaterArtifact,
              }
            : {}),
          ...(distribution
            ? adhoc || selfSigned
              ? { archiveArtifact: archiveDigest(installerPath), ...dmg }
              : { installerArtifact: archiveDigest(installerPath) }
            : {}),
          sourceProvenance,
          builtAt: new Date().toISOString(),
        },
        null,
        2,
      ) + "\n",
    );
    complete = true;
    console.log(
      (release
        ? "Verified signed archive: "
        : selfSigned
          ? "Verified local certificate distribution (not Apple notarized): "
          : adhoc
            ? "Verified ad-hoc distribution (not Apple notarized): "
            : "Local-only app (not for distribution): ") + output,
    );
  } finally {
    if (output && !complete) rmSync(output, { recursive: true, force: true });
    if (generatedApp && !complete)
      rmSync(generatedApp, { recursive: true, force: true });
    if (generatedApp && updater && !complete) {
      rmSync(generatedApp + ".tar.gz", { force: true });
      rmSync(generatedApp + ".tar.gz.sig", { force: true });
    }
    if (staging) rmSync(staging, { recursive: true, force: true });
    rmSync(lock, { recursive: true, force: true });
  }
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  main()
    .catch((error) => {
      // Do not dump subprocess output, env or notarization/signing credential values.
      console.error(
        "OSIR packaging: " +
          (error.code
            ? "File/tool preflight failed (" +
              error.code +
              "). Check configured paths and permissions."
            : error.message),
      );
      process.exitCode = 1;
    })
    .finally(() => {
      if (killTimer) clearTimeout(killTimer);
    });
