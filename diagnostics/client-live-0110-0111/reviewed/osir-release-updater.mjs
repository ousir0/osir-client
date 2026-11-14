// Explicit OSIR release updater inputs and artifact verification. No network,
// key generation, signing, Keychain access, publishing or app installation.
import {
  accessSync,
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readSync,
} from "node:fs";
import { createHash, createPublicKey, verify } from "node:crypto";
import { isAbsolute } from "node:path";
import { isIP } from "node:net";

function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

function base64(value, message) {
  requireThat(
    typeof value === "string" &&
      value.length > 0 &&
      value.length <= 16384 &&
      value.length % 4 === 0 &&
      /^[A-Za-z0-9+/]+={0,2}$/.test(value),
    message,
  );
  const decoded = Buffer.from(value, "base64");
  requireThat(decoded.toString("base64") === value, message);
  return decoded;
}

export function decodeUpdaterPublicKey(value) {
  const message =
    "OSIR updater public key must be a valid Tauri/minisign public key.";
  const text = base64(value, message).toString("utf8");
  const lines = text.trimEnd().split(/\r?\n/);
  requireThat(
    lines.length === 2 && lines[0].startsWith("untrusted comment: "),
    message,
  );
  const bytes = base64(lines[1], message);
  requireThat(
    bytes.length === 42 &&
      ["Ed", "ED"].includes(bytes.subarray(0, 2).toString("ascii")),
    message,
  );
  return bytes;
}

function publicHttpsUrl(value, { directory = false } = {}) {
  const message =
    "OSIR updater URLs must be public HTTPS URLs without credentials, query, fragment or upstream CC Switch sources.";
  requireThat(
    typeof value === "string" &&
      value.length <= 2048 &&
      value.trim() === value &&
      !/[\x00-\x20\x7f\\{}]/.test(value),
    message,
  );
  let url;
  try {
    url = new URL(value);
  } catch {
    throw new Error(message);
  }
  const host = url.hostname.toLowerCase();
  let upstreamPath;
  try {
    upstreamPath = decodeURIComponent(url.pathname).toLowerCase();
  } catch {
    throw new Error(message);
  }
  requireThat(
    url.protocol === "https:" &&
      !url.username &&
      !url.password &&
      !url.search &&
      !url.hash &&
      (!url.port || url.port === "443") &&
      !isIP(host.replace(/^\[|\]$/g, "")) &&
      host.includes(".") &&
      !["localhost", "ccswitch.io"].includes(host) &&
      !host.endsWith(".") &&
      !host.endsWith(".localhost") &&
      !host.endsWith(".local") &&
      !host.endsWith(".internal") &&
      !host.endsWith(".ccswitch.io") &&
      !(
        [
          "github.com",
          "raw.githubusercontent.com",
          "objects.githubusercontent.com",
        ].includes(host) && upstreamPath.includes("/farion1231/cc-switch")
      ),
    message,
  );
  requireThat(
    directory || !url.pathname.endsWith("/"),
    "OSIR updater endpoint must identify a manifest.",
  );
  if (directory) url.pathname = url.pathname.replace(/\/+$/, "") + "/";
  return url.toString();
}

export function validateUpdaterReleaseConfig(
  value,
  { channel, upstreamPublicKey } = {},
) {
  requireThat(
    channel === "beta" || channel === "prod",
    "OSIR updater is only available for beta/prod release builds.",
  );
  const keys = [
    "schemaVersion",
    "channel",
    "pubkey",
    "endpoint",
    "downloadBaseUrl",
    "notes",
  ];
  requireThat(
    value &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      Object.keys(value).every((key) => keys.includes(key)) &&
      value.schemaVersion === 1 &&
      value.channel === channel,
    "OSIR updater public configuration has unknown fields, wrong schema or wrong channel.",
  );
  const key = decodeUpdaterPublicKey(value.pubkey);
  if (upstreamPublicKey)
    requireThat(
      !key
        .subarray(10)
        .equals(decodeUpdaterPublicKey(upstreamPublicKey).subarray(10)),
      "OSIR updater must not reuse the upstream CC Switch public key.",
    );
  const endpoint = publicHttpsUrl(value.endpoint);
  const downloadBaseUrl = publicHttpsUrl(value.downloadBaseUrl, {
    directory: true,
  });
  requireThat(
    value.notes === undefined ||
      (typeof value.notes === "string" &&
        value.notes.length <= 8192 &&
        !/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(value.notes)),
    "OSIR updater notes are invalid.",
  );
  return Object.freeze({
    schemaVersion: 1,
    channel,
    pubkey: value.pubkey,
    endpoint,
    downloadBaseUrl,
    notes: value.notes ?? "",
  });
}

