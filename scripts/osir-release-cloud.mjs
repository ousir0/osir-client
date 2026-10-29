#!/usr/bin/env node
// One explicit OSIR release: build -> immutable upload -> byte readback -> CAS.
// Credentials remain with the native signer/SSH agent; this entry reads neither
// private signing keys nor application tokens and never retries a publication.
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readSync,
  realpathSync,
  renameSync,
  writeFileSync,
  writeSync,
} from "node:fs";
import { randomBytes } from "node:crypto";
import { spawn } from "node:child_process";
import { basename, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";
import {
  createReleasePlan,
  createServiceReleaseMetadata,
  inspectArtifact,
  verifyPublicReleaseArtifacts,
} from "./osir-release-publisher.mjs";

const ROOT = fileURLToPath(new URL("../", import.meta.url));
const ORIGIN = "https://osirclaw.com";
const SAFE_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$/;
const SHA256 = /^[a-f0-9]{64}$/;
const OPERATOR_FIELDS = [
  "serverCheckout",
  "binaryPath",
  "binarySha256",
  "liveRelease",
  "liveSha",
];
const PYTHON = "/usr/bin/python3";
const BUILD_PREFIXES = [
  "Verified ad-hoc distribution (not Apple notarized): ",
  "Verified local certificate distribution (not Apple notarized): ",
];
const USAGE =
  "node scripts/osir-release-cloud.mjs <beta|prod> --operator-config /absolute/private/config.json [--build-directory /absolute/prebuilt-directory] [--publish]";

function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

function within(root, path) {
  const child = relative(root, path);
  return (
    child === "" ||
    (child !== ".." && !child.startsWith(".." + sep) && !isAbsolute(child))
  );
}

function ordinaryDirectory(path, label) {
  requireThat(
    typeof path === "string" && isAbsolute(path) && !/[\r\n\0]/.test(path),
    `${label} must be an absolute path.`,
  );
  const stat = lstatSync(path);
  requireThat(
    stat.isDirectory() && !stat.isSymbolicLink(),
    `${label} must be an ordinary directory.`,
  );
  return realpathSync(path);
}

function ordinaryFile(path, maxBytes, label) {
  requireThat(
    typeof path === "string" && isAbsolute(path) && !/[\r\n\0]/.test(path),
    `${label} must be an absolute path.`,
  );
  const stat = lstatSync(path);
  requireThat(
    stat.isFile() &&
      !stat.isSymbolicLink() &&
      stat.nlink === 1 &&
      stat.size > 0 &&
      stat.size <= maxBytes,
    `${label} must be one bounded regular file, not a link.`,
  );
  return realpathSync(path);
}

function checkedJson(path, limit = 131072) {
  ordinaryFile(path, limit, "Release metadata");
  const fd = openSync(
    path,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  try {
    const before = fstatSync(fd);
    requireThat(
      before.isFile() &&
        before.nlink === 1 &&
        before.size > 0 &&
        before.size <= limit,
      "Release metadata must be a bounded regular file.",
    );
    const data = Buffer.alloc(limit + 1);
    let count = 0;
    while (count < data.length) {
      const read = readSync(fd, data, count, data.length - count, null);
      if (!read) break;
      count += read;
    }
    const after = fstatSync(fd);
    requireThat(
      count === before.size &&
        after.size === before.size &&
        before.mtimeMs === after.mtimeMs &&
        before.ctimeMs === after.ctimeMs,
      "Release metadata changed while reading.",
    );
    return JSON.parse(data.subarray(0, count).toString("utf8"));
  } finally {
    closeSync(fd);
  }
}

export function loadOperatorConfig(path) {
  ordinaryFile(path, 16384, "Operator configuration");
  const permissions = lstatSync(path);
  requireThat(
    (permissions.mode & 0o077) === 0 &&
      (process.getuid === undefined || permissions.uid === process.getuid()),
    "Operator configuration must be owned by this user with private permissions (0600).",
  );
  const config = checkedJson(path, 16384);
  requireThat(
    config &&
      !Array.isArray(config) &&
      isDeepStrictEqual(
        Object.keys(config).sort(),
        [...OPERATOR_FIELDS].sort(),
      ),
    "Operator configuration accepts only serverCheckout, binaryPath, binarySha256, liveRelease and liveSha; never put credentials here.",
  );
  requireThat(
    typeof config.binarySha256 === "string" &&
      SHA256.test(config.binarySha256) &&
      typeof config.liveSha === "string" &&
      /^[a-f0-9]{40}$/.test(config.liveSha) &&
      typeof config.liveRelease === "string" &&
      SAFE_ID.test(config.liveRelease) &&
      !config.liveRelease.includes(".."),
    "Operator binary SHA-256, live release or full 40-character live SHA is invalid.",
  );
  const serverCheckout = ordinaryDirectory(
    config.serverCheckout,
    "Server checkout",
  );
  const binaryPath = ordinaryFile(
    config.binaryPath,
    128 * 1024 * 1024,
    "Maintenance binary",
  );
  const binary = inspectArtifact(binaryPath);
  requireThat(
    binary.sha256 === config.binarySha256,
    "Maintenance binary SHA-256 differs from operator configuration.",
  );
  const fd = openSync(
    binaryPath,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  try {
    const magic = Buffer.alloc(4);
    requireThat(
      readSync(fd, magic, 0, 4, 0) === 4 &&
        magic.equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46])),
      "Operator configuration must select the reviewed Linux maintenance ELF.",
    );
  } finally {
    closeSync(fd);
  }
  const tools = {};
  for (const [key, name] of Object.entries({
    ops: "osir_client_release_ops.py",
    remote: "osir_client_release_remote.py",
    upload: "osir_client_artifacts.py",
  })) {
    const file = ordinaryFile(
      join(serverCheckout, "tools", name),
      1024 * 1024,
      "Server release tool",
    );
    requireThat(
      within(serverCheckout, file),
      "Server release tool escaped its checkout.",
    );
    tools[key] = { path: file, sha256: inspectArtifact(file).sha256 };
  }
  return { ...config, serverCheckout, binaryPath, tools };
}

