// Publishes only verified, already-built OSIR release artifacts. This module
// never builds, signs, uploads files or reads credentials from a keychain.
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readFileSync,
  readSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";
import {
  loadUpdaterReleaseConfig,
  verifyUpdaterArtifact,
} from "./osir-release-updater.mjs";

const TRUSTED_ORIGIN = "https://osirclaw.com";
const CHANNELS = new Set(["beta", "prod"]);
const PLATFORMS = new Set([
  "darwin-aarch64",
  "darwin-x86_64",
  "windows-x86_64",
  "windows-aarch64",
  "linux-x86_64",
  "linux-aarch64",
]);
const MAX_ARTIFACT_BYTES = 2 * 1024 * 1024 * 1024;

function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

function readJson(path, maxBytes = 1024 * 1024) {
  const fd = openSync(
    path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  try {
    const stat = fstatSync(fd);
    requireThat(
      stat.isFile() && stat.size > 0 && stat.size <= maxBytes,
      "Release metadata must be a bounded regular file.",
    );
    const value = JSON.parse(readFileSync(fd, "utf8"));
    const after = fstatSync(fd);
    requireThat(
      stat.size === after.size &&
        stat.mtimeMs === after.mtimeMs &&
        stat.ctimeMs === after.ctimeMs,
      "Release metadata changed while reading.",
    );
    return value;
  } finally {
    closeSync(fd);
  }
}

export function inspectArtifact(path) {
  const absolute = resolve(path);
  const before = lstatSync(absolute);
  requireThat(
    before.isFile() &&
      !before.isSymbolicLink() &&
      before.size > 0 &&
      before.size <= MAX_ARTIFACT_BYTES,
    "Release artifacts must be bounded regular files, not symlinks.",
  );
  const fd = openSync(
    absolute,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  const digest = createHash("sha256");
  let bytes = 0;
  const buffer = Buffer.allocUnsafe(64 * 1024);
  try {
    const opened = fstatSync(fd);
    requireThat(
      opened.ino === before.ino && opened.dev === before.dev,
      "Release artifact changed before hashing.",
    );
    for (;;) {
      const size = readSync(fd, buffer, 0, buffer.length, null);
      if (!size) break;
      digest.update(buffer.subarray(0, size));
      bytes += size;
    }
    const after = fstatSync(fd);
    requireThat(
      bytes === before.size &&
        after.size === before.size &&
        after.mtimeMs === before.mtimeMs &&
        after.ctimeMs === before.ctimeMs,
      "Release artifact changed while hashing.",
    );
  } finally {
    closeSync(fd);
  }
  return { path: absolute, sha256: digest.digest("hex"), size_bytes: bytes };
}

export function createReleasePlan({
  channel,
  buildDirectory,
  updaterConfigPath,
  origin = TRUSTED_ORIGIN,
}) {
  requireThat(CHANNELS.has(channel), "Release channel must be beta or prod.");
  requireThat(origin === TRUSTED_ORIGIN, "Release API origin is not trusted.");
  const root = resolve(buildDirectory);
  requireThat(
    lstatSync(root).isDirectory() && !lstatSync(root).isSymbolicLink(),
    "Release build directory must not be a symlink.",
  );
  const manifest = readJson(join(root, "build-manifest.json"));
  if (manifest.mode === "release-multiplatform-distribution") {
    requireThat(
      Array.isArray(manifest.builds) &&
        manifest.builds.length > 0 &&
        manifest.builds.length <= 4,
      "Combined release must identify its verified platform builds.",
    );
    const plans = manifest.builds.map((build) => {
      requireThat(
        typeof build === "string" && /^[a-z0-9-]+$/.test(build),
        "Invalid platform build directory.",
      );
      const child = join(root, build);
      const childManifest = readJson(join(child, "build-manifest.json"));
      requireThat(
        childManifest.mode !== manifest.mode,
        "Nested combined releases are not supported.",
      );
      return createReleasePlan({
        channel,
        buildDirectory: child,
        updaterConfigPath,
        origin,
      });
    });
    const first = plans[0];
    requireThat(
      plans.every(
        (plan) =>
          plan.release_id === first.release_id &&
          plan.source_sha === first.source_sha &&
          plan.git_head === first.git_head,
      ),
      "Platform builds must identify the same release and source snapshot.",
    );
    const artifacts = plans.flatMap((plan) => plan.artifacts);
    requireThat(
      new Set(artifacts.map((a) => a.platform + ":" + a.kind)).size ===
        artifacts.length,
      "Combined release contains duplicate platforms.",
    );
    // Uploaders deliberately read one flat directory, never artifact.path.
    for (const artifact of artifacts) {
      const flat = inspectArtifact(join(root, artifact.file_name));
      requireThat(
        flat.sha256 === artifact.sha256 &&
          flat.size_bytes === artifact.size_bytes,
        "Combined release's flat upload bytes differ from verified platform build.",
      );
      artifact.path = flat.path;
    }
    return {
      ...first,
      artifacts,
      distribution_signature_verified: false,
      distribution_signing: "platform-verified-updater-signed",
      code_signature_verified: plans.every(
        (plan) => plan.code_signature_verified,
      ),
      notarization: "not-notarized",
      notes: [
        ...new Set(plans.flatMap((plan) => plan.notes.split(/\n\s*\n/))),
      ].join("\n\n"),
    };
  }
  const notarized =
    manifest.mode === "release-distribution" &&
    manifest.distributionSignatureVerified === true &&
    manifest.signing === "developer-id-notarized";
  const adhoc =
    manifest.mode === "release-adhoc-distribution" &&
    manifest.distributionSignatureVerified === false &&
    manifest.codeSignatureVerified === true &&
    manifest.notarization === "not-notarized" &&
    manifest.signing === "ad-hoc-not-notarized";
  const selfSigned =
    manifest.mode === "release-self-signed-distribution" &&
    manifest.distributionSignatureVerified === false &&
    manifest.codeSignatureVerified === true &&
    manifest.notarization === "not-notarized" &&
    manifest.signing === "local-certificate-not-notarized" &&
    /^[A-F0-9]{40}$/.test(manifest.localSigningIdentity ?? "") &&
    typeof manifest.localSigningRequirement === "string" &&
    !manifest.localSigningRequirement.includes("cdhash");
  const windows =
    manifest.mode === "release-windows-distribution" &&
    manifest.signing === "windows-unsigned-updater-signed" &&
    manifest.distributionSignatureVerified === false &&
    manifest.codeSignatureVerified === false &&
    manifest.notarization === "not-applicable" &&
    manifest.peArchitectureVerified === true &&
    ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"].includes(
      manifest.target,
    );
  requireThat(
    manifest.channel === channel &&
      (notarized || adhoc || selfSigned || windows) &&
      manifest.updaterEnabled === true &&
      manifest.updaterRuntimeConfigured === true &&
      manifest.updaterUpgradeAcceptance === "not-run" &&
      manifest.sourceProvenance?.verifiedAtEnd === true &&
      /^[a-f0-9]{64}$/.test(manifest.sourceProvenance?.sourceSha256 ?? "") &&
      /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(
        manifest.sourceProvenance?.git?.head ?? "",
      ),
    "Build manifest is not an explicit verified OSIR distribution with signed updates.",
  );
  requireThat(
    /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version) &&
      (channel !== "prod" || !manifest.version.includes("-")),
    "Build version is invalid for the selected channel.",
  );
  requireThat(
    manifest.architectures?.length > 0 &&
      manifest.architectures.every((architecture) =>
        ["arm64", "x86_64"].includes(architecture),
      ),
    "Build manifest has no supported macOS architecture.",
  );
  const input = loadUpdaterReleaseConfig(resolve(updaterConfigPath), {
    channel,
  });
  requireThat(
    input.sha256 === manifest.updaterConfigSha256 &&
      input.config.endpoint ===
        `${origin}/api/v1/osir-client/updater/${channel}.json`,
    "Public updater configuration is not the one embedded in this cloud release.",
  );
  const feed = readJson(join(root, `updater-${channel}.json`));
  requireThat(
    feed.version === manifest.version &&
      feed.platforms &&
      Object.keys(feed.platforms).length > 0 &&
      Object.values(feed.platforms).every(
        (item) =>
          typeof item.signature === "string" &&
          typeof item.url === "string" &&
          new URL(item.url).origin === origin,
      ),
    "Generated updater manifest does not match this signed release.",
  );
  const releaseId = `${channel}-${manifest.version}`;
  requireThat(
    /^[a-z0-9][a-z0-9.-]{0,95}$/.test(releaseId),
    "Release ID is invalid.",
  );
  const artifacts = [];
  const expectedPlatforms = manifest.architectures.map(
    (architecture) =>
      (windows ? "windows-" : "darwin-") +
      (architecture === "arm64" ? "aarch64" : "x86_64"),
  );
  requireThat(
    JSON.stringify(Object.keys(feed.platforms).sort()) ===
      JSON.stringify(expectedPlatforms.sort()),
    "Updater platforms do not match the signed app architectures.",
  );
  const buildTarget = windows
    ? manifest.target
    : manifest.architectures.length === 2
      ? "universal-apple-darwin"
      : manifest.architectures[0] === "arm64"
        ? "aarch64-apple-darwin"
        : "x86_64-apple-darwin";
  for (const [target, entries] of Object.entries(feed.platforms)) {
    requireThat(
      PLATFORMS.has(target),
      "Updater manifest contains an unsupported platform.",
    );
    requireThat(
      entries && Object.keys(entries).sort().join(",") === "signature,url",
      "Updater entry has unexpected fields.",
    );
    const updaterName = basename(new URL(entries.url).pathname);
    const updaterPath = join(root, updaterName);
    requireThat(
      updaterName ===
        `OSIR-Client-${channel}-${manifest.version}-${buildTarget}${windows ? "-setup.exe" : ".app.tar.gz"}`,
      "Updater artifact name does not match its platform.",
    );
    const updater = inspectArtifact(updaterPath);
    const signaturePath = updaterPath + ".sig";
    const signatureStat = lstatSync(signaturePath);
    requireThat(
      signatureStat.isFile() &&
        !signatureStat.isSymbolicLink() &&
        signatureStat.size <= 16384,
      "Updater signature must be a small regular file.",
    );
    const signature = readFileSync(signaturePath, "utf8").trim();
    requireThat(
      signature === entries.signature,
      "Updater signature differs from generated manifest.",
    );
    const verified = verifyUpdaterArtifact(
      updaterPath,
      signature,
      input.config.pubkey,
    );
    requireThat(
      verified.sha256 === updater.sha256 &&
        verified.bytes === updater.size_bytes &&
        manifest.updaterArtifact?.sha256 === updater.sha256 &&
        manifest.updaterArtifact?.bytes === updater.size_bytes &&
        manifest.updaterArtifact?.signatureVerified === true,
      "Updater artifact differs from its verified build manifest.",
    );
    artifacts.push({
      platform: target,
      kind: "updater",
      file_name: updaterName,
      signature,
      ...updater,
      url: `${origin}/api/v1/osir-client/artifacts/${encodeURIComponent(releaseId)}/${encodeURIComponent(updaterName)}`,
    });
    const installerStem = `OSIR-Client-${channel}-${manifest.version}-${buildTarget}`;
    const installerName = manifest.installerArtifact?.name;
    requireThat(
      (windows
        ? [installerStem + "-setup.exe"]
        : [installerStem + ".zip", installerStem + ".dmg"]
      ).includes(installerName) &&
        (!(adhoc || selfSigned) || installerName.endsWith(".dmg")),
      "Installer must be the matching distribution archive; ad-hoc distribution requires a DMG.",
    );
    const installerPath = join(root, installerName);
    const installer = inspectArtifact(installerPath);
    requireThat(
      manifest.installerArtifact?.name === installerName &&
        manifest.installerArtifact?.sha256 === installer.sha256 &&
        manifest.installerArtifact?.bytes === installer.size_bytes,
      "Installer archive differs from its verified build manifest.",
    );
    artifacts.push({
      platform: target,
      kind: "installer",
      file_name: installerName,
      ...installer,
      url: `${origin}/api/v1/osir-client/artifacts/${encodeURIComponent(releaseId)}/${encodeURIComponent(installerName)}`,
    });
  }
  const uniqueArtifacts = new Set(
    artifacts.map((artifact) => `${artifact.platform}:${artifact.kind}`),
  );
  requireThat(
    uniqueArtifacts.size === artifacts.length,
    "Duplicate platform artifact in release plan.",
  );
  return {
    schema_version: 1,
    release_id: releaseId,
    version: manifest.version,
    channel,
    source_sha: manifest.sourceProvenance.sourceSha256,
    git_head: manifest.sourceProvenance.git.head,
    git_dirty: manifest.sourceProvenance.git.dirtyAtStart,
    distribution_signature_verified: notarized,
    distribution_signing: manifest.signing,
    code_signature_verified: !windows,
    notarization: windows
      ? "not-applicable"
      : notarized
        ? "stapled-verified"
        : "not-notarized",
    updater_signature_verified: true,
    updater_acceptance: "not-run",
    notes:
      (adhoc || selfSigned
        ? "此版本未经 Apple 公证。首次打开若被 macOS 阻止，请前往“系统设置 → 隐私与安全”，允许打开。\n\n"
        : windows
          ? "此 Windows 安装包未使用 Authenticode 签名；系统可能提示未知发布者。\n\n"
          : "") + input.config.notes,
    artifacts,
  };
}

