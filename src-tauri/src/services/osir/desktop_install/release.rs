//! Offline release boundary: parses supplied XML, never fetches URLs or installs.
//! Sources copied from Manager app/mac_update.rs:110-118. Highest build means
//! highest in the supplied snapshot, NOT a claim about today's latest release.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use codex_mac_engine::{limits, parse_appcast, verify_sparkle};
use serde::Serialize;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Auto,
    Official,
    OsirMirror,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    Arm64,
    X86_64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseError {
    UrlPolicy,
    Xml,
    Size,
    Signature,
    AmbiguousRelease,
    NotAvailable,
}
pub type Result<T> = std::result::Result<T, ReleaseError>;

pub fn appcast_url(source: Source, arch: Architecture) -> &'static str {
    match (source, arch) {
        (Source::Official, Architecture::Arm64) => {
            "https://persistent.oaistatic.com/codex-app-prod/appcast.xml"
        }
        (Source::Official, Architecture::X86_64) => {
            "https://persistent.oaistatic.com/codex-app-prod/appcast-x64.xml"
        }
        (Source::Auto | Source::OsirMirror, Architecture::Arm64) => {
            "https://app.osirclaw.com/latest/appcast.xml"
        }
        (Source::Auto | Source::OsirMirror, Architecture::X86_64) => {
            "https://app.osirclaw.com/latest/appcast-x64.xml"
        }
    }
}

/// All explicit ports (including :443), userinfo, query and fragment are denied.
/// Reject ambiguous URL syntax before URL normalization can erase it.
fn trusted_url(raw: &str) -> Result<Url> {
    if !raw.starts_with("https://")
        || raw
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        || raw.as_bytes().contains(&92)
    {
        return Err(ReleaseError::UrlPolicy);
    }
    let authority = raw
        .strip_prefix("https://")
        .unwrap()
        .split('/')
        .next()
        .unwrap_or("");
    if !matches!(authority, "persistent.oaistatic.com" | "app.osirclaw.com") {
        return Err(ReleaseError::UrlPolicy);
    }
    let u = Url::parse(raw).map_err(|_| ReleaseError::UrlPolicy)?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(ReleaseError::UrlPolicy);
    }
    if u.as_str() != raw
        || u.path().contains('%')
        || u.path().split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err(ReleaseError::UrlPolicy);
    }
    Ok(u)
}

