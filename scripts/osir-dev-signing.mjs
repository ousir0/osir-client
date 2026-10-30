#!/usr/bin/env node
// Local DEV signing metadata and restricted dedicated-signing-keychain prep.
// Never create certificates, change trust, or alter search/default settings here.
import {
  chmodSync,
  closeSync,
  existsSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, isAbsolute, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes } from "node:crypto";
import { userInfo } from "node:os";

const root = fileURLToPath(new URL("../", import.meta.url));
const managedDirectory = resolve(root, ".osir-dev-home/.dev-signing");
export const MANAGED_DEV_SIGNING = Object.freeze({
  directory: managedDirectory,
  keychain: resolve(managedDirectory, "signing.keychain-db"),
  passwordPath: resolve(managedDirectory, "keychain-password"),
  helperPath: resolve(managedDirectory, "keychain-helper"),
  certificatePath: resolve(managedDirectory, "certificate.pem"),
});
export const DEV_SIGNING_CONFIG_PATH = resolve(
  root,
  ".osir-dev-home/.dev-signing.json",
);

// Signing is an OS-user operation even when the app/tool configuration HOME is
// isolated. Change only the short-lived signing subprocess environment.
export function devSigningToolEnvironment(env = process.env) {
  const realUserHome = userInfo().homedir;
  if (!isAbsolute(realUserHome) || /[\x00-\x1f\x7f]/.test(realUserHome))
    throw new Error("无法确定本机开发签名的系统用户目录。");
  return { ...env, HOME: realUserHome, CFFIXED_USER_HOME: realUserHome };
}

export function parseKeychainSearchList(output) {
  try {
    if (typeof output !== "string" || output.length > 128 * 1024)
      throw new Error();
    return output
      .split(/\r?\n/)
      .filter((line) => line.trim())
      .map((line) => {
        const path = JSON.parse(line.trim());
        if (
          typeof path !== "string" ||
          !isAbsolute(path) ||
          /[\x00-\x1f\x7f]/.test(path)
        )
          throw new Error();
        return path;
      });
  } catch {
    throw new Error("无法安全解析签名钥匙串搜索列表，未修改系统设置。");
  }
}

function verifySigningKeychainRegistration(
  signing,
  { run = execFileSync } = {},
) {
  if (!signing.keychain) return;
  let paths;
  try {
    paths = parseKeychainSearchList(
      run("/usr/bin/security", ["list-keychains", "-d", "user"], {
        env: devSigningToolEnvironment(),
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
        timeout: 3000,
        maxBuffer: 128 * 1024,
      }),
    );
  } catch {
    throw new Error("无法只读验证 DEV 签名钥匙串登记状态，未修改系统设置。");
  }
  if (!paths.includes(signing.keychain))
    throw new Error(
      "DEV 签名钥匙串尚未登记；请明确执行签名 setup，普通启动不会修改搜索列表。",
    );
}

function metadata(value) {
  if (
    !value ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    Object.keys(value).some((key) => !["identity", "keychain"].includes(key)) ||
    typeof value.identity !== "string" ||
    !/^[a-fA-F0-9]{40}$/.test(value.identity)
  ) {
    throw new Error("DEV 签名身份必须为有效的 40 位 SHA-1 指纹。");
  }
  const result = { identity: value.identity.toUpperCase() };
  if (value.keychain !== undefined) {
    if (
      typeof value.keychain !== "string" ||
      !isAbsolute(value.keychain) ||
      /[\x00-\x1f\x7f]/.test(value.keychain)
    ) {
      throw new Error("DEV 签名钥匙串必须使用有效的绝对路径。");
    }
    result.keychain = resolve(value.keychain);
  }
  return result;
}

// Metadata is already selected/validated by the caller. Verification must not
// unlock a keychain or change a reusable app merely to inspect its signature.
export function verifyDevAppSigning(app, signing, { run = execFileSync } = {}) {
  try {
    if (
      typeof app !== "string" ||
      !isAbsolute(app) ||
      /[\x00-\x1f\x7f]/.test(app)
    ) {
      throw new Error();
    }
    const args = ["--verify", "--strict"];
    if (signing?.identity !== "-") {
      const selected = metadata({ identity: signing?.identity });
      args.push(
        `-R=identifier "com.osir.client.dev" and certificate leaf = H"${selected.identity}"`,
      );
    }
    args.push(app);
    run("/usr/bin/codesign", args, {
      env: devSigningToolEnvironment(),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      timeout: 10000,
      maxBuffer: 16 * 1024,
    });
    return true;
  } catch {
    throw new Error(
      "现有 DEV 应用签名与当前配置不一致或无法验证，请运行 pnpm dev:osir 重建后再使用 start:osir。",
    );
  }
}

