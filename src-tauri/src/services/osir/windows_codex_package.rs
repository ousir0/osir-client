use serde::Deserialize;
use std::io::Read;
use std::path::Path;

pub const PACKAGE_IDENTITY: &str = "OpenAI.Codex";
// Store package identity, also used by installed-package discovery. This is
// the identity that hashes to PublisherId 2p2nqsd0c76g0, not the company name.
pub const PACKAGE_PUBLISHER: &str = "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B";
pub const PACKAGE_FAMILY: &str = "OpenAI.Codex_2p2nqsd0c76g0";
const MARKETPLACE_SIGNER_SUBJECT: &str = PACKAGE_PUBLISHER;
pub const MIRROR_REPOSITORY: &str = "Wangnov/codex-app-mirror";
const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Mirror,
    Official,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mirror => "osir_mirror",
            Self::Official => "official",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRelease {
    pub tag: String,
    pub version: String,
    pub package_version: String,
    pub build: u64,
    pub size: u64,
    pub url: String,
    pub sha256: Option<String>,
    pub checksum_url: Option<String>,
    pub checksum_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

pub fn parse_mirror_releases(text: &str, architecture: &str) -> Result<PackageRelease, String> {
    let releases: Vec<GitHubRelease> =
        serde_json::from_str(text).map_err(|_| "无法读取 Windows 镜像版本信息".to_string())?;
    let arch = normalize_architecture(architecture)?;
    let mut candidates = releases
        .into_iter()
        .filter_map(|release| parse_mirror_release(release, arch).ok())
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| compare_versions(&a.version, &b.version));
    candidates
        .pop()
        .ok_or_else(|| "Windows 镜像暂未提供经过验证的 Codex 安装包".to_string())
}

fn parse_mirror_release(
    release: GitHubRelease,
    architecture: &str,
) -> Result<PackageRelease, String> {
    if release.draft || release.prerelease {
        return Err("release is not stable".into());
    }
    let tag = release.tag_name;
    let version = tag
        .strip_prefix("codex-app-")
        .ok_or("unsupported release tag")?
        .to_string();
    parse_version_parts(&version, 3)?;
    let package = release
        .assets
        .iter()
        .find(|asset| {
            asset.name.starts_with("OpenAI.Codex_")
                && asset
                    .name
                    .ends_with(&format!("_{architecture}__2p2nqsd0c76g0.Msix"))
        })
        .ok_or("matching MSIX is missing")?;
    let package_version = package
        .name
        .strip_prefix("OpenAI.Codex_")
        .and_then(|name| name.strip_suffix(&format!("_{architecture}__2p2nqsd0c76g0.Msix")))
        .ok_or("invalid MSIX name")?;
    parse_version_parts(package_version, 4)?;
    let digest = parse_github_digest(package.digest.as_deref())?;
    validate_package_size(package.size)?;
    let url = package.browser_download_url.clone();
    validate_mirror_url(&url, &tag, &package.name)?;

    let checksum_name = "SHA256SUMS-windows.txt";
    let checksum = release
        .assets
        .iter()
        .find(|asset| asset.name == checksum_name)
        .ok_or("Windows checksum file is missing")?;
    validate_package_size(checksum.size)?;
    let checksum_sha256 = parse_github_digest(checksum.digest.as_deref())?;
    let checksum_url = checksum.browser_download_url.clone();
    validate_mirror_url(&checksum_url, &tag, checksum_name)?;

    Ok(PackageRelease {
        tag,
        version,
        package_version: package_version.to_string(),
        build: package_version_number(package_version)?,
        size: package.size,
        url,
        sha256: Some(digest),
        checksum_url: Some(checksum_url),
        checksum_sha256: Some(checksum_sha256),
    })
}

pub fn official_release(
    version: &str,
    size: u64,
    url: &str,
    architecture: &str,
) -> Result<PackageRelease, String> {
    let arch = normalize_architecture(architecture)?;
    parse_version_parts(version, 4)?;
    validate_package_size(size)?;
    let expected_url = match arch {
        "x64" => "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-x64.msix",
        "arm64" => "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-arm64.msix",
        _ => unreachable!(),
    };
    if url != expected_url {
        return Err("official package URL did not match the pinned endpoint".into());
    }
    Ok(PackageRelease {
        tag: String::new(),
        version: version.to_string(),
        package_version: version.to_string(),
        build: package_version_number(version)?,
        size,
        url: url.to_string(),
        sha256: None,
        checksum_url: None,
        checksum_sha256: None,
    })
}

pub fn verify_mirror_checksum(text: &str, file_name: &str, expected: &str) -> Result<(), String> {
    let actual = text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == file_name).then(|| digest.to_ascii_lowercase())
    });
    if actual.as_deref() == Some(expected) {
        Ok(())
    } else {
        Err("镜像校验清单与 GitHub 文件摘要不一致".into())
    }
}

