#!/usr/bin/env node
// Export declared build inputs into a NEW directory for a private repository.
// Never copies .git, user homes, screenshots, build outputs or signing material.
import assert from "node:assert/strict";
import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createHash } from "node:crypto";
import {
  sourceSnapshot,
  verifySourceSnapshot,
  gitProvenance,
} from "./osir-build.mjs";
import { loadUpdaterReleaseConfig } from "./osir-release-updater.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const extraFiles = [
  ".gitattributes",
  ".gitignore",
  "src-tauri/.gitignore",
  "LICENSE",
  "README_OSIR.md",
];
const forbidden =
  /(?:^|\/)(?:\.env(?:\..*)?|\.npmrc|id_(?:rsa|ed25519)|[^/]+\.(?:p8|p12|pfx|pem|key|keystore))$/i;

export function validateSnapshotPath(relative) {
  assert.ok(
    typeof relative === "string" &&
      relative.length > 0 &&
      !isAbsolute(relative),
  );
  assert.ok(
    !relative
      .split(/[\\/]/)
      .some(
        (part) =>
          part === ".." ||
          part === ".git" ||
          part === "node_modules" ||
          part === ".osir-dev-home",
      ),
  );
  assert.ok(
    !forbidden.test(relative),
    "Signing or environment file rejected from source snapshot",
  );
  return relative;
}

function main() {
  const [flag, destination, ...rest] = process.argv.slice(2);
  assert.ok(
    flag === "--output" && destination && rest.length === 0,
    "Usage: node scripts/osir-source-snapshot.mjs --output <new-private-source-directory>",
  );
  const output = resolve(destination);
  assert.ok(!existsSync(output), "Snapshot destination must not exist");
  assert.ok(
    existsSync(dirname(output)),
    "Destination parent must already exist",
  );
  const snapshot = sourceSnapshot();
  const git = gitProvenance();
  const excluded = new Set(snapshot.provenance.inputs.excludedDirectoryNames);
  for (const channel of ["prod", "beta"])
    loadUpdaterReleaseConfig(
      join(root, `config/osir-updater-${channel}.json`),
      { channel },
    );
  mkdirSync(output, { mode: 0o700 });
  const copied = [];
  function copy(relative) {
    validateSnapshotPath(relative);
    const source = join(root, relative);
    if (!existsSync(source)) return;
    const stat = lstatSync(source);
    assert.ok(
      !stat.isSymbolicLink(),
      "Snapshot symlinks are rejected without following",
    );
    const target = join(output, relative);
    if (stat.isDirectory()) {
      mkdirSync(target, { recursive: true, mode: 0o755 });
      for (const name of readdirSync(source).sort())
        if (!excluded.has(name)) copy(relative + "/" + name);
    } else {
      assert.ok(stat.isFile());
      if (copied.includes(relative)) return;
      mkdirSync(dirname(target), { recursive: true });
      copyFileSync(source, target);
      copied.push(relative);
    }
  }
  for (const relative of [
    ...snapshot.provenance.inputs.directories,
    ...snapshot.provenance.inputs.files,
    ...extraFiles,
  ])
    copy(relative);
  verifySourceSnapshot(snapshot, git);
  const metadata = {
    schemaVersion: 1,
    sourceSha256: snapshot.provenance.sourceSha256,
    inputsManifestSha256: snapshot.provenance.inputsManifestSha256,
    exportedFrom: git,
    files: copied.sort(),
    fileListSha256: createHash("sha256")
      .update(copied.sort().join("\n"))
      .digest("hex"),
  };
  // This helper report is outside the repository. No personal paths enter Git.
  writeFileSync(
    output + ".snapshot.json",
    JSON.stringify(metadata, null, 2) + "\n",
    { flag: "wx", mode: 0o600 },
  );
  console.log(
    JSON.stringify(
      {
        directory: output,
        files: copied.length,
        sourceSha256: metadata.sourceSha256,
        note: "Review files, init a private Git repository, commit all declared public inputs. This command does not initialize, push or dispatch CI.",
      },
      null,
      2,
    ),
  );
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  try {
    main();
  } catch (error) {
    console.error("OSIR source snapshot: " + error.message);
    process.exitCode = 1;
  }
}
