//! Read revision only from the natively detected app's packed build metadata.
//! Legacy packages omit revision; invalid or external metadata is an
//! error, never revision zero. No app execution or per-user config reads.
use super::manifest;
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub(super) fn needs_install(
    current: &str,
    current_revision: u32,
    release: &str,
    release_revision: u32,
) -> Result<bool, String> {
    if current_revision > 65535 || release_revision > 65535 {
        return Err("应用修订信息无效，请修复安装".into());
    }
    Ok((manifest::version(release)?, release_revision)
        > (manifest::version(current)?, current_revision))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Product {
    name: String,
    product_name: String,
    main: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildInfo {
    product_id: String,
    desktop_version: String,
    #[serde(default)]
    revision: u32,
}

fn no_links(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor).map_err(|_| "无法读取应用构建信息")?;
        if meta.is_symlink() {
            return Err("应用构建信息不能使用链接".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err("应用构建信息不能使用重解析路径".into());
            }
        }
    }
    Ok(())
}

pub(super) fn installed(app: &Path, expected_version: &str) -> Result<u32, String> {
    let relative = if cfg!(target_os = "windows") {
        "resources/app.asar"
    } else {
        "Contents/Resources/app.asar"
    };
    read_app(app, expected_version, relative)
}

fn read_app(app: &Path, expected: &str, relative: &str) -> Result<u32, String> {
    if !app.is_absolute()
        || app
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir))
    {
        return Err("应用安装路径无效".into());
    }
    manifest::version(expected)?;
    let path = app.join(relative);
    no_links(&path)?;
    let mut file = File::open(&path).map_err(|_| "无法读取应用构建信息")?;
    let before = file.metadata().map_err(|_| "无法读取应用构建信息")?;
    if !before.is_file() {
        return Err("应用构建信息不是有效文件".into());
    }
    let mut prefix = [0; 16];
    file.read_exact(&mut prefix)
        .map_err(|_| "应用构建信息不完整")?;
    let word = |at| u32::from_le_bytes(prefix[at..at + 4].try_into().unwrap()) as u64;
    let (header_size, json_size) = (word(4), word(12));
    if word(0) != 4
        || !(8..=8 * 1024 * 1024).contains(&header_size)
        || json_size < 2
        || json_size > header_size - 8
        || header_size + 8 > before.len()
    {
        return Err("应用构建信息结构无效".into());
    }
    let mut bytes = vec![0; json_size as usize];
    file.read_exact(&mut bytes)
        .map_err(|_| "应用构建信息不完整")?;
    let header: Value = serde_json::from_slice(&bytes).map_err(|_| "应用构建信息无效")?;
    let package = entry(
        &mut file,
        &header,
        &["package.json"],
        header_size,
        before.len(),
    )?
    .ok_or("应用产品信息缺失")?;
    let product: Product = serde_json::from_slice(&package).map_err(|_| "应用产品信息无效")?;
    if product.name != "osir-anywhere-desktop-workbench"
        || product.product_name != "OSIR Anywhere"
        || product.main != "dist/electron/main.js"
        || product.version != expected
    {
        return Err("应用产品或版本与本机检测不符，请修复安装".into());
    }
    let info = entry(
        &mut file,
        &header,
        &["dist", "build-info.json"],
        header_size,
        before.len(),
    )?;
    let revision = if let Some(info) = info {
        let value: Value = serde_json::from_slice(&info).map_err(|_| "应用修订信息无效")?;
        let object = value.as_object().ok_or("应用修订信息无效")?;
        // Original packages recorded only serverVersion. A revision-bearing
        // build must identify this same product and desktop version.
        if object.len() == 1 && object.contains_key("serverVersion") {
            manifest::version(object["serverVersion"].as_str().ok_or("应用构建版本无效")?)?;
            0
        } else {
            let info: BuildInfo = serde_json::from_value(value).map_err(|_| "应用修订信息无效")?;
            if info.product_id != "osir-anywhere"
                || info.desktop_version != expected
                || info.revision > 65535
            {
                return Err("应用修订信息与本机检测不符，请修复安装".into());
            }
            info.revision
        }
    } else {
        0
    };
    let after = file.metadata().map_err(|_| "无法复查应用构建信息")?;
    no_links(&path)?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("应用构建信息已变化，请重新检测".into());
    }
    Ok(revision)
}

fn entry(
    file: &mut File,
    header: &Value,
    parts: &[&str],
    header_size: u64,
    file_size: u64,
) -> Result<Option<Vec<u8>>, String> {
    let mut row = header;
    for part in parts {
        let files = row
            .get("files")
            .and_then(Value::as_object)
            .ok_or("应用目录信息无效")?;
        let Some(next) = files.get(*part) else {
            return Ok(None);
        };
        if !next.is_object()
            || next.get("link").is_some()
            || next
                .get("unpacked")
                .is_some_and(|value| value != &Value::Bool(false))
        {
            return Err("应用构建信息不能使用外部文件或链接".into());
        }
        row = next;
    }
    let offset = row
        .get("offset")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("应用构建信息偏移无效")?;
    let length = row
        .get("size")
        .and_then(Value::as_u64)
        .filter(|n| (2..=256 * 1024).contains(n))
        .ok_or("应用构建信息大小无效")?;
    let start = (8 + header_size)
        .checked_add(offset)
        .ok_or("应用构建信息边界无效")?;
    if start
        .checked_add(length)
        .filter(|end| *end <= file_size)
        .is_none()
    {
        return Err("应用构建信息超出包边界".into());
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|_| "无法读取应用构建信息")?;
    let mut bytes = vec![0; length as usize];
    file.read_exact(&mut bytes)
        .map_err(|_| "应用构建信息不完整")?;
    Ok(Some(bytes))
}

#[cfg(test)]
#[path = "osir_anywhere_install_revision_tests.rs"]
mod tests;
