// Isolated compatibility proof: compile only the installed semver source, never
// Tauri, the application, an installer, or any production app/data directory.
import test from "node:test";
import assert from "node:assert/strict";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

test("Tauri updater 2.10.0 default > recognizes same-public-version numeric revisions with semver 1.0.27", (t) => {
  const registry = join(
    process.env.CARGO_HOME || join(homedir(), ".cargo"),
    "registry/src",
  );
  const sources = existsSync(registry)
    ? readdirSync(registry).map((p) => join(registry, p))
    : [];
  const directory = sources.find(
    (p) =>
      existsSync(join(p, "semver-1.0.27/src/lib.rs")) &&
      existsSync(join(p, "tauri-plugin-updater-2.10.0/src/updater.rs")),
  );
  if (!directory || spawnSync("rustc", ["--version"]).status !== 0) {
    t.skip(
      "Requires local semver 1.0.27, updater 2.10.0 registry sources and rustc; compatibility NOT verified here",
    );
    return;
  }
  const updater = readFileSync(
    join(directory, "tauri-plugin-updater-2.10.0/src/updater.rs"),
    "utf8",
  );
  assert.match(updater, /None => release\.version > self\.current_version/);
  const settings = readFileSync(
    new URL("../src-tauri/src/commands/settings.rs", import.meta.url),
    "utf8",
  );
  assert.match(settings, /pub async fn install_update_and_restart/);
  assert.match(settings, /pub async fn check_app_update_available/);
  assert.doesNotMatch(settings, /\.version_comparator\s*\(/);
  const lock = readFileSync(
    new URL("../src-tauri/Cargo.lock", import.meta.url),
    "utf8",
  );
  assert.match(lock, /name = "semver"\nversion = "1\.0\.27"/);
  assert.match(lock, /name = "tauri-plugin-updater"\nversion = "2\.10\.0"/);
  const work = mkdtempSync(join(tmpdir(), "osir-updater-order-"));
  t.after(() => rmSync(work, { recursive: true, force: true }));
  const library = join(work, "libsemver.rlib");
  const binary = join(
    work,
    process.platform === "win32" ? "check.exe" : "check",
  );
  function run(command, args) {
    const result = spawnSync(command, args, {
      encoding: "utf8",
      timeout: 30000,
    });
    assert.equal(result.status, 0, result.error?.message || result.stderr);
    return result.stdout;
  }
  run("rustc", [
    "--edition=2021",
    "--crate-type=rlib",
    "--crate-name=semver",
    "--cfg",
    'feature="std"',
    join(directory, "semver-1.0.27/src/lib.rs"),
    "-o",
    library,
  ]);
  writeFileSync(
    join(work, "main.rs"),
    `
use semver::Version;
fn main() {
  for (current, release, expected) in [
    ("0.1.10", "0.1.11+1", true),
    ("0.1.11", "0.1.11+1", true),
    ("0.1.11+1", "0.1.11+2", true),
    ("0.1.11+9", "0.1.11+10", true),
    ("0.1.11+99", "0.1.11+100", true),
    ("0.1.11+65534", "0.1.11+65535", true),
    ("0.1.11+10", "0.1.11+10", false),
    ("0.1.11+10", "0.1.11+9", false),
    ("0.1.11+1", "0.1.11", false),
    ("0.1.11+1", "0.1.10+65535", false),
    ("0.1.11+65535", "0.1.12-alpha.1", true),
    ("0.1.12-alpha.1", "0.1.11+65535", false),
    ("0.1.12-alpha.1", "0.1.12", true),
    ("0.1.12", "0.1.11+65535", false),
  ] {
    let current_version = Version::parse(current).unwrap();
    let release_version = Version::parse(release).unwrap();
    assert_eq!(release_version > current_version, expected, "{current} -> {release}");
    println!("{current} -> {release}: {expected}");
  }
}`,
  );
  run("rustc", [
    "--edition=2021",
    join(work, "main.rs"),
    "--extern",
    `semver=${library}`,
    "-o",
    binary,
  ]);
  const output = run(binary, []);
  assert.equal(output.trim().split("\n").length, 14);
  t.diagnostic(output.trim());
});