export function parseCloudArguments(args) {
  const [channel, ...flags] = args;
  requireThat(["beta", "prod"].includes(channel), "Usage: " + USAGE);
  const options = { channel, publish: false };
  const seen = new Set();
  for (let index = 0; index < flags.length; index += 1) {
    const flag = flags[index];
    requireThat(!seen.has(flag), "Duplicate cloud release option.");
    seen.add(flag);
    if (flag === "--publish") options.publish = true;
    else if (flag === "--operator-config" || flag === "--build-directory") {
      const value = flags[++index];
      requireThat(
        value && isAbsolute(value) && !value.startsWith("--"),
        `Missing absolute path for ${flag}.`,
      );
      options[
        flag === "--operator-config" ? "operatorConfigPath" : "buildDirectory"
      ] = value;
    } else throw new Error("Unknown cloud release option. Usage: " + USAGE);
  }
  requireThat(
    options.operatorConfigPath,
    "An explicit private --operator-config is required. Usage: " + USAGE,
  );
  return options;
}

// Only these OS facilities reach Python/SSH. Signing, API and arbitrary user
// environment variables cannot cross this boundary, even with unexpected names.
export function pythonEnvironment(environment = process.env) {
  const clean = {
    PATH: "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    PYTHONDONTWRITEBYTECODE: "1",
    PYTHONNOUSERSITE: "1",
  };
  for (const key of [
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "SSH_AUTH_SOCK",
  ]) {
    if (typeof environment[key] === "string") clean[key] = environment[key];
  }
  return clean;
}

function buildEnvironment() {
  return Object.fromEntries(
    Object.entries(process.env).filter(
      ([key]) =>
        (!key.startsWith("APPLE_") &&
          !/(?:ADMIN_TOKEN|ACCESS_TOKEN|AUTH_TOKEN|API_TOKEN|API_KEY|PASSWORD)$/.test(
            key,
          )) ||
        key === "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
    ),
  );
}

function writeJson(path, value) {
  writeFileSync(path, JSON.stringify(value, null, 2) + "\n", {
    flag: "wx",
    mode: 0o600,
  });
}

