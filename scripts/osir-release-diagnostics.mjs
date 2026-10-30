// Execute only the native read-only diagnostics entry point. Never start the UI
// or inherit signing credentials, tool homes, loader overrides or account tokens.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstatSync, readFileSync } from "node:fs";
import { dirname, isAbsolute } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { decodeUpdaterPublicKey } from "./osir-release-updater.mjs";

const FLAG = "--osir-release-diagnostics";
const DISABLED_REASON = "OSIR 更新渠道未配置：尚无可用的 OSIR 签名更新源。";
const LIMIT = 65536;
function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}
function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}
function digest(executable) {
  try {
    const stat = lstatSync(executable);
    requireThat(stat.isFile() && !stat.isSymbolicLink() && stat.size > 0);
    return sha256(readFileSync(executable));
  } catch {
    throw new Error("Release diagnostics requires a regular executable.");
  }
}
function exactKeys(value, keys) {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    isDeepStrictEqual(Object.keys(value).sort(), [...keys].sort())
  );
}

function expectedReport({
  config,
  updatesEnabled,
  bundleType,
  releaseBuild = true,
}) {
  requireThat(
    config &&
      [
        "com.osir.client",
        "com.osir.client.beta",
        "com.osir.client.dev",
      ].includes(config.identifier) &&
      typeof config.version === "string" &&
      typeof updatesEnabled === "boolean" &&
      typeof releaseBuild === "boolean" &&
      [null, "app", "nsis"].includes(bundleType),
    "Release diagnostics expectation is invalid.",
  );
  const updater = config.plugins?.updater;
  const pubkey = updater?.pubkey || null;
  if (pubkey !== null) decodeUpdaterPublicKey(pubkey);
  const endpoints = updater?.endpoints ?? null;
  const marker = config.plugins?.["osir-updates"] ?? null;
  return {
    schemaVersion: 1,
    identifier: config.identifier,
    version: config.version,
    packageVersion: config.version,
    releaseBuild,
    bundleType,
    updater: { pubkey, endpoints },
    releaseMarker: marker,
    updateDistribution: {
      kind: "osir",
      updatesEnabled,
      reason: updatesEnabled ? null : DISABLED_REASON,
    },
  };
}

export function verifyReleaseDiagnostics(
  executable,
  expectation,
  run = spawnSync,
) {
  requireThat(
    typeof executable === "string" &&
      isAbsolute(executable) &&
      !/[\x00-\x1f\x7f]/.test(executable),
    "Release diagnostics requires an absolute executable path.",
  );
  const expected = expectedReport(expectation);
  const before = digest(executable);
  const env = {};
  // Windows needs its system directory to load system DLLs. No user/profile,
  // account, signing, NODE_OPTIONS or DYLD/LD variables are passed through.
  if (process.platform === "win32")
    for (const key of ["SystemRoot", "WINDIR"])
      if (process.env[key]) env[key] = process.env[key];
  let result;
  try {
    result = run(executable, [FLAG], {
      cwd: dirname(executable),
      env,
      shell: false,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
      timeout: 10000,
      maxBuffer: LIMIT,
      encoding: "utf8",
    });
  } catch {
    throw new Error("Native release diagnostics could not execute.");
  }
  requireThat(
    !result?.error && result.status === 0 && !result.signal,
    "Native release diagnostics failed or timed out.",
  );
  requireThat(
    digest(executable) === before,
    "Executable changed during release diagnostics.",
  );
  requireThat(
    typeof result.stdout === "string" &&
      Buffer.byteLength(result.stdout) <= LIMIT &&
      result.stdout.endsWith("\n") &&
      !result.stderr,
    "Native release diagnostics output is invalid.",
  );
  let report;
  try {
    report = JSON.parse(result.stdout);
  } catch {
    throw new Error("Native release diagnostics did not return JSON.");
  }
  requireThat(
    exactKeys(report, Object.keys(expected)) &&
      exactKeys(report.updater, ["pubkey", "endpoints"]) &&
      exactKeys(report.updateDistribution, [
        "kind",
        "updatesEnabled",
        "reason",
      ]) &&
      (report.releaseMarker === null ||
        exactKeys(report.releaseMarker, [
          "schemaVersion",
          "channel",
          "bundleArtifacts",
        ])) &&
      isDeepStrictEqual(report, expected),
    "Compiled release configuration differs from the requested public configuration.",
  );
  // Only the validated public schema is retained. Never echo raw child output
  // or include it in errors, even when a future binary returns unexpected data.
  return {
    schemaVersion: 1,
    executableSha256: before,
    reportSha256: sha256(Buffer.from(JSON.stringify(report))),
    report,
  };
}
