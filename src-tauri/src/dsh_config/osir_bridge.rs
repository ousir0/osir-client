//! OSIR's private Host bundle. Installation belongs to the official DSH CLI;
//! only profile overrides are written here. Never install an upstream/latest package.
use super::*;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read};

const PACKAGE: &str = "@osir/anywhere-dsh-bridge";
const VERSION: &str = "2.2.1";
const ID: &str = "osir-anywhere-dsh-bridge";
const UPSTREAM_PACKAGE: &str = "@agents-anywhere/dsh-bridge-next";
const UPSTREAM_ID: &str = "agents-anywhere-bridge-next";
const LIMIT: u64 = 32 * 1024 * 1024;

pub(crate) fn enabled() -> bool {
    enabled_for(
        crate::distribution::is_osir(),
        crate::distribution::is_isolated(),
    )
}

fn enabled_for(osir: bool, isolated: bool) -> bool {
    osir && !isolated
}

fn failure(detail: &str) -> AppError {
    AppError::Config(format!("DeepSeek Harness 的 OSIR 联络插件{detail}"))
}

/// Electron main.ts sets userData = appData/OSIR Anywhere on every platform.
/// dirs::config_dir matches Electron appData (Application Support / APPDATA / XDG).
/// Packaged OSIR Anywhere ignores AGENT_CONNECTOR_DATA_DIR, as does this path.
fn connector_root(app_data: Option<PathBuf>) -> Result<PathBuf, AppError> {
    let app_data = app_data
        .filter(|p| p.is_absolute())
        .ok_or_else(|| failure("无法定位 OSIR Anywhere 数据目录，请检查系统应用数据目录"))?;
    Ok(app_data.join("OSIR Anywhere").join("connector"))
}

/// The caller first checks the native DSH application identity. The ordinary
/// npm dsh CLI deliberately denies the reserved desktop profile. Desktop's
/// shipped wrapper enables it via dsh-desktop-host's manageDesktopProfile flag.
/// Never fall back to PATH, a standalone CLI or a renderer-supplied executable.
#[cfg(target_os = "macos")]
pub(crate) fn desktop_cli(checked_bundle: &Path) -> Result<PathBuf, AppError> {
    use std::os::unix::fs::PermissionsExt;
    let missing = || failure("缺少官方桌面安装入口，请修复 DeepSeek Harness 安装");
    if !checked_bundle.is_absolute() {
        return Err(missing());
    }
    let bundle = fs::canonicalize(checked_bundle).map_err(|_| missing())?;
    let path = bundle.join("Contents/Resources/runtime/cli/bin/dsh");
    let meta = fs::symlink_metadata(&path).map_err(|_| missing())?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.permissions().mode() & 0o111 == 0
        || !fs::canonicalize(&path).is_ok_and(|real| real.starts_with(&bundle))
    {
        return Err(missing());
    }
    let bytes = bounded(&path, 16 * 1024)?;
    let script = std::str::from_utf8(&bytes).map_err(|_| missing())?;
    if !script.starts_with("#!/bin/sh\n")
        || !script.contains("ELECTRON_RUN_AS_NODE=1")
        || !script.contains("app.asar/dsh/node_modules/@deepseek-ai/dsh-desktop-host/lib/cli.js")
    {
        return Err(missing());
    }
    Ok(path)
}

fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, AppError> {
    let mut file =
        fs::File::open(path).map_err(|_| failure("缺失或无法读取，请修复 OSIR Anywhere 安装"))?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("无法读取，请修复安装"))?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(failure("文件格式或大小不兼容，请修复安装"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("无法读取，请修复安装"))?;
    if bytes.len() as u64 > limit {
        return Err(failure("文件过大，请修复安装"));
    }
    Ok(bytes)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageInfo {
    schema_version: u32,
    name: String,
    version: String,
    sha256: String,
    size: u64,
}

struct Artifact {
    bytes: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
    defaults: Yaml,
}

fn relative_file(name: &str) -> bool {
    !name.contains('\\')
        && !name.contains('\0')
        && !name.is_empty()
        && Path::new(name)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Read regular members without extracting or executing the package. The fixed
/// Host artifact uses short ustar paths; exotic/link archives fail closed.
fn archive_files(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, AppError> {
    let invalid = || failure("包格式不兼容，请使用 OSIR Anywhere 随附的固定版本");
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take(LIMIT + 1)
        .read_to_end(&mut tar)
        .map_err(|_| invalid())?;
    if tar.len() as u64 > LIMIT {
        return Err(invalid());
    }
    let mut files = BTreeMap::new();
    let mut offset = 0;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512];
        if header.iter().all(|b| *b == 0) {
            return Ok(files);
        }
        let string = |range: std::ops::Range<usize>| -> Result<&str, AppError> {
            let field = &header[range];
            std::str::from_utf8(field.split(|b| *b == 0).next().unwrap()).map_err(|_| invalid())
        };
        let octal = |range| -> Result<usize, AppError> {
            usize::from_str_radix(string(range)?.trim(), 8).map_err(|_| invalid())
        };
        let sum: usize = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    *b as usize
                }
            })
            .sum();
        if octal(148..156)? != sum {
            return Err(invalid());
        }
        let name = string(0..100)?;
        let size = octal(124..136)?;
        if !string(345..500)?.is_empty() || size as u64 > LIMIT {
            return Err(invalid());
        }
        let start = offset + 512;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= tar.len())
            .ok_or_else(invalid)?;
        if header[156] == b'5' && size == 0 {
            // npm may include explicit directories; never materialize them.
        } else {
            let name = name
                .strip_prefix("package/")
                .filter(|n| relative_file(n))
                .ok_or_else(invalid)?;
            if !matches!(header[156], 0 | b'0')
                || files
                    .insert(name.into(), tar[start..end].to_vec())
                    .is_some()
            {
                return Err(invalid());
            }
        }
        offset = start + size.div_ceil(512) * 512;
    }
    Err(invalid())
}