async function runCommand(
  command,
  args,
  { cwd, env, logPath, timeoutMs = 25 * 60 * 1000 },
) {
  const log = openSync(
    logPath,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL,
    0o600,
  );
  let tail = "";
  try {
    return await new Promise((resolveCommand, reject) => {
      const child = spawn(command, args, {
        cwd,
        env,
        shell: false,
        stdio: ["ignore", "pipe", "pipe"],
      });
      let interrupted;
      let escalation;
      const stop = (signal, reason) => {
        interrupted = reason;
        child.kill(signal);
        escalation ??= setTimeout(() => child.kill("SIGKILL"), 5000);
      };
      const timer = setTimeout(() => stop("SIGTERM", "timeout"), timeoutMs);
      const handlers = ["SIGINT", "SIGTERM", "SIGHUP"].map((signal) => {
        const listener = () => stop(signal, signal);
        process.once(signal, listener);
        return [signal, listener];
      });
      const collect = (data) => {
        writeSync(log, data);
        tail = (tail + data.toString("utf8")).slice(-2 * 1024 * 1024);
      };
      child.stdout.on("data", collect);
      child.stderr.on("data", collect);
      const finish = () => {
        clearTimeout(timer);
        clearTimeout(escalation);
        for (const [signal, listener] of handlers)
          process.removeListener(signal, listener);
      };
      child.once("error", () => {
        finish();
        reject(
          new Error(
            `Could not start ${basename(command)}; inspect ${logPath}.`,
          ),
        );
      });
      child.once("close", (code, signal) => {
        finish();
        if (code !== 0 || interrupted)
          reject(
            new Error(
              `${basename(command)} stopped (${interrupted ?? "exit " + (code ?? signal)}); inspect ${logPath}.`,
            ),
          );
        else resolveCommand({ stdout: tail });
      });
    });
  } finally {
    closeSync(log);
  }
}

async function publicJson(fetchImpl, url) {
  requireThat(
    new URL(url).origin === ORIGIN,
    "Public release request origin is not trusted.",
  );
  // Mutable metadata can be cached for 60 seconds by intermediaries, including
  // the old current read immediately before CAS publication. Use a fresh cache
  // key for verification; immutable artifact downloads keep their stable URLs.
  const fresh = new URL(url);
  fresh.searchParams.set(
    "_osir_release_check",
    randomBytes(12).toString("hex"),
  );
  url = fresh.href;
  const response = await fetchImpl(url, {
    method: "GET",
    redirect: "error",
    cache: "no-store",
    signal: AbortSignal.timeout(15000),
    headers: { accept: "application/json", "cache-control": "no-cache" },
  });
  requireThat(
    response.url === url && response.ok,
    `Public release readback failed (HTTP ${response.status}).`,
  );
  if (response.status === 204) return null;
  const parts = [];
  let bytes = 0;
  for await (const part of response.body ?? []) {
    bytes += part.length;
    requireThat(
      bytes <= 131072,
      "Public release response exceeded its size limit.",
    );
    parts.push(part);
  }
  return JSON.parse(Buffer.concat(parts).toString("utf8"));
}

async function readCurrent(fetchImpl, channel) {
  const envelope = await publicJson(
    fetchImpl,
    `${ORIGIN}/api/v1/osir-client/releases/${channel}`,
  );
  if (envelope === null || (envelope?.code === 0 && envelope.data === null))
    return null;
  const value = envelope?.data;
  requireThat(
    envelope?.code === 0 &&
      value &&
      value.channel === channel &&
      value.status === "published" &&
      value.current === true &&
      typeof value.release_id === "string" &&
      SAFE_ID.test(value.release_id) &&
      !value.release_id.includes(".."),
    "Public current release did not match its channel contract.",
  );
  return value;
}

function assertToolBindings(config) {
  requireThat(
    inspectArtifact(
      ordinaryFile(config.binaryPath, 128 * 1024 * 1024, "Maintenance binary"),
    ).sha256 === config.binarySha256,
    "Maintenance binary changed after planning.",
  );
  for (const tool of Object.values(config.tools)) {
    requireThat(
      inspectArtifact(
        ordinaryFile(tool.path, 1024 * 1024, "Server release tool"),
      ).sha256 === tool.sha256,
      "Server release tool changed after planning.",
    );
  }
}