async function requestJson(
  fetchImpl,
  url,
  token,
  { method = "GET", body, timeoutMs = 15000 } = {},
) {
  requireThat(
    new URL(url).origin === TRUSTED_ORIGIN,
    "Release API origin is not trusted.",
  );
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetchImpl(url, {
      method,
      redirect: "error",
      signal: controller.signal,
      headers: {
        accept: "application/json",
        ...(token ? { authorization: `Bearer ${token}` } : {}),
        ...(body ? { "content-type": "application/json" } : {}),
      },
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
    const chunks = [];
    let bytes = 0;
    for await (const chunk of response.body ?? []) {
      bytes += chunk.length;
      requireThat(
        bytes <= 128 * 1024,
        "Release API response exceeded its size limit.",
      );
      chunks.push(chunk);
    }
    const text = Buffer.concat(chunks).toString("utf8");
    let data;
    try {
      data = text ? JSON.parse(text) : undefined;
    } catch {
      throw new Error("Release API returned invalid JSON.");
    }
    if (!response.ok) {
      const code = typeof data?.code === "number" ? data.code : response.status;
      throw new Error(
        `Release API rejected the request (HTTP ${response.status}, code ${code}).`,
      );
    }
    return { status: response.status, data };
  } finally {
    clearTimeout(timer);
  }
}

function dataOf(envelope) {
  if (
    envelope?.code !== 0 ||
    !envelope.data ||
    typeof envelope.data !== "object"
  )
    throw new Error(
      "Release API response did not match the expected contract.",
    );
  return envelope.data;
}

async function verifyPublicArtifact(fetchImpl, artifact) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 180000);
  try {
    const response = await fetchImpl(artifact.url, {
      method: "GET",
      redirect: "error",
      signal: controller.signal,
      headers: { "accept-encoding": "identity" },
    });
    requireThat(
      response.ok && response.url === artifact.url,
      `Public artifact readback failed for ${artifact.file_name}.`,
    );
    const hash = createHash("sha256");
    let bytes = 0;
    for await (const chunk of response.body) {
      bytes += chunk.length;
      requireThat(
        bytes <= MAX_ARTIFACT_BYTES && bytes <= artifact.size_bytes,
        `Public artifact size differs for ${artifact.file_name}.`,
      );
      hash.update(chunk);
    }
    requireThat(
      bytes === artifact.size_bytes && hash.digest("hex") === artifact.sha256,
      `Public artifact hash differs for ${artifact.file_name}.`,
    );
  } finally {
    clearTimeout(timer);
  }
}