/// Future HTTP adapter must disable automatic redirects and call this BEFORE
/// following each Location. Feed redirects are intentionally unsupported.
pub fn validate_feed_response(
    source: Source,
    arch: Architecture,
    response_url: &str,
) -> Result<()> {
    trusted_url(response_url)?;
    if response_url != appcast_url(source, arch) {
        return Err(ReleaseError::UrlPolicy);
    }
    Ok(())
}
fn artifact_url(raw: &str) -> Result<()> {
    let u = trusted_url(raw)?;
    if !u.path().ends_with(".zip") {
        return Err(ReleaseError::UrlPolicy);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseMetadata {
    pub architecture: Architecture,
    pub build: u64,
    pub version: String,
    pub minimum_system_version: Option<String>,
    pub published_at: Option<String>,
    pub full_size: u64,
    pub update_available: bool,
}
/// Native-only capability; cannot be deserialized or constructed from IPC.
/// This is only a full ZIP plan. No BinaryDelta executable is bundled, so delta
/// entries are deliberately never selected, even when the engine prefers one.
pub struct ReleasePlan {
    source: Source,
    metadata: ReleaseMetadata,
    url: String,
    signature: String,
}
impl ReleasePlan {
    pub fn source(&self) -> Source {
        self.source
    }
    pub fn metadata(&self) -> &ReleaseMetadata {
        &self.metadata
    }
    pub fn download_url(&self) -> &str {
        &self.url
    }
    pub fn validate_redirect(&self, from: &str, to: &str, hop: usize) -> Result<()> {
        if hop >= 5 {
            return Err(ReleaseError::UrlPolicy);
        }
        artifact_url(from)?;
        artifact_url(to)?;
        // Restrict every hop to the original approved origin. No arbitrary CDN
        // or mirror-to-origin redirect expansion based on network responses.
        let origin = trusted_url(&self.url)?.origin();
        if trusted_url(from)?.origin() != origin || trusted_url(to)?.origin() != origin {
            return Err(ReleaseError::UrlPolicy);
        }
        Ok(())
    }
    /// Bytes must already be bounded by the downloader while streaming. This
    /// authenticates bytes only: no extraction, bundle signature or build proof.
    pub fn verify_artifact<'a>(&self, bytes: &'a [u8]) -> Result<VerifiedArtifact<'a>> {
        if !self.metadata.update_available {
            return Err(ReleaseError::NotAvailable);
        }
        if bytes.len() as u64 != self.metadata.full_size
            || bytes.len() as u64 > limits::MAX_PACKAGE_BYTES
        {
            return Err(ReleaseError::Size);
        }
        verify_sparkle(bytes, &self.signature).map_err(|_| ReleaseError::Signature)?;
        Ok(VerifiedArtifact {
            bytes,
            build: self.metadata.build,
        })
    }
}
pub struct VerifiedArtifact<'a> {
    bytes: &'a [u8],
    build: u64,
}
impl<'a> VerifiedArtifact<'a> {
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn build(&self) -> u64 {
        self.build
    }
}
pub fn parse_release(
    source: Source,
    arch: Architecture,
    response_url: &str,
    xml: &str,
    current_build: u64,
) -> Result<ReleasePlan> {
    validate_feed_response(source, arch, response_url)?;
    if xml.is_empty() || xml.len() as u64 > limits::MAX_TEXT_BYTES {
        return Err(ReleaseError::Size);
    }
    let feed = parse_appcast(xml).map_err(|_| ReleaseError::Xml)?;
    let item = feed.latest().ok_or(ReleaseError::Xml)?;
    if item.build == 0 || feed.items.iter().filter(|i| i.build == item.build).count() != 1 {
        return Err(ReleaseError::AmbiguousRelease);
    }
    artifact_url(&item.full.url)?;
    if item.full.length == 0 || item.full.length > limits::MAX_PACKAGE_BYTES {
        return Err(ReleaseError::Size);
    }
    let signature = item
        .full
        .ed_signature
        .as_ref()
        .ok_or(ReleaseError::Signature)?;
    if signature.len() != 88
        || STANDARD
            .decode(signature)
            .map_err(|_| ReleaseError::Signature)?
            .len()
            != 64
    {
        return Err(ReleaseError::Signature);
    }
    Ok(ReleasePlan {
        source,
        metadata: ReleaseMetadata {
            architecture: arch,
            build: item.build,
            version: item.short_version.clone(),
            minimum_system_version: item.minimum_system_version.clone(),
            published_at: item.pub_date.clone(),
            full_size: item.full.length,
            update_available: item.build > current_build,
        },
        url: item.full.url.clone(),
        signature: signature.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const XML: &str = include_str!("fixtures/release-appcast.xml");
    fn parse(xml: &str, current: u64) -> Result<ReleasePlan> {
        parse_release(
            Source::Official,
            Architecture::Arm64,
            appcast_url(Source::Official, Architecture::Arm64),
            xml,
            current,
        )
    }
    #[test]
    fn snapshot_selects_full_without_delta_tool() {
        let p = parse(XML, 3511).unwrap();
        assert_eq!(p.metadata().build, 3575);
        assert!(p.download_url().ends_with(".zip"));
        assert_eq!(p.metadata().full_size, 406581087);
    }
    #[test]
    fn up_to_date_never_authorizes_install() {
        let p = parse(XML, 9999).unwrap();
        assert!(!p.metadata().update_available);
        assert!(matches!(
            p.verify_artifact(b""),
            Err(ReleaseError::NotAvailable)
        ));
    }
    #[test]
    fn rejects_url_credentials_ports_queries_and_untrusted_redirects() {
        let p = parse(XML, 0).unwrap();
        for bad in [
            "http://app.osirclaw.com/a.zip",
            "https://user@app.osirclaw.com/a.zip",
            "https://app.osirclaw.com:443/a.zip",
            "https://app.osirclaw.com/a.zip?token=secret",
            "https://app.osirclaw.com/a.zip#x",
            "https://evil.test/a.zip",
            "https://persistent.oaistatic.com/%2e%2e/a.zip",
        ] {
            assert!(p.validate_redirect(p.download_url(), bad, 0).is_err());
        }
        assert!(p
            .validate_redirect(p.download_url(), "https://app.osirclaw.com/a.zip", 0)
            .is_err());
        assert!(p
            .validate_redirect(p.download_url(), p.download_url(), 5)
            .is_err());
        assert!(p
            .validate_redirect(p.download_url(), p.download_url(), 0)
            .is_ok());
    }
    #[test]
    fn feed_source_and_arch_are_bound() {
        assert!(parse_release(
            Source::Official,
            Architecture::X86_64,
            appcast_url(Source::Official, Architecture::Arm64),
            XML,
            0
        )
        .is_err());
    }
    #[test]
    fn fixed_sources_match_manager_and_cannot_be_crossed() {
        assert_eq!(
            appcast_url(Source::OsirMirror, Architecture::Arm64),
            "https://app.osirclaw.com/latest/appcast.xml"
        );
        assert_eq!(
            appcast_url(Source::OsirMirror, Architecture::X86_64),
            "https://app.osirclaw.com/latest/appcast-x64.xml"
        );
        assert_eq!(
            serde_json::to_value(Source::OsirMirror).unwrap(),
            "osir_mirror"
        );
        assert_eq!(serde_json::to_value(Source::Official).unwrap(), "official");
        for arch in [Architecture::Arm64, Architecture::X86_64] {
            assert!(validate_feed_response(
                Source::OsirMirror,
                arch,
                appcast_url(Source::Official, arch)
            )
            .is_err());
            assert!(validate_feed_response(
                Source::Official,
                arch,
                appcast_url(Source::OsirMirror, arch)
            )
            .is_err());
        }
    }
    #[test]
    fn rejects_size_and_missing_signature() {
        assert!(matches!(
            parse(&XML.replace("406581087", "0"), 0),
            Err(ReleaseError::Size)
        ));
        assert!(matches!(
            parse(&XML.replace("406581087", "2147483649"), 0),
            Err(ReleaseError::Size)
        ));
        assert!(matches!(
            parse(&XML.replace("sparkle:edSignature=", "ignored="), 0),
            Err(ReleaseError::Signature)
        ));
    }
    #[test]
    fn signed_metadata_is_not_verified_artifact() {
        let p = parse(XML, 0).unwrap();
        assert!(matches!(p.verify_artifact(b"x"), Err(ReleaseError::Size)));
        // Real snapshot signature, synthetic matching-length data: must fail the
        // pinned key check. No test-only key injection in production API.
        let p = parse(&XML.replace("406581087", "1"), 0).unwrap();
        assert!(matches!(
            p.verify_artifact(b"x"),
            Err(ReleaseError::Signature)
        ));
    }
}
