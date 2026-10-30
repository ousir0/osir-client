// Packages an already validated OSIR app. It never changes or signs the app,
// reads signing credentials, changes Gatekeeper, or claims Apple notarization.
import {
  closeSync,
  constants,
  fstatSync,
  linkSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  readSync,
  realpathSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const CHANNELS = {
  beta: { product: "OSIR Client Beta", identifier: "com.osir.client.beta" },
  prod: { product: "OSIR Client", identifier: "com.osir.client" },
};
const MAX_BYTES = 8 * 1024 * 1024 * 1024;
const MAX_ENTRIES = 200_000;

function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

function contained(root, path) {
  const child = relative(root, path);
  return (
    child === "" ||
    (!child.startsWith(".." + sep) && child !== ".." && !child.startsWith(sep))
  );
}

function fileDigest(path, limit = MAX_BYTES) {
  const before = lstatSync(path);
  requireThat(
    before.isFile() && !before.isSymbolicLink() && before.size <= limit,
    "DMG inputs must be bounded regular files.",
  );
  const fd = openSync(
    path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  try {
    const opened = fstatSync(fd);
    requireThat(
      opened.ino === before.ino && opened.dev === before.dev,
      "DMG input changed before reading.",
    );
    const hash = createHash("sha256");
    const buffer = Buffer.allocUnsafe(128 * 1024);
    let bytes = 0;
    for (;;) {
      const count = readSync(fd, buffer, 0, buffer.length, null);
      if (!count) break;
      hash.update(buffer.subarray(0, count));
      bytes += count;
      requireThat(bytes <= limit, "DMG input exceeds the size limit.");
    }
    const after = fstatSync(fd);
    requireThat(
      bytes === before.size &&
        after.size === before.size &&
        after.mtimeMs === before.mtimeMs &&
        after.ctimeMs === before.ctimeMs,
      "DMG input changed while reading.",
    );
    return { sha256: hash.digest("hex"), bytes };
  } finally {
    closeSync(fd);
  }
}

function readManifest(path) {
  const before = fileDigest(path, 1024 * 1024);
  const fd = openSync(
    path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(fd, "utf8"));
  } finally {
    closeSync(fd);
  }
  requireThat(
    fileDigest(path, 1024 * 1024).sha256 === before.sha256,
    "Build manifest changed while reading.",
  );
  return { manifest, sha256: before.sha256 };
}

// Include regular-file contents, executable bits, and links in the snapshot.
// Framework links are permitted only when their resolved targets stay in app.
function snapshotApp(app) {
  requireThat(
    lstatSync(app).isDirectory() && !lstatSync(app).isSymbolicLink(),
    "Expected an ordinary app bundle directory.",
  );
  const root = realpathSync(app);
  const records = [];
  let bytes = 0;
  function walk(path, depth) {
    requireThat(
      depth <= 64 && records.length < MAX_ENTRIES,
      "App bundle exceeds the directory inventory limit.",
    );
    const stat = lstatSync(path);
    const name = relative(root, path);
    if (stat.isSymbolicLink()) {
      requireThat(
        contained(root, realpathSync(path)),
        "App bundle contains a symlink escape.",
      );
      records.push([name, "link", readlinkSync(path)]);
    } else if (stat.isDirectory()) {
      records.push([name, "directory", stat.mode & 0o777]);
      const entries = readdirSync(path).sort();
      for (const entry of entries) walk(join(path, entry), depth + 1);
      const after = lstatSync(path);
      requireThat(
        after.ino === stat.ino &&
          after.dev === stat.dev &&
          after.mtimeMs === stat.mtimeMs &&
          after.ctimeMs === stat.ctimeMs,
        "App directory changed during inventory.",
      );
    } else {
      requireThat(
        stat.isFile(),
        "App bundle contains an unsupported special file.",
      );
      const digest = fileDigest(path);
      bytes += digest.bytes;
      requireThat(bytes <= MAX_BYTES, "App bundle exceeds the size limit.");
      records.push([
        name,
        "file",
        stat.mode & 0o777,
        digest.bytes,
        digest.sha256,
      ]);
    }
  }
  walk(root, 0);
  return createHash("sha256").update(JSON.stringify(records)).digest("hex");
}

export function validateDmgInput({ buildDirectory, manifest, target }) {
  requireThat(
    typeof buildDirectory === "string" && buildDirectory.length > 0,
    "A build directory is required.",
  );
  const requested = resolve(buildDirectory);
  requireThat(
    lstatSync(requested).isDirectory() &&
      !lstatSync(requested).isSymbolicLink(),
    "Build directory must be an ordinary directory, not a symlink.",
  );
  const directory = realpathSync(requested);
  requireThat(
    manifest && typeof manifest === "object" && !Array.isArray(manifest),
    "A validated build manifest is required.",
  );
  const channel = CHANNELS[manifest.channel];
  requireThat(
    channel &&
      /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(
        manifest.version,
      ),
    "DMG builds require a beta/prod channel and a valid version.",
  );
  const notarized =
    manifest.mode === "release-distribution" &&
    manifest.signing === "developer-id-notarized" &&
    manifest.distributionSignatureVerified === true;
  const adhoc =
    manifest.mode === "release-adhoc-distribution" &&
    manifest.signing === "ad-hoc-not-notarized" &&
    manifest.distributionSignatureVerified === false &&
    manifest.codeSignatureVerified === true &&
    manifest.notarization === "not-notarized" &&
    manifest.updaterEnabled === true;
  const selfSigned =
    manifest.mode === "release-self-signed-distribution" &&
    manifest.signing === "local-certificate-not-notarized" &&
    manifest.distributionSignatureVerified === false &&
    manifest.codeSignatureVerified === true &&
    manifest.notarization === "not-notarized" &&
    manifest.updaterEnabled === true;
  requireThat(
    notarized || adhoc || selfSigned,
    "DMG packaging requires an explicitly verified distribution build; local-unsigned is not accepted.",
  );
  requireThat(
    manifest.identifier === channel.identifier &&
      manifest.renderer === "embedded",
    "Build manifest identity or renderer does not match OSIR distribution.",
  );
  requireThat(
    !Object.hasOwn(manifest, "installerArtifact"),
    "Build manifest already registers an installerArtifact; it cannot be overwritten.",
  );
  const architectures = manifest.architectures;
  requireThat(
    Array.isArray(architectures) &&
      architectures.length >= 1 &&
      architectures.length <= 2 &&
      new Set(architectures).size === architectures.length &&
      architectures.every((arch) => arch === "arm64" || arch === "x86_64"),
    "Build manifest has unsupported architectures.",
  );
  const derivedTarget =
    architectures.length === 2
      ? "universal-apple-darwin"
      : architectures[0] === "arm64"
        ? "aarch64-apple-darwin"
        : "x86_64-apple-darwin";
  requireThat(
    target === undefined || target === derivedTarget,
    "DMG target does not match the manifest architectures.",
  );
  const app = join(directory, channel.product + ".app");
  requireThat(
    lstatSync(app).isDirectory() &&
      !lstatSync(app).isSymbolicLink() &&
      contained(directory, realpathSync(app)),
    "App bundle must be inside the build directory.",
  );
  const name = `OSIR-Client-${manifest.channel}-${manifest.version}-${derivedTarget}.dmg`;
  const output = join(directory, name);
  let existing;
  try {
    existing = lstatSync(output);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  requireThat(
    !existing,
    "DMG output already exists; refusing to overwrite it.",
  );
  return {
    directory,
    app,
    name,
    output,
    product: channel.product,
    target: derivedTarget,
  };
}

async function defaultRun(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    input: options.input,
    maxBuffer: 8 * 1024 * 1024,
    env: { ...process.env, COPYFILE_DISABLE: "0" },
  });
  requireThat(
    !result.error && result.status === 0,
    `${basename(command)} failed: ${(result.error?.message || result.stderr || result.stdout || "unknown failure").trim()}`,
  );
  return result.stdout;
}