impl Artifact {
    fn load(resources: &Path) -> Result<Self, AppError> {
        if !resources.is_absolute() {
            return Err(failure("资源目录无效"));
        }
        let directory = resources.join("dsh-bridge");
        let path = directory.join("osir-dsh-bridge.tgz");
        let info: PackageInfo =
            serde_json::from_slice(&bounded(&directory.join("package-info.json"), 16 * 1024)?)
                .map_err(|_| failure("资源信息不兼容，请升级 OSIR Anywhere"))?;
        if info.schema_version != 1
            || info.name != PACKAGE
            || info.version != VERSION
            || info.size == 0
            || info.size > LIMIT
        {
            return Err(failure(
                "版本不兼容，请使用 OSIR Anywhere 随附的 2.2.1 私有插件",
            ));
        }
        let bytes = bounded(&path, LIMIT)?;
        if bytes.len() as u64 != info.size || format!("{:x}", Sha256::digest(&bytes)) != info.sha256
        {
            return Err(failure("资源校验失败，请修复 OSIR Anywhere 安装"));
        }
        // The caller supplies a native-discovered, identity-checked application.
        // Reject resource links escaping it; no renderer/package path is accepted.
        let canonical = fs::canonicalize(resources).map_err(|_| failure("资源目录无效"))?;
        for resource in [&path, &directory.join("package-info.json")] {
            if !fs::canonicalize(resource).is_ok_and(|p| p.starts_with(&canonical)) {
                return Err(failure("资源路径超出应用目录"));
            }
        }
        let files = archive_files(&bytes)?;
        let manifest: Value = serde_json::from_slice(
            files
                .get("package.json")
                .ok_or_else(|| failure("包信息缺失"))?,
        )
        .map_err(|_| failure("包信息无效"))?;
        if manifest["name"] != PACKAGE
            || manifest["version"] != VERSION
            || manifest["main"] != "./lib/index.js"
            || manifest["dsh"]["bundle"]["patch"] != "./cordis.patch.yml"
            || manifest["dsh"].get("client").is_some()
            || !files.contains_key("lib/index.js")
        {
            return Err(failure("包身份或 Host 接口不兼容"));
        }
        let patch = std::str::from_utf8(
            files
                .get("cordis.patch.yml")
                .ok_or_else(|| failure("包配置缺失"))?,
        )
        .map_err(|_| failure("包配置无效"))?;
        let rows = entries(patch)?;
        let insert = rows
            .first()
            .and_then(|row| row["insert"].as_sequence())
            .filter(|_| rows.len() == 1)
            .filter(|insert| insert.len() == 1)
            .ok_or_else(|| failure("包配置不兼容"))?;
        let entry = &insert[0];
        if entry["id"].as_str() != Some(ID)
            || entry["name"].as_str() != Some(PACKAGE)
            || entry["disabled"].as_bool() == Some(true)
            || !entry["config"].is_mapping()
        {
            return Err(failure("包插件标识不兼容"));
        }
        let defaults = entry["config"].clone();
        Ok(Self {
            bytes,
            files,
            defaults,
        })
    }

