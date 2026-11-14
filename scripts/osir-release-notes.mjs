// Reviewed publication copy is independent of immutable build/updater inputs.
import assert from "node:assert/strict";
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { isAbsolute, join } from "node:path";
import {
  createReleasePlan,
  inspectArtifact,
} from "./osir-release-publisher.mjs";
import { loadUpdaterReleaseConfig } from "./osir-release-updater.mjs";

export const notesDigest = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");
function sorted(value) {
  if (Array.isArray(value)) return value.map(sorted);
  if (value && typeof value === "object")
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, sorted(value[key])]),
    );
  return value;
}
// Artifact.path is the sole location-dependent field; retain all other fields,
// including notes, updater signatures, signing limitations and evidence audit.
export function releasePlanFingerprint(plan) {
  return notesDigest(
    JSON.stringify(
      sorted({
        ...plan,
        artifacts: plan.artifacts.map(({ path, ...artifact }) => artifact),
      }),
    ),
  );
}

export function readReviewedNotes(binding) {
  assert.deepEqual(
    Object.keys(binding ?? {}).sort(),
    ["path", "sha256"],
    "notesOverride requires exactly path and sha256",
  );
  assert.ok(isAbsolute(binding.path), "notesOverride path must be absolute");
  assert.match(binding.sha256 ?? "", /^[a-f0-9]{64}$/);
  const stat = lstatSync(binding.path);
  assert.ok(
    stat.isFile() &&
      !stat.isSymbolicLink() &&
      stat.nlink === 1 &&
      stat.size > 0 &&
      stat.size <= 65536,
    "Notes override must be a bounded ordinary file",
  );
  const fd = openSync(
    binding.path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  try {
    const before = fstatSync(fd);
    assert.ok(
      before.isFile() &&
        before.nlink === 1 &&
        before.ino === stat.ino &&
        before.dev === stat.dev &&
        before.size === stat.size,
      "Notes override changed before reading",
    );
    const raw = readFileSync(fd),
      after = fstatSync(fd);
    assert.ok(
      raw.length === before.size &&
        before.size === after.size &&
        before.mtimeMs === after.mtimeMs &&
        before.ctimeMs === after.ctimeMs,
      "Notes override changed while reading",
    );
    assert.equal(
      notesDigest(raw),
      binding.sha256,
      "Reviewed override raw hash differs",
    );
    return {
      raw,
      value: JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(raw)),
    };
  } finally {
    closeSync(fd);
  }
}