function assertOperationReceipt(
  path,
  {
    config,
    plan,
    action,
    reviewId,
    metadataSha,
    expectedCurrent,
    verificationSha,
  },
) {
  const receipt = checkedJson(path, 1024 * 1024);
  const request = receipt.request;
  const remote = receipt.remote_result;
  const result = remote?.result;
  const execute = action !== "inspect";
  requireThat(
    receipt.schema_version === 1 &&
      receipt.remote_script_sha256 === config.tools.remote.sha256 &&
      receipt.exit_code === 0 &&
      remote?.schema_version === 1 &&
      remote?.exit_code === 0 &&
      request?.schema_version === 1 &&
      request?.action === action &&
      request.execute === execute &&
      request.review_id === reviewId &&
      request.live_release === config.liveRelease &&
      request.live_sha === config.liveSha &&
      request.binary_sha256 === config.binarySha256 &&
      request.metadata_sha256 === metadataSha &&
      request.expected_current ===
        (action === "publish" ? (expectedCurrent ?? "none") : null) &&
      request.verification_sha256 === (verificationSha ?? null) &&
      remote.live?.release_id === config.liveRelease &&
      remote.live?.sha === config.liveSha &&
      remote.review_id === reviewId &&
      remote.binary_sha256 === config.binarySha256 &&
      remote.metadata_sha256 === metadataSha &&
      remote.verification_sha256 === (verificationSha ?? null) &&
      result?.action === action &&
      result.execution_requested === execute &&
      result.release_id === plan.release_id &&
      result.channel === plan.channel &&
      result.metadata_sha256 === metadataSha &&
      !result.state_may_have_changed &&
      (!execute || result.audit_recorded === true),
    `Root ${action} receipt did not confirm the exact reviewed operation; reconcile state before retrying.`,
  );
  if (action === "register")
    requireThat(
      result.status === "draft",
      "Root register did not confirm a draft.",
    );
  if (action === "publish")
    requireThat(
      result.status === "published" &&
        result.current === true &&
        Array.isArray(remote.artifact_bytes_checked) &&
        isDeepStrictEqual(
          remote.artifact_bytes_checked
            .map(({ url, sha256, size_bytes }) =>
              JSON.stringify([url, sha256, size_bytes]),
            )
            .sort(),
          plan.artifacts
            .map(({ url, sha256, size_bytes }) =>
              JSON.stringify([url, sha256, size_bytes]),
            )
            .sort(),
        ),
      "Root publish did not confirm this release as current; reconcile state before retrying.",
    );
  return result;
}

function normalizeArtifacts(artifacts) {
  return artifacts
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
}

