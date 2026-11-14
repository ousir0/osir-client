import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { releaseVersion, setSnapshotVersion } from "./osir-release-version.mjs";
import { sourceSnapshot } from "./osir-build.mjs";

test("revision tokens are canonical, distinct, safe for existing backend IDs/files, and leave legacy releases intact", () => {
  for (const version of ["0.1.10", "0.1.11", "4.0.21-beta.1"])
    assert.equal(releaseVersion(version).token, version);
  const tokens = [
    "0.1.11",
    "0.1.11+1",
    "0.1.11+2",
    "0.1.11+10",
    "0.1.11-build.1",
  ].map((v) => releaseVersion(v).token);
  assert.equal(new Set(tokens).size, tokens.length);
  assert.equal(tokens[1], "0.1.11.build.1");
  for (const token of tokens)
    assert.match(`prod-${token}`, /^[a-z0-9][a-z0-9.-]{0,95}$/);
  for (const invalid of [
    "0.1.11+0",
    "0.1.11+01",
    "0.1.11+65536",
    "0.1.11+20261115",
    "0.1.11+r1",
    "0.1.11+1.2",
    "0.1.11-beta.1+1",
    "0.1.11-beta.01",
    "01.1.11",
    "0.1.11+",
    "0.1.11/1",
    "0.1.11\n",
  ])
    assert.throws(() => releaseVersion(invalid));
  assert.throws(() => releaseVersion("0.1.11-beta.1", "prod"));
});

test("snapshot version changes all four version records only in the copy and fingerprints the resulting build inputs", (t) => {
  const work = mkdtempSync(join(tmpdir(), "osir-revision-snapshot-"));
  t.after(() => rmSync(work, { recursive: true, force: true }));
  mkdirSync(join(work, "src-tauri"));
  writeFileSync(join(work, "package.json"), '{"version":"0.1.10"}\n');
  writeFileSync(
    join(work, "src-tauri/tauri.conf.json"),
    '{"version":"0.1.10"}\n',
  );
  writeFileSync(
    join(work, "src-tauri/Cargo.toml"),
    '[package]\nname = "cc-switch"\nversion = "0.1.10"\n',
  );
  writeFileSync(
    join(work, "src-tauri/Cargo.lock"),
    'version = 4\n\n[[package]]\nname = "cc-switch"\nversion = "0.1.10"\n\n[[package]]\nname = "other"\nversion = "0.1.10"\n',
  );
  const before = sourceSnapshot(work).provenance.sourceSha256;
  setSnapshotVersion(work, "0.1.11+10");
  assert.equal(
    JSON.parse(readFileSync(join(work, "package.json"))).version,
    "0.1.11+10",
  );
  assert.equal(
    JSON.parse(readFileSync(join(work, "src-tauri/tauri.conf.json"))).version,
    "0.1.11+10",
  );
  assert.match(
    readFileSync(join(work, "src-tauri/Cargo.toml"), "utf8"),
    /version = "0.1.11\+10"/,
  );
  const lock = readFileSync(join(work, "src-tauri/Cargo.lock"), "utf8");
  assert.match(lock, /name = "cc-switch"\nversion = "0.1.11\+10"/);
  assert.match(lock, /name = "other"\nversion = "0.1.10"/);
  assert.notEqual(sourceSnapshot(work).provenance.sourceSha256, before);
  assert.throws(() => setSnapshotVersion(work, "0.1.12"));
  const original = readFileSync(join(work, "package.json"), "utf8");
  writeFileSync(join(work, "src-tauri/Cargo.lock"), "invalid");
  assert.throws(() => setSnapshotVersion(work, "0.1.11+11"));
  assert.equal(readFileSync(join(work, "package.json"), "utf8"), original);
});