async function plistJson(run, value) {
  return JSON.parse(
    await run("/usr/bin/plutil", ["-convert", "json", "-o", "-", "-"], {
      input: value,
    }),
  );
}

async function verifyBundle(run, path, manifest, plan) {
  const plist = JSON.parse(
    await run("/usr/bin/plutil", [
      "-convert",
      "json",
      "-o",
      "-",
      join(path, "Contents/Info.plist"),
    ]),
  );
  requireThat(
    plist.CFBundleIdentifier === manifest.identifier &&
      plist.CFBundleShortVersionString === manifest.version &&
      (plist.CFBundleDisplayName || plist.CFBundleName) === plan.product,
    "App bundle identity/version does not match the build manifest.",
  );
  const executable = plist.CFBundleExecutable;
  requireThat(
    typeof executable === "string" &&
      executable.length > 0 &&
      basename(executable) === executable &&
      executable !== "." &&
      executable !== "..",
    "App bundle has an invalid executable path.",
  );
  const binary = join(path, "Contents/MacOS", executable);
  requireThat(
    lstatSync(binary).isFile() &&
      !lstatSync(binary).isSymbolicLink() &&
      (lstatSync(binary).mode & 0o111) !== 0,
    "App bundle executable is missing or invalid.",
  );
  const architectures = (await run("/usr/bin/lipo", ["-archs", binary]))
    .trim()
    .split(/\s+/)
    .sort();
  requireThat(
    JSON.stringify(architectures) ===
      JSON.stringify([...manifest.architectures].sort()),
    "App executable architectures do not match the build manifest.",
  );
  await run("/usr/bin/codesign", ["--verify", "--deep", "--strict", path]);
}