    fn installed(&self, profile: &Path) -> Result<bool, AppError> {
        let manifest_path = profile.join("package.json");
        let manifest: Value = match fs::read(&manifest_path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| failure("profile 包信息无效"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(failure("profile 包信息无法读取")),
        };
        let active = manifest["dsh"]["profile"]["bundles"]
            .as_array()
            .is_some_and(|bundles| bundles.iter().any(|v| v == PACKAGE));
        let package = profile.join("node_modules").join(PACKAGE);
        if !package.exists() {
            if active {
                return Err(failure("已启用但文件缺失，请在 DSH 中修复安装"));
            }
            return Ok(false);
        }
        // Compare actual installed Host/bundle files with the verified fixed
        // artifact, rather than trusting a version number or npm latest.
        for (name, expected) in &self.files {
            if name == "package.json" || name == "cordis.patch.yml" || name.starts_with("lib/") {
                if bounded(&package.join(name), LIMIT)? != *expected {
                    return Err(failure("已安装版本或来源不兼容，请在 DSH 中修复后重试"));
                }
            }
        }
        Ok(active)
    }
}

fn references(value: &Yaml, id: &str, package: &str) -> bool {
    match value {
        Yaml::Sequence(items) => items.iter().any(|v| references(v, id, package)),
        Yaml::Mapping(map) => {
            value["id"].as_str() == Some(id)
                || value["name"].as_str() == Some(package)
                || map.values().any(|v| references(v, id, package))
        }
        _ => false,
    }
}

fn preflight(profiles: &[PathBuf], texts: &[String], home: &str) -> Result<(), AppError> {
    let home_rows = entries(home)?;
    let home_doc = Yaml::Sequence(home_rows.clone());
    if references(&home_doc, ID, PACKAGE) {
        return Err(failure("被全局配置覆盖，请先在 DSH 中处理该覆盖"));
    }
    for (path, text) in profiles.iter().zip(texts) {
        let manifest = path.parent().unwrap().join("package.json");
        let has_upstream_bundle = if manifest.exists() {
            let value: Value = serde_json::from_slice(&bounded(&manifest, 1024 * 1024)?)
                .map_err(|_| failure("profile 包信息无效"))?;
            value["dsh"]["profile"]["bundles"]
                .as_array()
                .is_some_and(|bundles| bundles.iter().any(|v| v == UPSTREAM_PACKAGE))
        } else {
            false
        };
        let profile_rows = entries(text)?;
        let mut disabled = false;
        let mut ambiguous = false;
        let mut seen = has_upstream_bundle;
        // A user can explicitly disable the old bundle without removing its
        // dependency. Only an unambiguous native disabled:true override counts.
        for rows in [&profile_rows, &home_rows] {
            let matches: Vec<_> = rows
                .iter()
                .filter(|row| references(row, UPSTREAM_ID, UPSTREAM_PACKAGE))
                .collect();
            if matches.len() > 1 {
                ambiguous = true;
            }
            for row in matches {
                seen = true;
                if row["id"].as_str() == Some(UPSTREAM_ID)
                    && row.get("insert").is_none()
                    && row
                        .get("name")
                        .is_none_or(|name| name.as_str() == Some(UPSTREAM_PACKAGE))
                {
                    if let Some(value) = row["disabled"].as_bool() {
                        disabled = value;
                    }
                } else {
                    ambiguous = true;
                }
            }
        }
        if seen && (!disabled || ambiguous) {
            return Err(failure(
                "与已配置的上游 Agents Anywhere 插件冲突，请先在 DSH 中停用上游插件；未自动删除",
            ));
        }
    }
    Ok(())
}

fn inject(text: &str, root: &Path, defaults: &Yaml) -> Result<String, AppError> {
    let root = root
        .to_str()
        .filter(|_| root.is_absolute())
        .ok_or_else(|| failure("Connector 目录无效"))?;
    let rows = entries(text)?;
    let mut found = None;
    for row in rows {
        if row["id"].as_str() == Some(ID) {
            if found.is_some() {
                return Err(failure("配置重复，请在 DSH 中修复"));
            }
            found = Some(row);
        } else if references(&row, ID, PACKAGE) {
            return Err(failure("使用了额外的插入配置，请先在 DSH 中合并配置"));
        }
    }
    let mut row = found.unwrap_or_else(|| serde_yaml::to_value(json!({"id":ID})).unwrap());
    if row["disabled"].as_bool() == Some(true)
        || row["enabled"].as_bool() == Some(false)
        || row.get("insert").is_some()
        || row
            .get("name")
            .is_some_and(|name| name.as_str() != Some(PACKAGE))
    {
        return Err(failure("已停用或标识冲突，请在 DSH 中确认后重试"));
    }
    // Include replaces config wholesale. Start from the effective config, so
    // a bundle default or user field is not lost when adding our one option.
    if row["config"].is_null() {
        row["config"] = defaults.clone();
    }
    row["config"]
        .as_mapping_mut()
        .ok_or_else(|| failure("配置格式不兼容"))?
        .insert("osirConnectorDataDir".into(), root.into());
    replace_entry(text, ID, Some(row))
}

/// Called only by an explicit production DSH Desktop launch. `resources` and
/// `cli` must come from native application/CLI identity discovery, never IPC.
/// Missing resources, incompatible installed packages and upstream conflicts
/// are errors; readiness is returned only after installation AND patch commit.
pub(crate) async fn prepare_desktop_bridge(
    resources: &Path,
    cli: &Path,
    running: bool,
) -> Result<(), AppError> {
    if !enabled() {
        return Ok(());
    }
    let root = connector_root(dirs::config_dir())?;
    prepare(resources, cli, running, &root).await
}

async fn prepare(resources: &Path, cli: &Path, running: bool, root: &Path) -> Result<(), AppError> {
    let artifact = Artifact::load(resources)?;
    let profiles = target_paths();
    let home = home_patch_path();
    let mut files = profiles.clone();
    files.push(home);
    validate_write_paths(&files)?;
    // Validate before the official CLI's side effects. It may initialize a new
    // profile patch; after installation, lock/re-read and merge the latest text.
    let before = files
        .iter()
        .map(|p| read_text(p))
        .collect::<Result<Vec<_>, _>>()?;
    preflight(
        &profiles,
        &before[..profiles.len()],
        &before[profiles.len()],
    )?;
    let desktop = profile_dir("desktop");
    let installed = artifact.installed(&desktop)?;
    // Validate every proposed override before allowing the CLI's side effects.
    let mut next = before.clone();
    for (i, path) in profiles.iter().enumerate() {
        if *path == profile_path("desktop") || artifact.installed(path.parent().unwrap())? {
            next[i] = inject(&before[i], root, &artifact.defaults)?;
        }
    }
    if running && (!installed || next != before) {
        return Err(failure("需要配置，请先关闭 DeepSeek Harness 再启动"));
    }
    if !installed {
        if !cli.is_absolute() || !cli.is_file() {
            return Err(failure("需要官方 dsh 命令，请先安装 DSH CLI"));
        }
        // pnpm persists the file: source in its manifest/lockfile. Keep an
        // immutable private copy, not a temporary path or an upgradeable app file.
        let tarball = data_dir()
            .join(".osir-plugin-artifacts")
            .join(format!("{:x}", Sha256::digest(&artifact.bytes)))
            .join("osir-dsh-bridge.tgz");
        match fs::symlink_metadata(&tarball) {
            Ok(meta)
                if meta.file_type().is_symlink() || bounded(&tarball, LIMIT)? != artifact.bytes =>
            {
                return Err(failure("本地固定包已变化，请检查安装缓存"))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                atomic_write_private(&tarball, &artifact.bytes)?
            }
            Err(_) => return Err(failure("本地固定包无法读取")),
        }
        install(
            cli,
            &tarball,
            &data_dir(),
            std::time::Duration::from_secs(120),
        )
        .await?;
        if !artifact.installed(&desktop)? {
            return Err(failure("安装后未被 desktop profile 启用"));
        }
    }
    edit_files_and_commit(
        &files,
        |texts| {
            preflight(&profiles, &texts[..profiles.len()], &texts[profiles.len()])?;
            let mut next = texts.to_vec();
            for (i, path) in profiles.iter().enumerate() {
                if *path == profile_path("desktop") || artifact.installed(path.parent().unwrap())? {
                    next[i] = inject(&texts[i], root, &artifact.defaults)?;
                }
            }
            if running && next != texts {
                return Err(failure("配置在启动期间已变化，请关闭 DSH 后重试"));
            }
            Ok(next)
        },
        || Ok(()),
    )?;
    Ok(())
}

async fn install(
    cli: &Path,
    tarball: &Path,
    home: &Path,
    timeout: std::time::Duration,
) -> Result<(), AppError> {
    let mut command = tokio::process::Command::new(cli);
    command
        .args(["plugin", "--profile", "desktop", "add"])
        .arg(tarball)
        .env("DSH_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| failure("无法调用官方安装命令，请检查 DSH CLI"))?;
    let pid = child.id();
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result.map_err(|_| failure("无法确认安装结果，请检查 DSH 插件状态"))?,
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = pid {
                // DSH spawns pnpm: stop the entire dedicated process group so
                // descendants cannot keep mutating files after a timeout.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(failure("安装超时，请在 DSH 中检查插件状态后重试"));
        }
    };
    if !status.success() {
        return Err(failure("安装失败，请在 DSH 中检查插件状态后重试"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "osir_bridge_tests.rs"]
mod tests;
