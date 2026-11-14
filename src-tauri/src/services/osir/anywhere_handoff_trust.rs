//! A release-signed bundle inventory authenticates the Mac handoff peer.
//! The operating system still owns permission to launch the application.
#![cfg(target_os = "macos")]

use base64::{engine::general_purpose::STANDARD, Engine};
use futures::StreamExt;
use rsa::{pkcs1v15, pkcs8::DecodePublicKey, signature::Verifier, traits::PublicKeyParts, RsaPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, os::unix::fs::MetadataExt, path::{Path, PathBuf}, time::Duration};

const URL: &str = "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/handoff-trust.json";
const KEY: &str = include_str!("../../../resources/products/osir-anywhere/release-public.pub");
const ERROR: &str = "无法验证应用版本，请通过 OSIR 客户端重新安装 Anywhere";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope { payload: String, signature: String }
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Trust {
    schema_version: u8, product_id: String, published_at: String, valid_until: String,
    builds: Vec<Build>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Build { product_id: String, version: String, release_id: String, platform: String, tree_sha256: String }

pub(crate) async fn fetch() -> Result<Trust, String> {
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10)).build().map_err(|_| ERROR)?;
    let response = http.get(URL).header("Accept", "application/json").send().await.map_err(|_| ERROR)?;
    if response.status() != 200 || response.url().as_str() != URL { return Err(ERROR.into()); }
    let mut stream = response.bytes_stream(); let mut bytes = Vec::new();
    while let Some(part) = stream.next().await {
        let part = part.map_err(|_| ERROR)?;
        if bytes.len() + part.len() > 256 * 1024 { return Err(ERROR.into()); }
        bytes.extend_from_slice(&part);
    }
    verify(&bytes, KEY, chrono::Utc::now().timestamp())
}

fn decode(value: &str) -> Result<Vec<u8>, String> {
    let bytes = STANDARD.decode(value).map_err(|_| ERROR)?;
    if value.len() > 180_000 || value.is_empty() || STANDARD.encode(&bytes) != value { return Err(ERROR.into()); }
    Ok(bytes)
}

fn verify(bytes: &[u8], key: &str, now: i64) -> Result<Trust, String> {
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| ERROR)?;
    let payload = decode(&envelope.payload)?; let signature = decode(&envelope.signature)?;
    let key = RsaPublicKey::from_public_key_pem(key).map_err(|_| ERROR)?;
    if !(2048..=8192).contains(&key.n().bits()) { return Err(ERROR.into()); }
    pkcs1v15::VerifyingKey::<Sha256>::new(key).verify(&payload,
        &pkcs1v15::Signature::try_from(signature.as_slice()).map_err(|_| ERROR)?).map_err(|_| ERROR)?;
    let trust: Trust = serde_json::from_slice(&payload).map_err(|_| ERROR)?;
    let published = chrono::DateTime::parse_from_rfc3339(&trust.published_at).map_err(|_| ERROR)?.timestamp();
    let expires = chrono::DateTime::parse_from_rfc3339(&trust.valid_until).map_err(|_| ERROR)?.timestamp();
    if trust.schema_version != 1 || trust.product_id != "osir-desktop-handoff-trust"
        || published > now + 300 || expires <= now || expires <= published || expires - published > 366 * 86400
        || trust.builds.is_empty() || trust.builds.len() > 128 { return Err(ERROR.into()); }
    let mut seen = std::collections::HashSet::new();
    for b in &trust.builds {
        let valid_version = regex::Regex::new(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(\+[1-9][0-9]{0,4})?$")
            .map_err(|_| ERROR)?.is_match(&b.version);
        if !matches!(b.product_id.as_str(), "osir-client" | "osir-anywhere") || b.platform != "darwin"
            || !valid_version || b.version.len() > 40 || b.release_id.is_empty() || b.release_id.len() > 128
            || !b.release_id.as_bytes()[0].is_ascii_alphanumeric() || b.release_id.contains("..")
            || !b.release_id.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || b.tree_sha256.len() != 64 || !b.tree_sha256.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || !seen.insert((&b.product_id, &b.tree_sha256)) { return Err(ERROR.into()); }
    }
    Ok(trust)
}

impl Trust {
    pub(crate) async fn verify_bundle(&self, root: &Path, product: &str) -> Result<(), String> {
        let root = root.to_owned(); let builds = self.builds.clone(); let product = product.to_owned();
        tokio::task::spawn_blocking(move || {
            let hash = fingerprint(&root)?;
            if !builds.iter().any(|b| b.product_id == product && b.platform == "darwin" && b.tree_sha256 == hash) { return Err(ERROR.into()); }
            Ok(())
        }).await.map_err(|_| ERROR)?
    }
}