function readPublicConfig(path) {
  requireThat(
    typeof path === "string" &&
      isAbsolute(path) &&
      !/[\x00-\x1f\x7f]/.test(path),
    "OSIR updater config requires an absolute path.",
  );
  const descriptor = openSync(
    path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  try {
    const stat = fstatSync(descriptor);
    requireThat(
      stat.isFile() && stat.size > 0 && stat.size <= 16384,
      "OSIR updater config must be a small regular public JSON file.",
    );
    const buffer = Buffer.alloc(16385);
    const size = readSync(descriptor, buffer, 0, buffer.length, null);
    const after = fstatSync(descriptor);
    requireThat(
      size === stat.size &&
        size <= 16384 &&
        after.size === stat.size &&
        after.mtimeMs === stat.mtimeMs &&
        after.ctimeMs === stat.ctimeMs,
      "OSIR updater config changed while reading.",
    );
    return buffer.subarray(0, size).toString("utf8");
  } finally {
    closeSync(descriptor);
  }
}

export function loadUpdaterReleaseConfig(path, options) {
  const text = readPublicConfig(path);
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error("OSIR updater public config is not valid JSON.");
  }
  return {
    config: validateUpdaterReleaseConfig(value, options),
    sha256: createHash("sha256").update(text).digest("hex"),
  };
}

export function verifyUpdaterConfigUnchanged(path, expected) {
  requireThat(
    createHash("sha256").update(readPublicConfig(path)).digest("hex") ===
      expected,
    "OSIR updater config changed during packaging; no output is approved.",
  );
}

export function updaterConfigPatch(config) {
  if (!config)
    return {
      bundle: { createUpdaterArtifacts: false },
      plugins: { updater: { pubkey: "", endpoints: [] }, "osir-updates": null },
    };
  return {
    bundle: { createUpdaterArtifacts: true },
    plugins: {
      updater: { pubkey: config.pubkey, endpoints: [config.endpoint] },
      // Tauri's generated BundleConfig drops createUpdaterArtifacts at runtime.
      // This public compiled marker is injected only with the validated opt-in.
      "osir-updates": {
        schemaVersion: 1,
        channel: config.channel,
        bundleArtifacts: true,
      },
    },
  };
}

export function requireUpdaterSigner(env) {
  const key = env.TAURI_SIGNING_PRIVATE_KEY;
  requireThat(
    typeof key === "string" && key.trim().length > 0,
    "OSIR updater requires TAURI_SIGNING_PRIVATE_KEY in the native build environment.",
  );
  requireThat(
    !env.TAURI_SIGNING_PRIVATE_KEY_PATH &&
      !env.TAURI_PRIVATE_KEY &&
      !env.TAURI_KEY_PASSWORD &&
      !env.TAURI_PRIVATE_KEY_PATH,
    "OSIR updater accepts only the current TAURI_SIGNING_PRIVATE_KEY method, not mixed or legacy signing inputs.",
  );
  // Tauri accepts either the encoded private key or a key-file path in this
  // variable. Inspect only path metadata; never read private-key contents here.
  if (isAbsolute(key)) {
    const stat = lstatSync(key);
    requireThat(
      stat.isFile() && !stat.isSymbolicLink() && (stat.mode & 0o077) === 0,
      "OSIR updater private-key file must be a private regular file.",
    );
    accessSync(key, constants.R_OK);
  }
}

export function packagingEnvironments(
  env,
  { release, updater, appleSigning = release } = {},
) {
  const native = { ...env };
  delete native.TAURI_CONFIG;
  for (const key of Object.keys(native)) {
    if (
      key.startsWith("TAURI_SIGNING_") &&
      !(
        release &&
        updater &&
        [
          "TAURI_SIGNING_PRIVATE_KEY",
          "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
        ].includes(key)
      )
    )
      delete native[key];
    if (
      key.startsWith("TAURI_PRIVATE_") ||
      key === "TAURI_KEY_PASSWORD" ||
      (!appleSigning && key.startsWith("APPLE_"))
    )
      delete native[key];
  }
  const renderer = { ...native };
  for (const key of Object.keys(renderer)) {
    if (
      key.startsWith("APPLE_") ||
      key.startsWith("TAURI_SIGNING_") ||
      key.startsWith("TAURI_PRIVATE_") ||
      key === "TAURI_KEY_PASSWORD"
    )
      delete renderer[key];
  }
  return { native, renderer };
}