pub fn validate_mirror_url(url: &str, tag: &str, asset: &str) -> Result<(), String> {
    let expected =
        format!("https://github.com/{MIRROR_REPOSITORY}/releases/download/{tag}/{asset}");
    if url == expected {
        Ok(())
    } else {
        Err("Windows 镜像下载地址无效".into())
    }
}

pub fn parse_msix_identity(
    xml: &str,
    expected_version: &str,
    expected_architecture: &str,
) -> Result<(), String> {
    let document = roxmltree::Document::parse(xml).map_err(|_| "安装包清单无法读取")?;
    let identity = document
        .descendants()
        .find(|node| node.has_tag_name("Identity"))
        .ok_or("安装包缺少应用身份信息")?;
    let name = identity.attribute("Name").ok_or("安装包身份不完整")?;
    let publisher = identity.attribute("Publisher").ok_or("安装包身份不完整")?;
    let version = identity.attribute("Version").ok_or("安装包身份不完整")?;
    let architecture = identity
        .attribute("ProcessorArchitecture")
        .ok_or("安装包身份不完整")?;
    if name != PACKAGE_IDENTITY
        || !publisher.eq_ignore_ascii_case(PACKAGE_PUBLISHER)
        || version != expected_version
        || !architecture.eq_ignore_ascii_case(expected_architecture)
    {
        return Err("安装包身份、发布者、版本或系统架构不匹配".into());
    }
    Ok(())
}

pub fn read_msix_identity(
    path: &Path,
    expected_version: &str,
    expected_architecture: &str,
) -> Result<(), String> {
    let file = std::fs::File::open(path).map_err(|_| "无法读取 Windows 安装包")?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| "Windows 安装包格式无效")?;
    let manifest = archive
        .by_name("AppxManifest.xml")
        .map_err(|_| "Windows 安装包缺少应用清单")?;
    if manifest.size() > MAX_MANIFEST_BYTES as u64 {
        return Err("Windows 安装包清单过大".into());
    }
    let mut xml = String::new();
    manifest
        .take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_string(&mut xml)
        .map_err(|_| "Windows 安装包清单无法读取")?;
    if xml.len() > MAX_MANIFEST_BYTES {
        return Err("Windows 安装包清单过大".into());
    }
    parse_msix_identity(&xml, expected_version, expected_architecture)
}

pub fn is_valid_marketplace_signature(status: &str, subject: &str, issuer: &str) -> bool {
    let components = |value: &str| {
        value
            .split(',')
            .map(|part| part.trim().to_ascii_lowercase())
            .collect::<Vec<_>>()
    };
    let subject = components(subject);
    let issuer = components(issuer);
    status.eq_ignore_ascii_case("Valid")
        && subject.len() == 1
        && subject[0] == MARKETPLACE_SIGNER_SUBJECT.to_ascii_lowercase()
        && issuer
            .iter()
            .any(|part| part.starts_with("cn=microsoft marketplace ca"))
        && issuer.iter().any(|part| part == "o=microsoft corporation")
}

pub fn package_version_number(version: &str) -> Result<u64, String> {
    let parts = parse_version_parts(version, 4)?;
    let mut result = 0u64;
    for part in parts {
        result = result
            .checked_mul(65_536)
            .and_then(|value| value.checked_add(u64::from(part)))
            .ok_or("Windows 应用版本超出安全比较范围")?;
    }
    if result > 9_007_199_254_740_991 {
        return Err("Windows 应用版本超出安全比较范围".into());
    }
    Ok(result)
}

fn parse_github_digest(digest: Option<&str>) -> Result<String, String> {
    let digest = digest
        .and_then(|value| value.strip_prefix("sha256:"))
        .ok_or("GitHub 未提供 SHA-256 文件摘要")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("GitHub 文件摘要格式无效".into());
    }
    Ok(digest.to_ascii_lowercase())
}

fn validate_package_size(size: u64) -> Result<(), String> {
    if size == 0 || size > MAX_PACKAGE_BYTES {
        Err("Windows 安装包大小无效".into())
    } else {
        Ok(())
    }
}

fn normalize_architecture(value: &str) -> Result<&'static str, String> {
    match value.to_ascii_lowercase().as_str() {
        "x64" | "x86_64" | "amd64" => Ok("x64"),
        "arm64" | "aarch64" => Ok("arm64"),
        _ => Err("此 Windows 架构尚无已核实的 Codex 安装包".into()),
    }
}