export function readDevSigningConfig({
  configPath = DEV_SIGNING_CONFIG_PATH,
} = {}) {
  let stat;
  try {
    stat = lstatSync(configPath);
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw new Error("无法读取 DEV 签名配置。");
  }
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 4096) {
    throw new Error("DEV 签名配置必须为本机普通文件，不能使用链接。");
  }
  if ((stat.mode & 0o077) !== 0) {
    throw new Error("DEV 签名配置权限必须为 0600，请修正后重试。");
  }
  let value;
  try {
    value = JSON.parse(readFileSync(configPath, "utf8"));
  } catch {
    throw new Error("DEV 签名配置格式无效，请重新保存签名身份。");
  }
  return metadata(value);
}

export function validateDevSigningIdentity(
  value,
  { run = execFileSync, platform = process.platform } = {},
) {
  const selected = metadata(value);
  if (platform !== "darwin") {
    throw new Error("DEV 原生签名身份仅支持在 macOS 上验证。");
  }
  if (selected.keychain) {
    let stat;
    try {
      stat = lstatSync(selected.keychain);
    } catch {
      throw new Error("配置的 DEV 签名钥匙串不可用。");
    }
    if (!stat.isFile() || stat.isSymbolicLink()) {
      throw new Error("配置的 DEV 签名钥匙串必须为普通文件，不能使用链接。");
    }
  }
  let output;
  try {
    output = run(
      "/usr/bin/security",
      [
        "find-identity",
        "-v",
        "-p",
        "codesigning",
        ...(selected.keychain ? [selected.keychain] : []),
      ],
      {
        env: devSigningToolEnvironment(),
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
        timeout: 3000,
        maxBuffer: 128 * 1024,
      },
    );
  } catch {
    throw new Error(
      "无法验证 DEV 签名身份；未修改钥匙串，也不会退回临时签名。",
    );
  }
  const identities = String(output)
    .split(/\r?\n/)
    .flatMap((line) => {
      const match = line.match(/^\s*\d+\)\s+([a-fA-F0-9]{40})\s+"[^"]*"\s*$/);
      return match ? [match[1].toUpperCase()] : [];
    });
  if (!identities.includes(selected.identity)) {
    throw new Error(
      "配置的 DEV 签名身份不存在、已过期或不受信任；请提供有效身份。不会退回临时签名。",
    );
  }
  return selected;
}

function selectDevSigning({
  env = process.env,
  configPath = DEV_SIGNING_CONFIG_PATH,
} = {}) {
  if (env.OSIR_DEV_SIGNING_IDENTITY !== undefined) {
    const selected = metadata({
      identity: env.OSIR_DEV_SIGNING_IDENTITY,
      ...(env.OSIR_DEV_SIGNING_KEYCHAIN !== undefined
        ? { keychain: env.OSIR_DEV_SIGNING_KEYCHAIN }
        : {}),
    });
    return { ...selected, source: "environment" };
  }
  if (env.OSIR_DEV_SIGNING_KEYCHAIN !== undefined) {
    throw new Error("设置 DEV 签名钥匙串时必须同时提供签名身份指纹。");
  }
  const configured = readDevSigningConfig({ configPath });
  if (!configured) {
    if (
      existsSync(
        resolve(dirname(configPath), ".dev-signing/signing.keychain-db"),
      )
    ) {
      throw new Error(
        "本机 DEV 签名配置尚未完成；请完成系统确认并重新执行签名 setup，不会退回临时签名。",
      );
    }
    return { identity: "-", source: "adhoc" };
  }
  return { ...configured, source: "config" };
}

export function resolveDevSigning(options = {}) {
  const selected = selectDevSigning(options);
  if (selected.source === "adhoc") return selected;
  const { source, ...identity } = selected;
  return { ...validateDevSigningIdentity(identity, options), source };
}

