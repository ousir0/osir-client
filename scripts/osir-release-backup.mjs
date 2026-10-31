#!/usr/bin/env node
// Read-only cloud backup and local restore. No credentials, build or publication.
import {
  chmodSync,
  closeSync,
  constants,
  copyFileSync,
  existsSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  realpathSync,
  writeFileSync,
  writeSync,
} from "node:fs";
import {
  basename,
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";
import {
  createReleasePlan,
  inspectArtifact,
} from "./osir-release-publisher.mjs";

const ORIGIN = "https://osirclaw.com";
const SAFE_PATH =
  /^(?:[A-Za-z0-9][A-Za-z0-9._-]*\/)*[A-Za-z0-9][A-Za-z0-9._-]*$/;
function requireThat(ok, message) {
  if (!ok) throw new Error(message);
}
function ordinary(path, directory = false) {
  const stat = lstatSync(path);
  requireThat(
    !stat.isSymbolicLink() &&
      (directory ? stat.isDirectory() : stat.isFile() && stat.nlink === 1),
    "Backup inputs must be ordinary paths without links.",
  );
  return stat;
}
function json(path) {
  requireThat(
    ordinary(path).size <= 1024 * 1024,
    "Backup metadata exceeds size limit.",
  );
  return JSON.parse(readFileSync(path, "utf8"));
}
function inside(root, path) {
  const child = relative(root, path);
  return (
    child === "" ||
    (child !== ".." && !child.startsWith(".." + sep) && !isAbsolute(child))
  );
}
function fresh(path, forbidden) {
  requireThat(
    isAbsolute(path) && !existsSync(path),
    "Backup destination must be a new absolute directory.",
  );
  ordinary(dirname(path), true);
  const target = join(realpathSync(dirname(path)), basename(path));
  requireThat(
    !inside(forbidden, target) && !inside(target, forbidden),
    "Backup destination must be separate from its source.",
  );
  mkdirSync(target, { mode: 0o700 });
  return target;
}
function copyChecked(source, destination) {
  ordinary(source);
  const before = inspectArtifact(source);
  mkdirSync(dirname(destination), { recursive: true, mode: 0o700 });
  copyFileSync(source, destination, constants.COPYFILE_EXCL);
  const after = inspectArtifact(destination);
  requireThat(
    before.sha256 === after.sha256 &&
      before.size_bytes === after.size_bytes &&
      inspectArtifact(source).sha256 === before.sha256,
    "Backup source changed while copying.",
  );
  return { sha256: after.sha256, size_bytes: after.size_bytes };
}
function seal(root) {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    ordinary(path, entry.isDirectory());
    if (entry.isDirectory()) seal(path);
    else chmodSync(path, 0o400);
  }
  chmodSync(root, 0o500);
}
function canonical(plan) {
  return {
    release_id: plan.release_id,
    channel: plan.channel,
    version: plan.version,
    source_sha: plan.source_sha,
    git_head: plan.git_head,
    artifacts: plan.artifacts.map(
      ({ platform, kind, file_name, url, sha256, size_bytes, signature }) => ({
        platform,
        kind,
        file_name,
        url,
        sha256,
        size_bytes,
        ...(signature ? { signature } : {}),
      }),
    ),
  };
}
function acceptanceArtifacts(manifest) {
  const artifacts = [
    "mainExecutableArtifact",
    "packagedLifecycleReport",
    "packagedInstallerScreenshot",
    "packagedWindowScreenshot",
    "packagedRendererReport",
    "packagedMacAcceptance",
  ].flatMap((field) => (manifest[field] ? [manifest[field]] : []));
  const screenshots = manifest.packagedRendererScreenshots;
  requireThat(
    (!manifest.packagedRendererReport && screenshots === undefined) ||
      (manifest.packagedRendererReport &&
        Array.isArray(screenshots) &&
        screenshots.length === 4 &&
        screenshots.every(
          (item) =>
            typeof item?.name === "string" && item.name.endsWith(".png"),
        )),
    "Renderer acceptance must bind its report and all four PNG screenshots.",
  );
  artifacts.push(...(screenshots ?? []));
  artifacts.push(...(manifest.packagedMacScreenshots ?? []));
  const names = new Set();
  for (const item of artifacts) {
    requireThat(
      typeof item.name === "string" &&
        SAFE_PATH.test(item.name) &&
        !item.name.includes("..") &&
        !names.has(item.name),
      "Acceptance filename rejected.",
    );
    names.add(item.name);
  }
  return artifacts;
}
async function download(artifact, destination, fetchImpl) {
  const url = new URL(artifact.url);
  requireThat(
    url.origin === ORIGIN &&
      !url.username &&
      !url.password &&
      !url.search &&
      !url.hash &&
      url.pathname.startsWith("/api/v1/osir-client/artifacts/"),
    "Backup artifact origin rejected.",
  );
  const response = await fetchImpl(url.href, {
    method: "GET",
    redirect: "error",
    signal: AbortSignal.timeout(180000),
    headers: { accept: "application/octet-stream" },
  });
  requireThat(
    response.status === 200 && response.url === url.href,
    "Backup public download failed or redirected.",
  );
  const declared = response.headers.get("content-length");
  requireThat(
    declared === null || Number(declared) === artifact.size_bytes,
    "Backup response size differs from release.",
  );
  const fd = openSync(
    destination,
    constants.O_WRONLY |
      constants.O_CREAT |
      constants.O_EXCL |
      constants.O_NOFOLLOW,
    0o600,
  );
  let bytes = 0;
  try {
    for await (const chunk of response.body ?? []) {
      bytes += chunk.length;
      requireThat(
        bytes <= artifact.size_bytes,
        "Backup response exceeded release size.",
      );
      let offset = 0;
      while (offset < chunk.length)
        offset += writeSync(fd, chunk, offset, chunk.length - offset);
    }
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  const actual = inspectArtifact(destination);
  requireThat(
    actual.size_bytes === artifact.size_bytes &&
      actual.sha256 === artifact.sha256,
    "Backup public bytes differ from release.",
  );
}

export async function backupRelease({
  channel,
  buildDirectory,
  updaterConfigPath,
  destination,
  fetchImpl = globalThis.fetch,
}) {
  ordinary(buildDirectory, true);
  const source = realpathSync(buildDirectory);
  const expected = canonical(
    createReleasePlan({ channel, buildDirectory: source, updaterConfigPath }),
  );
  const root = fresh(destination, source);
  const tree = join(root, "release");
  mkdirSync(tree, { mode: 0o700 });
  // Only reviewed release metadata and signatures are copied. Never sweep logs,
  // operator inputs, signing material, app data or arbitrary evidence folders.
  const metadata = [];
  function collect(directory, prefix = "") {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const name = prefix + entry.name;
      if (
        entry.isDirectory() &&
        /^[a-z0-9-]+$/.test(entry.name) &&
        existsSync(join(directory, entry.name, "build-manifest.json"))
      ) {
        ordinary(join(directory, entry.name), true);
        collect(join(directory, entry.name), name + "/");
      } else if (
        entry.name === "build-manifest.json" ||
        entry.name === `updater-${channel}.json` ||
        /^OSIR-Client-[A-Za-z0-9._-]+\.sig$/.test(entry.name)
      )
        metadata.push(name);
    }
  }
  collect(source);
  for (const name of metadata)
    copyChecked(join(source, name), join(tree, name));
  copyChecked(updaterConfigPath, join(root, "public-updater.json"));
  const unique = new Map();
  for (const artifact of expected.artifacts) {
    const previous = unique.get(artifact.file_name);
    requireThat(
      !previous ||
        (previous.sha256 === artifact.sha256 &&
          previous.size_bytes === artifact.size_bytes),
      "Conflicting artifact filenames.",
    );
    unique.set(artifact.file_name, artifact);
  }
  for (const artifact of unique.values())
    await download(artifact, join(tree, artifact.file_name), fetchImpl);
  // Recreate each platform's validated flat input, without rerunning a signer.
  for (const entry of metadata.filter(
    (name) => basename(name) === "build-manifest.json",
  )) {
    const folder = dirname(entry);
    const manifest = json(join(source, entry));
    for (const artifact of unique.values())
      if (
        folder !== "." &&
        existsSync(join(source, folder, artifact.file_name))
      )
        copyChecked(
          join(tree, artifact.file_name),
          join(tree, folder, artifact.file_name),
        );
    for (const item of acceptanceArtifacts(manifest)) {
      const info = copyChecked(
        join(source, folder, item.name),
        join(tree, folder, item.name),
      );
      requireThat(
        info.sha256 === item.sha256 && info.size_bytes === item.bytes,
        "Acceptance evidence differs from manifest.",
      );
    }
  }
  const missingEvidence = [];
  for (const entry of metadata.filter(
    (name) => basename(name) === "build-manifest.json",
  )) {
    const manifest = json(join(source, entry));
    const item = manifest.nativeAcceptance;
    if (item) {
      requireThat(
        SAFE_PATH.test(item.path) && !item.path.includes(".."),
        "Native acceptance filename rejected.",
      );
      const local = join(source, dirname(entry), item.path);
      if (!existsSync(local))
        missingEvidence.push(join(dirname(entry), item.path));
      else {
        const destination = join(tree, dirname(entry), item.path);
        const info = existsSync(destination)
          ? copyInfo(destination)
          : copyChecked(local, destination);
        requireThat(
          info.sha256 === item.sha256 &&
            inspectArtifact(local).sha256 === item.sha256,
          "Native acceptance evidence differs from manifest.",
        );
      }
    }
  }
  requireThat(
    isDeepStrictEqual(
      canonical(
        createReleasePlan({
          channel,
          buildDirectory: tree,
          updaterConfigPath: join(root, "public-updater.json"),
        }),
      ),
      expected,
    ),
    "Recovered release plan differs from source.",
  );
  const files = [];
  function inventory(directory, prefix = "") {
    for (const entry of readdirSync(directory, { withFileTypes: true }).sort(
      (a, b) => a.name.localeCompare(b.name),
    )) {
      const name = prefix + entry.name;
      if (entry.isDirectory())
        inventory(join(directory, entry.name), name + "/");
      else files.push({ path: name, ...copyInfo(join(directory, entry.name)) });
    }
  }
  inventory(root);
  const receipt = {
    schema_version: 1,
    status: "verified",
    created_at: new Date().toISOString(),
    plan: expected,
    cloud_unique_artifacts: unique.size,
    missing_evidence: missingEvidence,
    scope:
      "signed release inputs and cloud artifacts; not database, credentials or complete acceptance archive",
    files,
  };
  writeFileSync(
    join(root, "backup-receipt.json"),
    JSON.stringify(receipt, null, 2) + "\n",
    { flag: "wx", mode: 0o600 },
  );
  const receiptSha256 = inspectArtifact(
    join(root, "backup-receipt.json"),
  ).sha256;
  seal(root);
  return {
    path: root,
    available: true,
    receipt_sha256: receiptSha256,
    cloud_unique_artifacts: unique.size,
    missing_evidence: missingEvidence,
  };
}
function copyInfo(path) {
  const { sha256, size_bytes } = inspectArtifact(path);
  return { sha256, size_bytes };
}