// The field order is part of the published native-smoke fingerprint format.
#[derive(Serialize)]
#[serde(untagged)]
enum Entry { File { path: String, size: u64, sha256: String }, Link { path: String, link: String } }

pub(crate) fn fingerprint(root: &Path) -> Result<String, String> {
    let base = root.canonicalize().map_err(|_| ERROR)?;
    if base != root || !fs::symlink_metadata(root).map_err(|_| ERROR)?.is_dir() { return Err(ERROR.into()); }
    let mut entries = Vec::new(); let mut total = 0u64;
    fn walk(base: &Path, file: &Path, name: String, entries: &mut Vec<Entry>, total: &mut u64) -> Result<(), String> {
        if entries.len() >= 100_000 { return Err(ERROR.into()); }
        let before = fs::symlink_metadata(file).map_err(|_| ERROR)?;
        if before.file_type().is_symlink() {
            if !file.canonicalize().map_err(|_| ERROR)?.starts_with(base) { return Err(ERROR.into()); }
            let link = fs::read_link(file).map_err(|_| ERROR)?.into_os_string().into_string().map_err(|_| ERROR)?;
            entries.push(Entry::Link { path: name, link });
        } else if before.is_dir() {
            let mut children = fs::read_dir(file).map_err(|_| ERROR)?.map(|v| v.map_err(|_| ERROR))
                .collect::<Result<Vec<_>, _>>()?;
            children.sort_by_key(|e| e.file_name());
            for child in children {
                let child_name = child.file_name().into_string().map_err(|_| ERROR)?;
                walk(base, &child.path(), if name.is_empty() { child_name } else { format!("{name}/{child_name}") }, entries, total)?;
            }
        } else {
            if !before.is_file() || before.nlink() != 1 { return Err(ERROR.into()); }
            *total = total.checked_add(before.len()).filter(|n| *n <= 16 * 1024 * 1024 * 1024).ok_or(ERROR)?;
            let mut hash = Sha256::new(); let mut stream = fs::File::open(file).map_err(|_| ERROR)?; let mut buffer = [0u8; 65536];
            loop { let n = stream.read(&mut buffer).map_err(|_| ERROR)?; if n == 0 { break; } hash.update(&buffer[..n]); }
            let after = fs::symlink_metadata(file).map_err(|_| ERROR)?;
            if before.dev() != after.dev() || before.ino() != after.ino() || before.len() != after.len()
                || before.mtime() != after.mtime() || before.mtime_nsec() != after.mtime_nsec()
                || before.ctime() != after.ctime() || before.ctime_nsec() != after.ctime_nsec() { return Err(ERROR.into()); }
            entries.push(Entry::File { path: name, size: after.len(), sha256: format!("{:x}", hash.finalize()) });
        }
        Ok(())
    }
    walk(&base, root, String::new(), &mut entries, &mut total)?;
    if entries.is_empty() { return Err(ERROR.into()); }
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&entries).map_err(|_| ERROR)?)))
}

/// Home comes from the effective OS user, independent of environment overrides.
pub(crate) fn native_home() -> Result<PathBuf, String> {
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() }; let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 65536];
    if unsafe { libc::getpwuid_r(libc::geteuid(), &mut pwd, buffer.as_mut_ptr().cast(), buffer.len(), &mut result) } != 0
        || result.is_null() || pwd.pw_dir.is_null() { return Err(ERROR.into()); }
    use std::os::unix::ffi::OsStrExt;
    let bytes = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }.to_bytes();
    let home = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    if !home.is_absolute() || home.canonicalize().ok().as_ref() != Some(&home) { return Err(ERROR.into()); }
    Ok(home)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree_matches_native_smoke_format_and_detects_resource_change() {
        let root = tempfile::tempdir().unwrap(); let root = root.path().canonicalize().unwrap();
        fs::write(root.join("a"), b"x").unwrap();
        let expected = format!("{:x}", Sha256::digest(format!("[{{\"path\":\"a\",\"size\":1,\"sha256\":\"{:x}\"}}]", Sha256::digest(b"x"))));
        assert_eq!(fingerprint(&root).unwrap(), expected);
        fs::write(root.join("a"), b"y").unwrap(); assert_ne!(fingerprint(&root).unwrap(), expected);
        std::os::unix::fs::symlink("/etc/passwd", root.join("escape")).unwrap(); assert!(fingerprint(&root).is_err());
    }
    #[test]
    fn os_home_is_absolute() { assert!(native_home().unwrap().is_absolute()); }
}
