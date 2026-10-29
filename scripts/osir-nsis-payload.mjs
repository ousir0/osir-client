// Tauri 2.10 bundles the NSS marker into NSIS then restores the Cargo executable.
// Never normalize arbitrary bytes: the exact, unique UNK -> NSS patch is required.
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
const unknown = Buffer.from("__TAURI_BUNDLE_TYPE_VAR_UNK");
const nsis = Buffer.from("__TAURI_BUNDLE_TYPE_VAR_NSS");

export function nsisPayload(compiled) {
  const offset = compiled.indexOf(unknown);
  assert.ok(offset >= 0 && compiled.lastIndexOf(unknown) === offset, "Expected exactly one Tauri unknown bundle marker");
  const result = Buffer.from(compiled);
  nsis.copy(result, offset);
  const changedOffsets = [];
  for (let i = 0; i < compiled.length; i++) if (compiled[i] !== result[i]) changedOffsets.push(i);
  assert.deepEqual(changedOffsets, [offset + unknown.length - 3, offset + unknown.length - 2, offset + unknown.length - 1]);
  return { bytes: result, changedOffsets };
}

export function verifyNsisPayload(compiled, actual) {
  const expected = nsisPayload(compiled);
  assert.ok(expected.bytes.equals(actual), "NSIS executable differs beyond the exact Tauri UNK to NSS marker patch");
  return { schemaVersion: 1, algorithm: "Tauri-2.10-unique-UNK-to-NSS", changedBytes: 3,
    changedOffsets: expected.changedOffsets, compiledSha256: createHash("sha256").update(compiled).digest("hex"),
    nsisSha256: createHash("sha256").update(actual).digest("hex"), bytes: actual.length };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [compiledPath, actualPath, reportPath] = process.argv.slice(2);
  try { writeFileSync(reportPath, JSON.stringify(verifyNsisPayload(readFileSync(compiledPath), readFileSync(actualPath)), null, 2) + "\n"); }
  catch (error) { console.error("NSIS payload: " + error.message); process.exitCode = 1; }
}