export function restoreRelease({
  backupDirectory,
  destination,
  expectedReceiptSha256,
}) {
  ordinary(backupDirectory, true);
  const root = realpathSync(backupDirectory);
  requireThat(
    /^[a-f0-9]{64}$/.test(expectedReceiptSha256 ?? "") &&
      inspectArtifact(join(root, "backup-receipt.json")).sha256 ===
        expectedReceiptSha256,
    "Backup receipt differs from independently retained SHA-256.",
  );
  const receipt = json(join(root, "backup-receipt.json"));
  requireThat(
    receipt.schema_version === 1 &&
      receipt.status === "verified" &&
      Array.isArray(receipt.files) &&
      receipt.files.length > 0 &&
      receipt.files.length <= 128,
    "Backup receipt invalid.",
  );
  const seen = new Set();
  for (const file of receipt.files) {
    requireThat(
      typeof file.path === "string" &&
        SAFE_PATH.test(file.path) &&
        !file.path.includes("..") &&
        !seen.has(file.path),
      "Backup receipt path rejected.",
    );
    seen.add(file.path);
    let parent = dirname(join(root, file.path));
    while (parent !== root) {
      ordinary(parent, true);
      parent = dirname(parent);
    }
    requireThat(
      isDeepStrictEqual(copyInfo(join(root, file.path)), {
        sha256: file.sha256,
        size_bytes: file.size_bytes,
      }),
      "Backup bytes differ from receipt.",
    );
  }
  for (const file of receipt.files.filter(
    (item) => basename(item.path) === "build-manifest.json",
  )) {
    for (const item of acceptanceArtifacts(json(join(root, file.path)))) {
      const path = join(dirname(file.path), item.name);
      requireThat(
        seen.has(path) &&
          isDeepStrictEqual(copyInfo(join(root, path)), {
            sha256: item.sha256,
            size_bytes: item.bytes,
          }),
        "Backup acceptance evidence is missing or differs from manifest.",
      );
    }
  }
  const plan = canonical(
    createReleasePlan({
      channel: receipt.plan.channel,
      buildDirectory: join(root, "release"),
      updaterConfigPath: join(root, "public-updater.json"),
    }),
  );
  requireThat(
    isDeepStrictEqual(plan, receipt.plan),
    "Backup release identity changed.",
  );
  const output = fresh(destination, root);
  for (const file of receipt.files)
    copyChecked(join(root, file.path), join(output, file.path));
  const restored = canonical(
    createReleasePlan({
      channel: plan.channel,
      buildDirectory: join(output, "release"),
      updaterConfigPath: join(output, "public-updater.json"),
    }),
  );
  requireThat(
    isDeepStrictEqual(restored, plan),
    "Restored release validation failed.",
  );
  writeFileSync(
    join(output, "restore-receipt.json"),
    JSON.stringify(
      {
        schema_version: 1,
        status: "verified",
        release_id: plan.release_id,
        backup_receipt: copyInfo(join(root, "backup-receipt.json")),
        files: receipt.files.length,
      },
      null,
      2,
    ) + "\n",
    { flag: "wx", mode: 0o600 },
  );
  seal(output);
  return {
    path: output,
    available: true,
    release_id: plan.release_id,
    files: receipt.files.length,
  };
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    const [action, ...args] = process.argv.slice(2);
    requireThat(
      (action === "backup" && args.length === 4) ||
        (action === "restore" && args.length === 3),
      "Usage: osir-release-backup.mjs backup <beta|prod> <build-directory> <public-config> <new-directory> | restore <backup-directory> <new-directory> <independently-retained-receipt-sha256>",
    );
    const result =
      action === "backup"
        ? await backupRelease({
            channel: args[0],
            buildDirectory: args[1],
            updaterConfigPath: args[2],
            destination: args[3],
          })
        : restoreRelease({
            backupDirectory: args[0],
            destination: args[1],
            expectedReceiptSha256: args[2],
          });
    console.log(JSON.stringify(result, null, 2));
  } catch {
    console.error(
      "OSIR release backup/restore failed; preserve the partial directory and inspect inputs before retrying with a new destination.",
    );
    process.exitCode = 1;
  }
}
