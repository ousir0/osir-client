//! Signed desktop releases. No renderer URLs, environment overrides or redirects.
use base64::{engine::general_purpose::STANDARD, Engine};
use rsa::{
    pkcs1v15, pkcs8::DecodePublicKey, signature::Verifier, traits::PublicKeyParts, RsaPublicKey,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub(super) const FEED: &str =
    "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/stable.json";
const PREFIX: &str = "https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/";
pub(super) const PUBLIC_KEY: &str =
    include_str!("../../resources/products/osir-anywhere/release-public.pub");
pub(super) const MAX_MANIFEST: usize = 256 * 1024;
pub(super) const MAX_PACKAGE: u64 = 8 * 1024 * 1024 * 1024;
const HEALTH: &str = "https://anywhere.osirclaw.com/api/v2/health";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Release {
    pub schema_version: u32,
    pub product_id: String,
    pub channel: String,
    pub release_id: String,
    pub version: String,
    pub published_at: String,
    pub minimum_client_version: Option<String>,
    pub minimum_server_version: Option<String>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    pub platform: String,
    pub arch: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub filename: String,
    #[serde(rename = "systemSignature", default)]
    pub system_signature: Option<SystemSignature>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum SystemSignature {
    #[serde(rename = "unsigned")]
    Unsigned,
    #[serde(rename = "developer-id-notarized")]
    DeveloperIdNotarized,
    #[serde(rename = "authenticode")]
    Authenticode,
}

impl SystemSignature {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsigned => "unsigned",
            Self::DeveloperIdNotarized => "developer-id-notarized",
            Self::Authenticode => "authenticode",
        }
    }
}

pub(super) fn version(value: &str) -> Result<[u64; 3], String> {
    // Stable channel deliberately fails closed for unknown/prerelease versions.
    let mut result = [0; 3];
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 {
        return Err("无法安全比较发行版本，请更新 Client 后重试".into());
    }
    for (slot, part) in result.iter_mut().zip(parts) {
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|c| c.is_ascii_digit())
        {
            return Err("发行版本格式无效".into());
        }
        *slot = part.parse().map_err(|_| "发行版本超出范围")?;
    }
    Ok(result)
}

fn token(value: &str, limit: usize, spaces: bool) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && !value.contains("..")
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c) || (spaces && c == b' '))
        && !value.ends_with('.')
        && !value.ends_with(' ')
}

pub(super) fn verify(bytes: &[u8], pem: &str) -> Result<Release, String> {
    if bytes.len() > MAX_MANIFEST {
        return Err("发行清单过大".into());
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| "发行清单格式无效")?;
    let payload = STANDARD
        .decode(envelope.payload)
        .map_err(|_| "发行清单编码无效")?;
    let signature = STANDARD
        .decode(envelope.signature)
        .map_err(|_| "发行签名编码无效")?;
    let key = RsaPublicKey::from_public_key_pem(pem).map_err(|_| "发行验证公钥不可用")?;
    if !(2048..=8192).contains(&key.n().bits()) {
        return Err("发行验证公钥不可用".into());
    }
    let signature =
        pkcs1v15::Signature::try_from(signature.as_slice()).map_err(|_| "发行签名无效")?;
    pkcs1v15::VerifyingKey::<Sha256>::new(key)
        .verify(&payload, &signature)
        .map_err(|_| "发行签名验证失败，已阻止下载")?;
    // Parse only the authenticated bytes, never a reserialized approximation.
    let release: Release = serde_json::from_slice(&payload).map_err(|_| "发行内容格式无效")?;
    release.validate()?;
    Ok(release)
}

impl Release {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.product_id != "osir-anywhere"
            || self.channel != "stable"
            || !token(&self.release_id, 128, false)
            || self.artifacts.is_empty()
            || self.artifacts.len() > 8
        {
            return Err("发行清单产品、渠道或格式不受支持".into());
        }
        version(&self.version)?;
        let published =
            chrono::DateTime::parse_from_rfc3339(&self.published_at).map_err(|_| "发行日期无效")?;
        if published.timestamp() > chrono::Utc::now().timestamp() + 300 {
            return Err("发行日期尚未生效，请检查系统时间".into());
        }
        if let Some(minimum) = &self.minimum_client_version {
            version(minimum)?;
        }
        if let Some(minimum) = &self.minimum_server_version {
            version(minimum)?;
        }
        let mut seen = std::collections::HashSet::new();
        for artifact in &self.artifacts {
            artifact.validate(&self.release_id)?;
            if !seen.insert((&artifact.platform, &artifact.arch)) {
                return Err("发行清单包含重复平台工件".into());
            }
        }
        Ok(())
    }

    pub fn select(&self, platform: &str, arch: &str, client: &str) -> Result<Artifact, String> {
        if !matches!(platform, "darwin" | "win32") || !matches!(arch, "arm64" | "x64") {
            return Err("此系统或 CPU 架构尚不支持".into());
        }
        if self
            .minimum_client_version
            .as_ref()
            .map(|s| version(s))
            .transpose()?
            .is_some_and(|minimum| version(client).map_or(true, |current| current < minimum))
        {
            return Err("请先更新 Client，再安装此版本 Anywhere".into());
        }
        self.artifacts
            .iter()
            .find(|a| a.platform == platform && a.arch == arch)
            .or_else(|| {
                self.artifacts.iter().find(|a| {
                    platform == "darwin" && a.platform == platform && a.arch == "universal"
                })
            })
            .cloned()
            .ok_or_else(|| "此系统或 CPU 架构尚无正式安装包".into())
    }

    pub async fn check_server(&self) -> Result<(), String> {
        let Some(minimum) = &self.minimum_server_version else {
            return Ok(());
        };
        let run = async {
            let response = http_client()?
                .get(HEALTH)
                .header("Accept", "application/json")
                .header("Accept-Encoding", "identity")
                .header("Cache-Control", "no-cache")
                .send()
                .await
                .map_err(|_| "无法确认服务端兼容版本，请稍后重试")?;
            if response.status() != reqwest::StatusCode::OK
                || response.url().as_str() != HEALTH
                || response
                    .headers()
                    .contains_key(reqwest::header::CONTENT_ENCODING)
                || response
                    .content_length()
                    .is_some_and(|size| size > 16 * 1024)
            {
                return Err("服务端版本检查不可用，已阻止不确定的安装".into());
            }
            use futures::StreamExt;
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| "服务端版本检查中断")?;
                if bytes.len() + chunk.len() > 16 * 1024 {
                    return Err("服务端版本响应过大".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            check_health(&bytes, minimum)
        };
        tokio::time::timeout(std::time::Duration::from_secs(15), run)
            .await
            .map_err(|_| "服务端版本检查超时，请稍后重试")?
    }
}