export function unlockManagedDevSigning(
  signing,
  { run = execFileSync, managed = MANAGED_DEV_SIGNING } = {},
) {
  // External identities and arbitrary keychains are never unlocked here.
  if (signing.keychain !== managed.keychain) return false;
  for (const path of [
    managed.keychain,
    managed.passwordPath,
    managed.helperPath,
  ]) {
    let stat;
    try {
      stat = lstatSync(path);
    } catch {
      throw new Error(
        "本机 DEV 签名资料不完整，请重新执行明确的签名配置操作。",
      );
    }
    if (
      !stat.isFile() ||
      stat.isSymbolicLink() ||
      (stat.mode & 0o077) !== 0 ||
      (process.getuid && stat.uid !== process.getuid())
    ) {
      throw new Error("本机 DEV 签名资料类型、所有者或权限无效。");
    }
  }
  const password = readFileSync(managed.passwordPath, "utf8");
  if (!/^[a-zA-Z0-9_-]{32,128}$/.test(password)) {
    throw new Error("本机 DEV 签名解锁资料无效。");
  }
  try {
    run(managed.helperPath, [], {
      env: devSigningToolEnvironment(),
      input: JSON.stringify({
        operation: "unlock",
        keychain: managed.keychain,
        password,
      }),
      encoding: "utf8",
      stdio: ["pipe", "pipe", "pipe"],
      timeout: 3000,
      maxBuffer: 4096,
    });
  } catch {
    throw new Error(
      "本机 DEV 签名钥匙串无法静默解锁；不会显示密码框或退回临时签名。",
    );
  }
  return true;
}

export function prepareDevSigning(options = {}) {
  const selected = selectDevSigning(options);
  if (selected.source === "adhoc") return selected;
  verifySigningKeychainRegistration(selected, options);
  unlockManagedDevSigning(selected, options);
  const { source, ...identity } = selected;
  return { ...validateDevSigningIdentity(identity, options), source };
}

export function saveDevSigningConfig(
  value,
  {
    configPath = DEV_SIGNING_CONFIG_PATH,
    run = execFileSync,
    platform = process.platform,
  } = {},
) {
  const selected = validateDevSigningIdentity(value, { run, platform });
  const directory = dirname(configPath);
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const parent = lstatSync(directory);
  if (!parent.isDirectory() || parent.isSymbolicLink()) {
    throw new Error("DEV 签名配置目录不能使用链接。");
  }
  try {
    const existing = lstatSync(configPath);
    if (!existing.isFile() || existing.isSymbolicLink()) {
      throw new Error("DEV 签名配置目标不能使用链接。");
    }
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const temporary = resolve(
    directory,
    `.dev-signing-${randomBytes(12).toString("hex")}.tmp`,
  );
  let descriptor;
  try {
    descriptor = openSync(temporary, "wx", 0o600);
    writeFileSync(descriptor, `${JSON.stringify(selected, null, 2)}\n`);
    closeSync(descriptor);
    descriptor = undefined;
    renameSync(temporary, configPath);
    chmodSync(configPath, 0o600);
  } finally {
    if (descriptor !== undefined) closeSync(descriptor);
    rmSync(temporary, { force: true });
  }
  return selected;
}

function main(args) {
  if (args.length === 1 && args[0] === "--help") {
    console.log(
      "用法：node scripts/osir-dev-signing.mjs --identity <SHA1> [--keychain <绝对路径>]\n      node scripts/osir-dev-signing.mjs --check\n只保存本机签名元数据；不会创建证书、修改信任或解锁钥匙串。",
    );
    return;
  }
  if (args.length === 1 && args[0] === "--check") {
    const selected = resolveDevSigning();
    console.log(
      selected.source === "adhoc"
        ? "未配置稳定 DEV 签名身份；原生重建后可能需要重新确认钥匙串访问。"
        : `DEV 签名身份已验证（${selected.source}）：${selected.identity}`,
    );
    return;
  }
  const value = {};
  for (let index = 0; index < args.length; index += 2) {
    const name = args[index];
    if (
      !["--identity", "--keychain"].includes(name) ||
      !args[index + 1] ||
      value[name.slice(2)] !== undefined
    ) {
      throw new Error(
        "参数无效，请使用 --identity <SHA1> [--keychain <绝对路径>]。",
      );
    }
    value[name.slice(2)] = args[index + 1];
  }
  const saved = saveDevSigningConfig(value);
  console.log(`DEV 签名身份已保存：${saved.identity}`);
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