export async function runCloudRelease(
  args,
  {
    fetchImpl = globalThis.fetch,
    command = runCommand,
    createPlan = createReleasePlan,
    serviceMetadata = createServiceReleaseMetadata,
    verifyPublic = verifyPublicReleaseArtifacts,
    root = ROOT,
    report = (message) => process.stderr.write(message + "\n"),
  } = {},
) {
  const options = parseCloudArguments(args);
  root = ordinaryDirectory(root, "Client checkout");
  const config = loadOperatorConfig(options.operatorConfigPath);
  const updaterConfigPath = ordinaryFile(
    join(root, "config", `osir-updater-${options.channel}.json`),
    65536,
    "Public updater configuration",
  );
  const prebuilt = options.buildDirectory
    ? ordinaryDirectory(options.buildDirectory, "Prebuilt directory")
    : undefined;
  const buildScript = ordinaryFile(
    join(root, "scripts/osir-release-local.mjs"),
    1024 * 1024,
    "Local release entry",
  );
  const makePlan = (buildDirectory) => {
    const plan = createPlan({
      channel: options.channel,
      buildDirectory,
      updaterConfigPath,
    });
    requireThat(
      plan.artifacts.some((artifact) => artifact.kind === "installer") &&
        plan.artifacts
          .filter((artifact) => artifact.kind === "installer")
          .every((artifact) => artifact.file_name.endsWith(".dmg")),
      "Cloud releases require the verified DMG installer before registration.",
    );
    return plan;
  };
  if (!options.publish) {
    return {
      schema_version: 1,
      mode: "dry-plan",
      publish: false,
      channel: options.channel,
      build_required: !prebuilt,
      ...(prebuilt
        ? { build_directory: prebuilt, plan: makePlan(prebuilt) }
        : { build_command: [process.execPath, buildScript, options.channel] }),
      operator: {
        server_checkout: config.serverCheckout,
        binary_sha256: config.binarySha256,
        live_release: config.liveRelease,
        live_sha: config.liveSha,
      },
      expected_current:
        "read from same-origin public current at execution start",
      steps: [
        "read-current",
        "build-or-validate-prebuilt",
        "inspect-live-and-draft",
        "register-draft",
        "upload-immutable-artifacts",
        "verify-full-public-bytes",
        "publish-with-expected-current",
        "inspect-and-read-current-feed-latest",
      ],
      note: "No build, network request or remote write was performed. Add --publish to execute once.",
    };
  }
  const outputRoot = join(root, "output/release-cloud");
  if (!lstatSafe(outputRoot))
    mkdirSync(outputRoot, { recursive: true, mode: 0o700 });
  const output = ordinaryDirectory(outputRoot, "Release receipt directory");
  requireThat(
    within(realpathSync(root), output) &&
      (lstatSync(output).mode & 0o077) === 0,
    "Release receipt directory must have private permissions (0700).",
  );
  const runDirectory = mkdtempSync(
    join(output, `${options.channel}-${Date.now()}-`),
  );
  const receiptPath = join(runDirectory, "cloud-receipt.json");
  const state = {
    schema_version: 1,
    channel: options.channel,
    status: "running",
    started_at: new Date().toISOString(),
    operator: {
      binary_sha256: config.binarySha256,
      live_release: config.liveRelease,
      live_sha: config.liveSha,
    },
    steps: [],
  };
  let revision = 0;
  const persist = () => {
    const temp = join(runDirectory, `.receipt-${revision++}.json`);
    writeJson(temp, state);
    renameSync(temp, receiptPath);
  };
  persist();
  report(`OSIR release evidence: ${runDirectory}`);
  let publishAttempted = false;
  async function step(name, task) {
    const entry = {
      name,
      status: "running",
      started_at: new Date().toISOString(),
    };
    state.steps.push(entry);
    persist();
    report(`OSIR release: ${name}`);
    try {
      const value = await task(entry);
      entry.status = "complete";
      entry.finished_at = new Date().toISOString();
      persist();
      return value;
    } catch (error) {
      entry.status = "failed";
      entry.finished_at = new Date().toISOString();
      persist();
      throw error;
    }
  }
  try {
    const currentBefore = await step("read-current", async () => {
      const current = await readCurrent(fetchImpl, options.channel);
      writeJson(join(runDirectory, "current-before.json"), {
        current,
        observed_at: new Date().toISOString(),
      });
      return current;
    });
    const expectedCurrent = currentBefore?.release_id ?? null;
    state.expected_current_release_id = expectedCurrent;
    persist();
    const buildDirectory = await step(
      prebuilt ? "validate-prebuilt" : "build",
      async (entry) => {
        if (prebuilt) return prebuilt;
        const logPath = join(runDirectory, "build.log");
        entry.log_path = logPath;
        persist();
        const result = await command(
          process.execPath,
          [buildScript, options.channel],
          {
            cwd: root,
            env: buildEnvironment(),
            logPath,
            timeoutMs: 3 * 60 * 60 * 1000,
          },
        );
        const outputLines = result.stdout
          .split(/\r?\n/)
          .filter((line) =>
            BUILD_PREFIXES.some((prefix) => line.startsWith(prefix)),
          );
        requireThat(
          outputLines.length === 1,
          "Build did not emit exactly one verified distribution path; inspect the build log.",
        );
        const path = ordinaryDirectory(
          outputLines[0]
            .slice(
              BUILD_PREFIXES.find((prefix) => outputLines[0].startsWith(prefix))
                .length,
            )
            .trim(),
          "Verified build output",
        );
        const mode = outputLines[0].startsWith(BUILD_PREFIXES[1])
          ? "release-self-signed-distribution"
          : "release-adhoc-distribution";
        const releaseRoot = resolve(
          root,
          "release/osir",
          options.channel,
          mode,
        );
        requireThat(
          within(releaseRoot, path) && path !== releaseRoot,
          "Build output is outside this channel's ad-hoc distribution directory.",
        );
        return path;
      },
    );
    state.build_directory = buildDirectory;
    persist();
    const plan = await step("plan-and-metadata", async () =>
      makePlan(buildDirectory),
    );
    const metadata = serviceMetadata(plan);
    const planPath = join(runDirectory, "plan.json"),
      metadataPath = join(runDirectory, "metadata.json");
    writeJson(planPath, plan);
    writeJson(metadataPath, metadata);
    const metadataSha = inspectArtifact(metadataPath).sha256;
    state.release_id = plan.release_id;
    state.metadata_sha256 = metadataSha;
    persist();
    const runId = `osir-cloud-${options.channel}-${Date.now()}-${randomBytes(5).toString("hex")}`;
    const assertPlan = () =>
      requireThat(
        isDeepStrictEqual(makePlan(buildDirectory), plan),
        "Build artifacts/configuration changed after planning.",
      );
    async function rootOperation(action, suffix, verificationPath) {
      assertToolBindings(config);
      const reviewId = runId + "-" + suffix;
      const operationReceipt = join(runDirectory, suffix + ".json");
      const logPath = join(runDirectory, suffix + ".log");
      const argv = [
        config.tools.ops.path,
        "--binary",
        config.binaryPath,
        "--binary-sha256",
        config.binarySha256,
        "--metadata",
        metadataPath,
        "--metadata-sha256",
        metadataSha,
        "--live-release",
        config.liveRelease,
        "--live-sha",
        config.liveSha,
        "--review-id",
        reviewId,
        "--action",
        action,
        "--receipt",
        operationReceipt,
      ];
      if (action === "inspect") argv.push("--remote");
      else argv.push("--execute");
      let verificationSha;
      if (action === "publish") {
        verificationSha = inspectArtifact(verificationPath).sha256;
        argv.push(
          "--expected-current",
          expectedCurrent ?? "none",
          "--verification-receipt",
          verificationPath,
        );
      }
      state.operation_receipts = {
        ...state.operation_receipts,
        [suffix]: operationReceipt,
      };
      persist();
      await command(PYTHON, argv, {
        cwd: config.serverCheckout,
        env: pythonEnvironment(),
        logPath,
      });
      return assertOperationReceipt(operationReceipt, {
        config,
        plan,
        action,
        reviewId,
        metadataSha,
        expectedCurrent,
        verificationSha,
      });
    }
    await step("inspect-live-and-draft", async () => {
      const inspected = await rootOperation("inspect", "inspect-before");
      requireThat(
        inspected.observed_current_release_id === expectedCurrent,
        "Current release changed during build; stop and review before registering.",
      );
      requireThat(
        ["not_registered", "draft"].includes(inspected.status),
        "This release is already published or withdrawn; use a new version.",
      );
    });
    await step("register-draft", async () => {
      assertPlan();
      const registered = await rootOperation("register", "register");
      requireThat(
        registered.observed_current_release_id === expectedCurrent,
        "Current release changed before registration completed; stop before upload.",
      );
    });
    await step("upload-immutable-artifacts", async (entry) => {
      assertPlan();
      assertToolBindings(config);
      const logPath = join(runDirectory, "upload.log");
      entry.log_path = logPath;
      persist();
      const result = await command(
        PYTHON,
        [config.tools.upload.path, planPath, buildDirectory, "--upload"],
        {
          cwd: config.serverCheckout,
          env: pythonEnvironment(),
          logPath,
        },
      );
      const line = result.stdout
        .split(/\r?\n/)
        .filter((value) => value.trim().startsWith("{"))
        .at(-1);
      const uploaded = line ? JSON.parse(line) : null;
      requireThat(
        uploaded?.release_id === plan.release_id &&
          uploaded.immutable === true &&
          uploaded.uploaded ===
            new Set(plan.artifacts.map((artifact) => artifact.file_name)).size,
        "Upload did not confirm every immutable artifact; inspect its log before retrying.",
      );
      writeJson(join(runDirectory, "upload-result.json"), uploaded);
    });
    const verificationPath = join(runDirectory, "public-verification.json");
    await step("verify-full-public-bytes", async () => {
      assertPlan();
      writeJson(
        verificationPath,
        await verifyPublic(plan, { fetchImpl, metadataPath }),
      );
    });
    await step("publish-with-expected-current", async () => {
      assertPlan();
      const now = await readCurrent(fetchImpl, options.channel);
      requireThat(
        (now?.release_id ?? null) === expectedCurrent,
        "Current release changed before publish; reconcile before retrying.",
      );
      publishAttempted = true;
      state.publish_attempted = true;
      persist();
      await rootOperation("publish", "publish", verificationPath);
    });
    await step("inspect-published", async () => {
      const inspected = await rootOperation("inspect", "inspect-after");
      requireThat(
        inspected.status === "published" &&
          inspected.current === true &&
          inspected.observed_current_release_id === plan.release_id,
        "Publication was submitted, but native inspection did not confirm current; do not republish automatically.",
      );
    });
    const latest = await step("read-current-feed-latest", async () => {
      const current = await readCurrent(fetchImpl, options.channel);
      requireThat(
        current &&
          ["release_id", "version", "channel", "source_sha", "notes"].every(
            (field) => current[field] === plan[field],
          ) &&
          Array.isArray(current.artifacts) &&
          isDeepStrictEqual(
            normalizeArtifacts(current.artifacts),
            normalizeArtifacts(plan.artifacts),
          ),
        "Published current release differs from the verified plan.",
      );
      const feed = await publicJson(
        fetchImpl,
        `${ORIGIN}/api/v1/osir-client/updater/${options.channel}.json`,
      );
      const updaters = plan.artifacts.filter(
        (artifact) => artifact.kind === "updater",
      );
      requireThat(
        feed?.version === plan.version &&
          feed.notes === plan.notes &&
          Number.isFinite(Date.parse(feed.pub_date)) &&
          feed.platforms &&
          isDeepStrictEqual(
            Object.keys(feed.platforms).sort(),
            updaters.map((artifact) => artifact.platform).sort(),
          ) &&
          updaters.every(
            (artifact) =>
              feed.platforms[artifact.platform]?.signature ===
                artifact.signature &&
              feed.platforms[artifact.platform]?.url ===
                `${ORIGIN}/api/v1/osir-client/download/${plan.release_id}/${artifact.platform}/updater`,
          ),
        "Published updater feed differs from the verified release.",
      );
      writeJson(join(runDirectory, "current-after.json"), current);
      writeJson(join(runDirectory, "updater-after.json"), feed);
      const links = [];
      for (const artifact of plan.artifacts.filter(
        (value) => value.kind === "installer",
      )) {
        requireThat(
          artifact.file_name.endsWith(".dmg"),
          "Cloud release requires the verified DMG installer.",
        );
        const url = `${ORIGIN}/api/v1/osir-client/download/latest/${options.channel}/${artifact.platform}/installer`;
        const response = await fetchImpl(url, {
          method: "HEAD",
          redirect: "manual",
          cache: "no-store",
          signal: AbortSignal.timeout(15000),
          headers: { "cache-control": "no-cache" },
        });
        requireThat(
          response.url === url &&
            response.status === 302 &&
            response.headers.get("location") === artifact.url,
          "Stable latest download does not resolve to this verified installer.",
        );
        links.push({
          platform: artifact.platform,
          latest_url: url,
          immutable_url: artifact.url,
          sha256: artifact.sha256,
        });
      }
      writeJson(join(runDirectory, "latest-downloads.json"), links);
      return links;
    });
    state.status = "published";
    state.finished_at = new Date().toISOString();
    state.latest_downloads = latest;
    persist();
    return {
      release_id: plan.release_id,
      status: "published",
      public_readback_verified: true,
      receipt_path: receiptPath,
      latest_downloads: latest,
    };
  } catch (error) {
    state.status = publishAttempted
      ? "publication-needs-reconciliation"
      : "stopped";
    state.finished_at = new Date().toISOString();
    persist();
    const reason =
      error instanceof Error ? error.message : "Release step failed.";
    throw new Error(
      `${reason}\nReceipts/logs: ${runDirectory}.${publishAttempted ? " Publication was attempted; inspect cloud state before retrying. No automatic republish or remote cleanup was performed." : " No automatic retry or remote artifact cleanup was performed."}`,
    );
  }
}

function lstatSafe(path) {
  try {
    return lstatSync(path);
  } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw error;
  }
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  runCloudRelease(process.argv.slice(2))
    .then((result) =>
      process.stdout.write(JSON.stringify(result, null, 2) + "\n"),
    )
    .catch((error) => {
      process.stderr.write(
        "OSIR cloud release stopped: " + error.message + "\n",
      );
      process.exitCode = 1;
    });
}