// The caller owns manifest publication. A build can call this before writing its
// final manifest, merge the returned fields, then recheck its source snapshot.
export async function buildDmg(
  { buildDirectory, manifest, target, adHocSign = true },
  { run = defaultRun } = {},
) {
  requireThat(typeof adHocSign === "boolean", "adHocSign must be a boolean.");
  const plan = validateDmgInput({ buildDirectory, manifest, target });
  const original = snapshotApp(plan.app);
  await verifyBundle(run, plan.app, manifest, plan);
  const work = mkdtempSync(join(plan.directory, ".osir-dmg-"));
  const payload = join(work, "payload");
  const mount = join(work, "mount");
  const image = join(work, "package.dmg");
  let detachTarget;
  let attachAttempted = false;
  let published = false;
  let complete = false;
  const detach = async () => {
    if (!attachAttempted) return;
    try {
      await run("/usr/bin/hdiutil", ["detach", detachTarget || mount]);
    } catch {
      await run("/usr/bin/hdiutil", [
        "detach",
        "-force",
        detachTarget || mount,
      ]);
    }
    attachAttempted = false;
  };
  try {
    mkdirSync(payload, { mode: 0o700 });
    mkdirSync(mount, { mode: 0o700 });
    const copiedApp = join(payload, basename(plan.app));
    await run("/usr/bin/ditto", [plan.app, copiedApp]);
    requireThat(
      snapshotApp(copiedApp) === original && snapshotApp(plan.app) === original,
      "App content changed while staging the DMG.",
    );
    symlinkSync("/Applications", join(payload, "Applications"));
    await run("/usr/bin/hdiutil", [
      "create",
      "-fs",
      "HFS+",
      "-format",
      "UDZO",
      "-volname",
      plan.product,
      "-srcfolder",
      payload,
      image,
    ]);
    if (adHocSign) {
      await run("/usr/bin/codesign", [
        "--force",
        "--sign",
        "-",
        "--timestamp=none",
        image,
      ]);
      await run("/usr/bin/codesign", ["--verify", "--strict", image]);
    }
    await run("/usr/bin/hdiutil", ["verify", image]);
    requireThat(
      (
        await run("/usr/bin/hdiutil", ["imageinfo", "-format", image])
      ).trim() === "UDZO",
      "DMG is not the expected read-only compressed UDZO format.",
    );
    attachAttempted = true;
    const attached = await plistJson(
      run,
      await run("/usr/bin/hdiutil", [
        "attach",
        "-readonly",
        "-nobrowse",
        "-noautoopen",
        "-mountpoint",
        mount,
        "-plist",
        image,
      ]),
    );
    const entity = attached["system-entities"]?.find(
      (entry) => entry["mount-point"] === mount,
    );
    requireThat(
      entity && /^\/dev\/disk\d+(?:s\d+)*$/.test(entity["dev-entry"]),
      "DMG did not mount at its private verification location.",
    );
    detachTarget = entity["dev-entry"];
    requireThat(
      JSON.stringify(readdirSync(mount).sort()) ===
        JSON.stringify(["Applications", basename(plan.app)].sort()),
      "DMG contains unexpected or missing items.",
    );
    requireThat(
      lstatSync(join(mount, "Applications")).isSymbolicLink() &&
        readlinkSync(join(mount, "Applications")) === "/Applications",
      "DMG Applications link is invalid.",
    );
    requireThat(
      snapshotApp(join(mount, basename(plan.app))) === original,
      "App content inside the DMG does not match the validated source.",
    );
    await run("/usr/bin/codesign", [
      "--verify",
      "--deep",
      "--strict",
      join(mount, basename(plan.app)),
    ]);
    await detach();
    requireThat(
      snapshotApp(plan.app) === original,
      "Source app changed during DMG packaging.",
    );
    const digest = fileDigest(image);
    requireThat(digest.bytes > 0, "DMG is empty.");
    // Hard-link publication is atomic and fails if an output appeared meanwhile.
    linkSync(image, plan.output);
    published = true;
    const result = {
      installerArtifact: { name: plan.name, ...digest },
      dmgVerification: {
        format: "UDZO",
        readOnly: true,
        checksumVerified: true,
        contentsVerified: true,
        applicationsLink: "/Applications",
        codeSignature: adHocSign ? "ad-hoc" : "unsigned",
        notarization: "not-notarized",
        sourceAppUnchanged: true,
      },
    };
    complete = true;
    return result;
  } finally {
    // Never remove a mount point while it remains mounted, even on failure.
    let detached = !attachAttempted;
    try {
      await detach();
      detached = true;
    } finally {
      if (detached) rmSync(work, { recursive: true, force: true });
      if (published && !complete) rmSync(plan.output, { force: true });
    }
  }
}