export function createServiceReleaseMetadata(plan) {
  const body = {
    release_id: plan.release_id,
    version: plan.version,
    channel: plan.channel,
    source_sha: plan.source_sha,
    notes: plan.notes,
    artifacts: plan.artifacts.map(
      ({ platform, kind, url, sha256, size_bytes, signature }) => ({
        platform,
        kind,
        url,
        sha256,
        size_bytes,
        ...(signature ? { signature } : {}),
      }),
    ),
  };
  return {
    ...body,
    idempotency_key: createHash("sha256")
      .update(JSON.stringify(body))
      .digest("hex"),
  };
}

export async function verifyPublicReleaseArtifacts(
  plan,
  { fetchImpl = globalThis.fetch, metadataPath } = {},
) {
  let metadataDigest;
  if (metadataPath) {
    const metadata = inspectArtifact(metadataPath);
    requireThat(
      metadata.size_bytes <= 65536,
      "Service release metadata exceeds its size limit.",
    );
    requireThat(
      isDeepStrictEqual(
        readJson(metadataPath, 65536),
        createServiceReleaseMetadata(plan),
      ),
      "Service release metadata differs from the verified build plan.",
    );
    requireThat(
      inspectArtifact(metadataPath).sha256 === metadata.sha256,
      "Service metadata changed while validating.",
    );
    metadataDigest = metadata.sha256;
  }
  const unique = new Map();
  for (const artifact of plan.artifacts) {
    const previous = unique.get(artifact.url);
    requireThat(
      !previous ||
        (previous.sha256 === artifact.sha256 &&
          previous.size_bytes === artifact.size_bytes),
      "Conflicting public artifact records for the same URL.",
    );
    unique.set(artifact.url, artifact);
  }
  for (const artifact of unique.values())
    await verifyPublicArtifact(fetchImpl, artifact);
  return {
    schema_version: 1,
    release_id: plan.release_id,
    ...(metadataDigest ? { metadata_sha256: metadataDigest } : {}),
    updater_signature_verified: true,
    readback_verified: true,
    artifacts: plan.artifacts.map(({ url, sha256, size_bytes }) => ({
      url,
      sha256,
      size_bytes,
      verified: true,
    })),
    verified_at: new Date().toISOString(),
  };
}