// Reuse the same manifest-declared evidence scope as combine/backup. This binds
// existing native evidence bytes; it does not claim new OS or account coverage.
function verifyEvidence(root) {
  const manifestPath = join(root, "build-manifest.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (manifest.mode === "release-multiplatform-distribution")
    return manifest.builds.flatMap((name) =>
      verifyEvidence(join(root, name)).map((item) => ({
        ...item,
        path: name + "/" + item.path,
      })),
    );
  const native =
    manifest.packagedMacAcceptance ?? manifest.packagedLifecycleReport;
  assert.ok(
    native,
    "Notes override requires original native acceptance evidence",
  );
  if (!manifest.mode.includes("windows"))
    assert.ok(
      manifest.nativeAcceptance,
      "Mac native acceptance binding is required",
    );
  const records = [
    manifest.mainExecutableArtifact,
    manifest.packagedLifecycleReport,
    manifest.packagedInstallerScreenshot,
    manifest.packagedWindowScreenshot,
    manifest.packagedRendererReport,
    manifest.packagedMacAcceptance,
    ...(manifest.packagedRendererScreenshots ?? []),
    ...(manifest.packagedMacScreenshots ?? []),
    ...(manifest.nativeAcceptance
      ? [{ ...manifest.nativeAcceptance, name: manifest.nativeAcceptance.path }]
      : []),
  ].filter(Boolean);
  const evidence = [
    { ...inspectArtifact(manifestPath), path: "build-manifest.json" },
  ];
  for (const record of records) {
    assert.ok(
      typeof record.name === "string" &&
        /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(record.name) &&
        !record.name.includes(".."),
      "Unsafe native evidence path",
    );
    const actual = inspectArtifact(join(root, record.name));
    assert.equal(actual.sha256, record.sha256, "Native evidence hash differs");
    if (record.bytes !== undefined)
      assert.equal(
        actual.size_bytes,
        record.bytes,
        "Native evidence size differs",
      );
    evidence.push({ ...actual, path: record.name });
  }
  for (const record of [native, manifest.packagedRendererReport].filter(
    Boolean,
  )) {
    const report = JSON.parse(readFileSync(join(root, record.name), "utf8"));
    assert.ok(
      report.passed === true &&
        report.version === manifest.version &&
        report.sourceSha256 === manifest.sourceProvenance.sourceSha256,
      "Native acceptance result/source differs from original manifest",
    );
    if (record === native)
      assert.ok(
        report.gitHead === manifest.sourceProvenance.git.head &&
          report.target === manifest.target,
        "Native acceptance commit/target differs from original manifest",
      );
  }
  return evidence;
}

export function createReviewedReleasePlan({ notesOverride, ...options }) {
  // Do not replace config or weaken any original manifest/byte/signature gates.
  const original = createReleasePlan(options);
  if (notesOverride === undefined) return original;
  const evidence = verifyEvidence(options.buildDirectory);
  const { value: review } = readReviewedNotes(notesOverride);
  assert.deepEqual(
    Object.keys(review).sort(),
    [
      "schemaVersion",
      "releaseId",
      "version",
      "channel",
      "sourceSha256",
      "gitHead",
      "originalPlanSha256",
      "originalNotesSha256",
      "notes",
    ].sort(),
    "Unexpected reviewed notes fields",
  );
  assert.equal(review.schemaVersion, 1);
  for (const [field, expected] of Object.entries({
    releaseId: original.release_id,
    version: original.version,
    channel: original.channel,
    sourceSha256: original.source_sha,
    gitHead: original.git_head,
    originalPlanSha256: releasePlanFingerprint(original),
    originalNotesSha256: notesDigest(original.notes),
  })) {
    assert.equal(
      review[field],
      expected,
      "Reviewed notes binding differs: " + field,
    );
  }
  assert.ok(
    typeof review.notes === "string" &&
      review.notes.trim().length > 0 &&
      review.notes.length <= 8192 &&
      !/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(review.notes),
    "Invalid reviewed user notes",
  );
  const config = loadUpdaterReleaseConfig(options.updaterConfigPath, {
    channel: options.channel,
  });
  const paragraphs = (text) => text.split(/\n\s*\n/);
  // Preserve every tool-added paragraph verbatim, including future disclosures.
  // Known platform disclosures are retained even if also present in config.
  const body = paragraphs(config.config.notes);
  const disclosures = [
    ...new Set(
      paragraphs(original.notes).filter(
        (p) =>
          !body.includes(p) ||
          p.startsWith("此版本未经 Apple 公证。") ||
          p.startsWith("此 Windows 安装包未使用 Authenticode 签名；"),
      ),
    ),
  ];
  const notes = [...disclosures, review.notes].join("\n\n");
  assert.ok(notes.length <= 16384, "Final reviewed notes exceed release limit");
  return {
    ...original,
    notes,
    notes_review: {
      schema_version: 1,
      override_sha256: notesOverride.sha256,
      original_plan_sha256: review.originalPlanSha256,
      original_notes_sha256: review.originalNotesSha256,
      reviewed_notes_sha256: notesDigest(review.notes),
      final_notes_sha256: notesDigest(notes),
      updater_config_sha256: config.sha256,
      native_evidence_sha256: notesDigest(JSON.stringify(sorted(evidence))),
      required_disclosures: disclosures,
    },
  };
}