export async function runDmgCli(args, dependencies = {}) {
  const options = {};
  for (let index = 0; index < args.length; index += 1) {
    const key = args[index];
    requireThat(!Object.hasOwn(options, key), "Duplicate DMG option.");
    if (key === "--no-adhoc-sign") options[key] = true;
    else if (key === "--build-dir" || key === "--target") {
      requireThat(
        args[index + 1] && !args[index + 1].startsWith("--"),
        `Missing value for ${key}.`,
      );
      options[key] = args[++index];
    } else throw new Error(`Unknown DMG option: ${key}`);
  }
  requireThat(
    options["--build-dir"],
    "Usage: node scripts/osir-dmg.mjs --build-dir /absolute/build [--target target] [--no-adhoc-sign]",
  );
  const manifestPath = join(
    resolve(options["--build-dir"]),
    "build-manifest.json",
  );
  const { manifest, sha256 } = readManifest(manifestPath);
  const input = {
    buildDirectory: options["--build-dir"],
    manifest,
    target: options["--target"],
    adHocSign: !options["--no-adhoc-sign"],
  };
  const plan = validateDmgInput(input);
  const result = await buildDmg(input, dependencies);
  const temp = join(
    mkdtempSync(join(plan.directory, ".osir-dmg-manifest-")),
    "build-manifest.json",
  );
  let complete = false;
  try {
    writeFileSync(
      temp,
      JSON.stringify({ ...manifest, ...result }, null, 2) + "\n",
      { flag: "wx", mode: 0o600 },
    );
    requireThat(
      fileDigest(manifestPath, 1024 * 1024).sha256 === sha256,
      "Build manifest changed during DMG creation; refusing to replace it.",
    );
    renameSync(temp, manifestPath);
    complete = true;
    return result;
  } finally {
    rmSync(dirname(temp), { recursive: true, force: true });
    if (!complete) rmSync(plan.output, { force: true });
  }
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  runDmgCli(process.argv.slice(2))
    .then((result) => console.log(JSON.stringify(result, null, 2)))
    .catch((error) => {
      console.error("OSIR DMG: " + error.message);
      process.exitCode = 1;
    });
}