async function readCurrent(fetchImpl, channel) {
  const response = await requestJson(
    fetchImpl,
    `${TRUSTED_ORIGIN}/api/v1/osir-client/releases/${channel}`,
    undefined,
  );
  if (
    response.status === 204 ||
    (response.data?.code === 0 && response.data?.data === null)
  )
    return null;
  return dataOf(response.data).release_id;
}

export async function runReleaseAction(
  plan,
  { action, token, expectedCurrent, fetchImpl = globalThis.fetch },
) {
  const { channel } = plan;
  requireThat(
    typeof token === "string" &&
      token.length >= 16 &&
      token.length <= 8192 &&
      !/[\r\n]/.test(token),
    "Set OSIR_CLIENT_ADMIN_TOKEN in the current secure shell session.",
  );
  if (action === "register" || action === "draft") {
    for (const artifact of plan.artifacts) {
      const local = inspectArtifact(artifact.path);
      requireThat(
        local.sha256 === artifact.sha256 &&
          local.size_bytes === artifact.size_bytes,
        `Artifact changed after planning: ${artifact.file_name}.`,
      );
    }
    const apiArtifacts = plan.artifacts.map(
      ({ platform, kind, url, sha256, size_bytes, signature }) => ({
        platform,
        kind,
        url,
        sha256,
        size_bytes,
        ...(signature ? { signature } : {}),
      }),
    );
    const body = {
      release_id: plan.release_id,
      version: plan.version,
      channel,
      source_sha: plan.source_sha,
      notes: plan.notes,
      artifacts: apiArtifacts,
    };
    const created = await requestJson(
      fetchImpl,
      `${TRUSTED_ORIGIN}/api/v1/admin/osir-client/releases`,
      token,
      {
        method: "POST",
        body: {
          ...body,
          idempotency_key: createHash("sha256")
            .update(JSON.stringify(body))
            .digest("hex"),
        },
      },
    );
    const draft = dataOf(created.data);
    requireThat(
      draft.release_id === plan.release_id && draft.status === "draft",
      "Release API did not return the expected draft.",
    );
    assertMatchingDraft(draft, plan);
    if (action === "draft") {
      for (const artifact of plan.artifacts)
        await verifyPublicArtifact(fetchImpl, artifact);
    }
    return {
      release_id: plan.release_id,
      status: "draft",
      readback_verified: action === "draft",
    };
  }
  requireThat(
    ["verify", "publish"].includes(action),
    "Unknown release operation.",
  );
  const releaseResponse = await requestJson(
    fetchImpl,
    `${TRUSTED_ORIGIN}/api/v1/admin/osir-client/releases/${encodeURIComponent(plan.release_id)}`,
    token,
  );
  const draft = dataOf(releaseResponse.data);
  requireThat(
    draft.status === "draft",
    "The draft changed or is no longer publishable.",
  );
  assertMatchingDraft(draft, plan);
  for (const artifact of plan.artifacts)
    await verifyPublicArtifact(fetchImpl, artifact);
  if (action === "verify")
    return {
      release_id: plan.release_id,
      status: "draft",
      readback_verified: true,
    };
  requireThat(
    expectedCurrent !== undefined,
    "Publish requires the reviewed expected-current ID; pass 'none' when the channel has no release.",
  );
  const expected = expectedCurrent === "none" ? null : expectedCurrent;
  requireThat(
    expected === null || /^[a-z0-9][a-z0-9.-]{0,95}$/.test(expected),
    "Expected-current release ID is invalid.",
  );
  const actual = await readCurrent(fetchImpl, channel);
  requireThat(
    actual === expected,
    "Published channel pointer changed. Review it and recreate the publish request with the current release ID.",
  );
  const published = await requestJson(
    fetchImpl,
    `${TRUSTED_ORIGIN}/api/v1/admin/osir-client/releases/${encodeURIComponent(plan.release_id)}/publish`,
    token,
    {
      method: "POST",
      body: { expected_current_release_id: expected },
    },
  );
  const publishedRelease = dataOf(published.data);
  requireThat(
    publishedRelease.status === "published" &&
      publishedRelease.release_id === plan.release_id,
    "Publish response did not confirm this release; reconcile cloud state before retrying.",
  );
  try {
    requireThat(
      (await readCurrent(fetchImpl, channel)) === plan.release_id,
      "Current release pointer differs.",
    );
    const feedResponse = await fetchImpl(
      `${TRUSTED_ORIGIN}/api/v1/osir-client/updater/${channel}.json`,
      { method: "GET", redirect: "error", signal: AbortSignal.timeout(15000) },
    );
    requireThat(feedResponse.ok, "Updater feed is unavailable.");
    const text = await feedResponse.text();
    requireThat(
      text.length < 128 * 1024,
      "Updater feed exceeded its size limit.",
    );
    const feed = JSON.parse(text);
    const updaters = plan.artifacts.filter(
      (artifact) => artifact.kind === "updater",
    );
    requireThat(
      feed.version === plan.version &&
        feed.platforms &&
        Object.keys(feed.platforms).length === updaters.length &&
        updaters.every(
          (artifact) =>
            feed.platforms[artifact.platform]?.signature ===
              artifact.signature &&
            feed.platforms[artifact.platform]?.url ===
              `${TRUSTED_ORIGIN}/api/v1/osir-client/download/${plan.release_id}/${artifact.platform}/updater`,
        ),
      "Updater feed differs from the published release.",
    );
  } catch {
    throw new Error(
      "Publish was submitted, but public current/updater readback was not confirmed. Reconcile cloud state; do not automatically republish.",
    );
  }
  return {
    release_id: plan.release_id,
    status: "published",
    readback_verified: true,
  };
}

