import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

// Stable revisions use one canonical numeric component. The bound also fits
// the Windows installer version's fourth (u16) component. Never use timestamps
// or arbitrary metadata whose Rust ordering differs from numeric revision order.
export function releaseVersion(version, channel) {
  assert.equal(typeof version, "string", "Release version must be a string");
  const match =
    /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([1-9]\d*))?$/.exec(
      version,
    );
  assert.ok(
    match && match[0] === version && version.length <= 64,
    "Invalid release version",
  );
  const [, major, minor, patch, prerelease, revision] = match;
  assert.ok(
    !prerelease || prerelease.split(".").every((p) => !/^0\d+$/.test(p)),
    "Invalid numeric prerelease",
  );
  assert.ok(
    !revision || (!prerelease && BigInt(revision) <= 65535n),
    "Revision must be stable and between 1 and 65535",
  );
  assert.ok(
    channel !== "prod" || !prerelease,
    "Production cannot promote a prerelease version",
  );
  const base = `${major}.${minor}.${patch}`;
  // A fourth dot component cannot collide with a legacy SemVer prerelease.
  // Keep + out of IDs AND filenames: both backend validators disallow it.
  return {
    version,
    base,
    revision,
    token: revision ? `${base}.build.${revision}` : version,
  };
}

// Only call against a NEW exported snapshot, never the development checkout.
// Prepare every replacement before writing; keep locked Cargo builds coherent.
export function setSnapshotVersion(directory, version) {
  assert.ok(
    releaseVersion(version).revision,
    "Snapshot override requires +revision",
  );
  const replacements = [];
  let previous;
  for (const file of ["package.json", "src-tauri/tauri.conf.json"]) {
    const path = join(directory, file);
    const value = JSON.parse(readFileSync(path, "utf8"));
    previous ??= value.version;
    assert.equal(value.version, previous, "Snapshot versions must match");
    value.version = version;
    replacements.push([path, JSON.stringify(value, null, 2) + "\n"]);
  }
  const cargoPath = join(directory, "src-tauri/Cargo.toml");
  const cargo = readFileSync(cargoPath, "utf8");
  const packageName = cargo.match(
    /^\[package\]\s*\nname\s*=\s*"([^"]+)"/m,
  )?.[1];
  assert.ok(packageName, "Missing root Cargo package");
  const cargoVersion = /^version\s*=\s*"([^"]+)"/m;
  assert.equal(
    cargo.match(cargoVersion)?.[1],
    previous,
    "Snapshot Cargo version must match",
  );
  replacements.push([
    cargoPath,
    cargo.replace(cargoVersion, `version = "${version}"`),
  ]);
  const lockPath = join(directory, "src-tauri/Cargo.lock");
  const lock = readFileSync(lockPath, "utf8");
  const needle = `[[package]]\nname = "${packageName}"\nversion = "${previous}"`;
  assert.equal(
    lock.split(needle).length,
    2,
    "Expected exactly one root package in Cargo.lock",
  );
  replacements.push([
    lockPath,
    lock.replace(
      needle,
      `[[package]]\nname = "${packageName}"\nversion = "${version}"`,
    ),
  ]);
  for (const [path, content] of replacements) writeFileSync(path, content);
}