export function verifyUpdaterArtifact(path, signatureValue, pubkey) {
  const message =
    "OSIR updater artifact signature is invalid or does not match its public key.";
  const key = decodeUpdaterPublicKey(pubkey);
  const lines = base64(signatureValue, message)
    .toString("utf8")
    .trimEnd()
    .split(/\r?\n/);
  requireThat(
    lines.length === 4 &&
      lines[0].startsWith("untrusted comment: ") &&
      lines[2].startsWith("trusted comment: "),
    message,
  );
  const signed = base64(lines[1], message);
  const global = base64(lines[3], message);
  // Current Tauri emits prehashed minisign ED signatures. Legacy unbounded
  // non-prehashed Ed payloads are intentionally outside this release path.
  requireThat(
    signed.length === 74 &&
      signed.subarray(0, 2).toString("ascii") === "ED" &&
      global.length === 64 &&
      signed.subarray(2, 10).equals(key.subarray(2, 10)),
    message,
  );
  const publicKey = createPublicKey({
    key: Buffer.concat([
      Buffer.from("302a300506032b6570032100", "hex"),
      key.subarray(10),
    ]),
    format: "der",
    type: "spki",
  });
  requireThat(
    verify(
      null,
      Buffer.concat([signed.subarray(10), Buffer.from(lines[2].slice(17))]),
      publicKey,
      global,
    ),
    message,
  );
  const descriptor = openSync(
    path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  const digest = createHash("blake2b512");
  const sha256 = createHash("sha256");
  let total = 0;
  try {
    const before = fstatSync(descriptor);
    requireThat(
      before.isFile() && before.size > 0,
      "OSIR updater payload must be a nonempty regular file.",
    );
    const chunk = Buffer.allocUnsafe(64 * 1024);
    for (;;) {
      const size = readSync(descriptor, chunk, 0, chunk.length, null);
      if (!size) break;
      digest.update(chunk.subarray(0, size));
      sha256.update(chunk.subarray(0, size));
      total += size;
    }
    const after = fstatSync(descriptor);
    requireThat(
      before.size === total &&
        before.size === after.size &&
        before.mtimeMs === after.mtimeMs &&
        before.ctimeMs === after.ctimeMs,
      "OSIR updater payload changed during verification.",
    );
  } finally {
    closeSync(descriptor);
  }
  requireThat(
    verify(null, digest.digest(), publicKey, signed.subarray(10)),
    message,
  );
  return {
    bytes: total,
    sha256: sha256.digest("hex"),
    signatureVerified: true,
  };
}

export function createUpdaterManifest({
  config,
  version,
  target,
  artifactName,
  signature,
  pubDate,
}) {
  requireThat(
    typeof version === "string" &&
      /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version),
    "OSIR updater version must be semver.",
  );
  requireThat(
    config.channel !== "prod" || !version.includes("-"),
    "Production updater manifests cannot promote a prerelease version.",
  );
  const platforms = {
    "aarch64-apple-darwin": ["darwin-aarch64"],
    "x86_64-apple-darwin": ["darwin-x86_64"],
    "universal-apple-darwin": ["darwin-aarch64", "darwin-x86_64"],
    "x86_64-pc-windows-msvc": ["windows-x86_64"],
    "aarch64-pc-windows-msvc": ["windows-aarch64"],
  }[target];
  requireThat(platforms, "OSIR updater target is unsupported.");
  requireThat(
    artifactName ===
      `OSIR-Client-${config.channel}-${version}-${target}${target.endsWith("windows-msvc") ? "-setup.exe" : ".app.tar.gz"}`,
    "OSIR updater artifact name does not match channel/version/target.",
  );
  requireThat(
    typeof pubDate === "string" && !Number.isNaN(Date.parse(pubDate)),
    "OSIR updater publication date is invalid.",
  );
  const url = new URL(
    `${config.channel}/${encodeURIComponent(version)}/${encodeURIComponent(artifactName)}`,
    config.downloadBaseUrl,
  ).toString();
  return {
    version,
    notes: config.notes,
    pub_date: pubDate,
    platforms: Object.fromEntries(
      platforms.map((platform) => [platform, { signature, url }]),
    ),
  };
}