fn parse_version_parts(value: &str, count: usize) -> Result<Vec<u16>, String> {
    let parts = value
        .split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("Windows 应用版本格式无效".to_string());
            }
            part.parse::<u16>()
                .map_err(|_| "Windows 应用版本格式无效".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parts.len() != count {
        return Err("Windows 应用版本格式无效".into());
    }
    Ok(parts)
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let left = parse_version_parts(a, 3).unwrap_or_default();
    let right = parse_version_parts(b, 3).unwrap_or_default();
    left.cmp(&right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mirror_release(version: &str, package_version: &str, arch: &str) -> serde_json::Value {
        let tag = format!("codex-app-{version}");
        let name = format!("OpenAI.Codex_{package_version}_{arch}__2p2nqsd0c76g0.Msix");
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "assets": [
                {"name": name, "size": 1_000_000, "digest": format!("sha256:{}", "a".repeat(64)), "browser_download_url": format!("https://github.com/{MIRROR_REPOSITORY}/releases/download/{tag}/{name}")},
                {"name": "SHA256SUMS-windows.txt", "size": 128, "digest": format!("sha256:{}", "b".repeat(64)), "browser_download_url": format!("https://github.com/{MIRROR_REPOSITORY}/releases/download/{tag}/SHA256SUMS-windows.txt")}
            ]
        })
    }

    #[test]
    fn selects_latest_stable_architecture_matched_mirror_and_checksums() {
        let json = serde_json::to_string(&vec![
            mirror_release("26.1026.30000", "26.1026.3200.0", "x64"),
            mirror_release("26.1027.30824", "26.1027.3384.0", "x64"),
            mirror_release("26.1028.1", "26.1028.1.0", "arm64"),
        ])
        .unwrap();
        let result = parse_mirror_releases(&json, "x86_64").unwrap();
        assert_eq!(result.version, "26.1027.30824");
        assert_eq!(result.package_version, "26.1027.3384.0");
        assert_eq!(result.sha256.as_deref(), Some("a".repeat(64).as_str()));
        assert_eq!(
            result.checksum_sha256.as_deref(),
            Some("b".repeat(64).as_str())
        );
    }

    #[test]
    fn rejects_untrusted_mirror_urls_missing_digests_and_wrong_architecture() {
        let mut release = mirror_release("26.1027.30824", "26.1027.3384.0", "x64");
        release["assets"][0]["browser_download_url"] =
            json!("https://attacker.invalid/package.Msix");
        assert!(parse_mirror_releases(&json!([release]).to_string(), "x64").is_err());

        let mut release = mirror_release("26.1027.30824", "26.1027.3384.0", "x64");
        release["assets"][0]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        assert!(parse_mirror_releases(&json!([release]).to_string(), "x64").is_err());

        assert!(parse_mirror_releases("[]", "arm64").is_err());
    }

    #[test]
    fn accepts_store_manifest_identity_from_a_literal_package_fixture() {
        let xml = r#"<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"><Identity Name="OpenAI.Codex" Publisher="CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B" Version="26.1027.3384.0" ProcessorArchitecture="x64" /></Package>"#;
        parse_msix_identity(xml, "26.1027.3384.0", "x64").unwrap();
    }

    #[test]
    fn validates_pinned_package_identity_version_architecture_and_signature() {
        let xml = format!(
            r#"<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"><Identity Name="{PACKAGE_IDENTITY}" Publisher="{PACKAGE_PUBLISHER}" Version="26.1027.3384.0" ProcessorArchitecture="x64" /></Package>"#
        );
        assert!(parse_msix_identity(&xml, "26.1027.3384.0", "x64").is_ok());
        assert!(parse_msix_identity(&xml, "26.1027.3384.0", "arm64").is_err());
        let wrong_manifest_publisher = xml.replace(
            PACKAGE_PUBLISHER,
            "CN=OpenAI OpCo, LLC, O=OpenAI OpCo, LLC, C=US",
        );
        assert!(parse_msix_identity(&wrong_manifest_publisher, "26.1027.3384.0", "x64").is_err());
        assert!(is_valid_marketplace_signature(
            "Valid",
            MARKETPLACE_SIGNER_SUBJECT,
            "CN=Microsoft Marketplace CA G 028, O=Microsoft Corporation, C=US"
        ));
        assert!(!is_valid_marketplace_signature(
            "Valid",
            "CN=OpenAI OpCo, LLC, O=OpenAI OpCo, LLC, C=US",
            "CN=Trusted CA, O=Microsoft Corporation"
        ));
    }

    #[test]
    fn compares_four_part_package_versions_without_overflow() {
        assert!(
            package_version_number("26.1027.3384.0").unwrap()
                > package_version_number("26.1027.3383.65535").unwrap()
        );
        assert!(package_version_number("65535.65535.65535.65535").is_err());
        assert!(verify_mirror_checksum(
            &format!("{}  OpenAI.Codex.Msix\n", "a".repeat(64)),
            "OpenAI.Codex.Msix",
            &"a".repeat(64)
        )
        .is_ok());
    }
}
