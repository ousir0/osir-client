#!/usr/bin/env node
// Local OSIR distribution entry. Uses the persistent channel key without
// reading or printing private material; the native Tauri signer reads it.
import { spawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { requireUpdaterSigner } from "./osir-release-updater.mjs";

const [channel, ...flags] = process.argv.slice(2);
if (
  !["beta", "prod"].includes(channel) ||
  flags.some((flag) => !["--check", "--dry-run"].includes(flag))
) {
  console.error(
    "Usage: node scripts/osir-release-local.mjs <beta|prod> [--check|--dry-run]",
  );
  process.exitCode = 1;
} else {
  const root = fileURLToPath(new URL("../", import.meta.url));
  const environment = {
    ...process.env,
    TAURI_SIGNING_PRIVATE_KEY:
      process.env.TAURI_SIGNING_PRIVATE_KEY ||
      join(
        homedir(),
        ".config/osir-client/release-signing",
        channel,
        "updater.key",
      ),
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD:
      process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD || "",
  };
  try {
    requireUpdaterSigner(environment);
    const child = spawn(
      process.execPath,
      [
        join(root, "scripts/osir-build.mjs"),
        channel,
        "--release-self-signed",
        "--updater-config",
        join(root, "config", `osir-updater-${channel}.json`),
        ...flags,
      ],
      {
        cwd: root,
        env: environment,
        stdio: "inherit",
      },
    );
    for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"])
      process.on(signal, () => child.kill(signal));
    child.once("error", () => {
      console.error("OSIR distribution could not start.");
      process.exitCode = 1;
    });
    child.once("exit", (code) => {
      process.exitCode = code ?? 1;
    });
  } catch {
    console.error(
      "OSIR channel updater key is missing or not a private regular file. Restore the original channel key in its private signing directory; never generate a replacement for an existing release.",
    );
    process.exitCode = 1;
  }
}