pub(super) fn check_health(bytes: &[u8], minimum: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Health {
        status: String,
        version: String,
    }
    if bytes.len() > 16 * 1024 {
        return Err("服务端版本响应过大".into());
    }
    // Only this fixed HTTPS server's own version field is accepted. Additional
    // health fields (e.g. serverTime) are permitted; no desktop/fallback aliases.
    let health: Health = serde_json::from_slice(bytes).map_err(|_| "服务端未提供有效版本信息")?;
    if health.status != "ok" {
        return Err("服务端暂不可用，请稍后重试安装".into());
    }
    if version(&health.version)? < version(minimum)? {
        return Err("服务端版本暂不兼容此发行包，请等待服务更新".into());
    }
    Ok(())
}

impl Artifact {
    pub fn signature_policy(&self) -> Result<SystemSignature, String> {
        match (self.platform.as_str(), self.system_signature) {
            ("darwin", None | Some(SystemSignature::DeveloperIdNotarized)) => {
                Ok(SystemSignature::DeveloperIdNotarized)
            }
            ("win32", None | Some(SystemSignature::Authenticode)) => {
                Ok(SystemSignature::Authenticode)
            }
            ("darwin" | "win32", Some(SystemSignature::Unsigned)) => Ok(SystemSignature::Unsigned),
            _ => Err("安装包系统签名声明与平台不匹配".into()),
        }
    }

    pub fn validate(&self, release_id: &str) -> Result<(), String> {
        self.signature_policy()?;
        if !matches!(
            (self.platform.as_str(), self.arch.as_str()),
            ("darwin", "universal" | "arm64" | "x64") | ("win32", "arm64" | "x64")
        ) || !token(&self.filename, 200, true)
            || self.size < 512
            || self.size > MAX_PACKAGE
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|c| c.is_ascii_hexdigit())
            || !self.filename.ends_with(if self.platform == "darwin" {
                ".dmg"
            } else {
                ".exe"
            })
        {
            return Err("发行安装包信息无效".into());
        }
        // Exact canonical object: excludes credentials, ports, queries, fragments,
        // escapes, dot segments, sibling products and same-host arbitrary paths.
        let expected = format!("{PREFIX}{release_id}/{}", self.filename.replace(' ', "%20"));
        let url = url::Url::parse(&self.url).map_err(|_| "发行地址无效")?;
        if self.url != expected || url.as_str() != expected {
            return Err("安装包地址不属于受信任的发行对象".into());
        }
        Ok(())
    }
}

pub(super) fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(20))
        .read_timeout(std::time::Duration::from_secs(60))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .map_err(|_| "安全下载服务不可用".into())
}

pub(super) async fn fetch() -> Result<Option<Release>, String> {
    use futures::StreamExt;
    let run = async {
        let response = http_client()?
            .get(FEED)
            .header("Accept-Encoding", "identity")
            .send()
            .await
            .map_err(|_| "无法获取正式发行信息，请检查网络后重试")?;
        if matches!(response.status().as_u16(), 404 | 410) {
            return Ok(None);
        }
        if response.status() != reqwest::StatusCode::OK
            || response.url().as_str() != FEED
            || response
                .headers()
                .contains_key(reqwest::header::CONTENT_ENCODING)
            || response
                .content_length()
                .is_some_and(|size| size > MAX_MANIFEST as u64)
        {
            return Err("正式发行信息暂不可用，未跟随跳转".into());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "发行信息下载中断")?;
            if bytes.len() + chunk.len() > MAX_MANIFEST {
                return Err("发行清单过大".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        verify(&bytes, PUBLIC_KEY).map(Some)
    };
    tokio::time::timeout(std::time::Duration::from_secs(30), run)
        .await
        .map_err(|_| "发行信息检查超时，请重试")?
}