function assertMatchingDraft(draft, plan) {
  const fields = ["release_id", "version", "channel", "source_sha", "notes"];
  requireThat(
    fields.every((field) => draft[field] === plan[field]),
    "Cloud draft differs from the local release plan.",
  );
  const normalize = (artifacts) =>
    artifacts
      .map(({ platform, kind, url, sha256, size_bytes, signature }) =>
        JSON.stringify([
          platform,
          kind,
          url,
          sha256,
          size_bytes,
          signature ?? "",
        ]),
      )
      .sort();
  requireThat(
    Array.isArray(draft.artifacts) &&
      JSON.stringify(normalize(draft.artifacts)) ===
        JSON.stringify(normalize(plan.artifacts)),
    "Cloud draft artifact bindings differ from the local release plan.",
  );
}

async function main(args) {
  const [action, channel, buildDirectory, updaterConfigPath, expectedCurrent] =
    args;
  requireThat(
    args.length <= 5 &&
      [
        "plan",
        "metadata",
        "register",
        "verify",
        "verify-public",
        "draft",
        "publish",
      ].includes(action),
    "Usage: node scripts/osir-release-publisher.mjs <plan|metadata|register|verify|verify-public|draft|publish> <beta|prod> <build-directory> <public-updater-config> [expected-current-release-id|none; or metadata.json for verify-public]",
  );
  const plan = createReleasePlan({
    channel,
    buildDirectory,
    updaterConfigPath,
  });
  const result =
    action === "plan"
      ? plan
      : action === "metadata"
        ? createServiceReleaseMetadata(plan)
        : action === "verify-public"
          ? await verifyPublicReleaseArtifacts(plan, {
              metadataPath: expectedCurrent,
            })
          : await runReleaseAction(plan, {
              action,
              token: process.env.OSIR_CLIENT_ADMIN_TOKEN,
              expectedCurrent,
            });
  process.stdout.write(JSON.stringify(result, null, 2) + "\n");
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`OSIR release failed: ${error.message}\n`);
    process.exitCode = 1;
  });
}
