//! Windows Codex installs use the signed Microsoft MSIX for the current user.
//! The GitHub mirror is only a transport; its package hash and Marketplace
//! signature are verified before Windows package deployment is invoked.
use super::super::osir_tools::{windows, windows_support as support};
use super::InstallSource;
use crate::services::osir::windows_codex_package as package;
use futures::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/Wangnov/codex-app-mirror/releases?per_page=100";
const MAX_API_BYTES: usize = 8 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Installer {
    checked: Mutex<Option<Plan>>,
    progress: Mutex<Option<Progress>>,
}

#[derive(Clone)]
struct Plan {
    release: package::PackageRelease,
    target: String,
    installed: Option<u64>,
    requested_source: String,
    actual_source: package::Source,
}

#[derive(Clone)]
struct Progress {
    operation_id: String,
    phase: String,
}

impl Installer {
    pub fn status(&self) -> Value {
        let progress = self.progress.lock().ok().and_then(|state| state.clone());
        json!({
            "operationId": progress.as_ref().map(|item| item.operation_id.clone()),
            "phase": progress.map(|item| item.phase),
        })
    }

    pub fn cancel(&self) -> Value {
        json!(false)
    }

    pub async fn check(
        &self,
        source: InstallSource,
        target: Option<String>,
    ) -> Result<Value, String> {
        let requested_source = source_name(source);
        let (target, installed) = selected(target.as_deref()).await?;
        let (release, actual_source) = resolve_release(source, std::env::consts::ARCH).await?;
        *self.checked.lock().map_err(|_| "安装状态不可用")? = Some(Plan {
            release: release.clone(),
            target: target.clone(),
            installed,
            requested_source: requested_source.to_string(),
            actual_source,
        });
        Ok(json!({
            "source":requested_source,
            "actualSource":actual_source.as_str(),
            "installedBuild":installed,
            "highestDetectedBuild":installed,
            "comparisonComplete":true,
            "release":{
                "build":release.build,
                "version":release.package_version,
                "fullSize":release.size,
                "updateAvailable":release.build>installed.unwrap_or(0)
            },
            "targetPath":target,
            "restorePoints":[],
            "platform":"windows",
            "installMethod":"windows_msix",
            "supportsAppBackup":false
        }))
    }

    pub async fn local(
        &self,
        app: &tauri::AppHandle,
        action: &str,
        target: Option<&str>,
    ) -> Result<Value, String> {
        let (target, installed) = selected(target).await?;
        match action {
            "inspect" => (),
            "launch" => windows::launch_desktop(app, "codex", false).await?,
            "reveal" => {
                let entries = support::discover("codex").await?;
                let entry = entries.first().ok_or("请先安装 Codex Desktop")?;
                tauri_plugin_opener::reveal_item_in_dir(&entry.executable)
                    .map_err(|_| "无法显示应用安装位置")?;
            }
            _ => return Err("安装器操作无效".into()),
        }
        Ok(json!({
            "targetPath":target,
            "installedBuild":installed,
            "restorePoints":[],
            "recoveryRequired":false,
            "updateBlockedReason":null,
            "platform":"windows",
            "installMethod":"windows_msix",
            "supportsAppBackup":false
        }))
    }

    pub async fn install(
        &self,
        _app: &tauri::AppHandle,
        source: InstallSource,
        expected_build: u64,
        target: Option<String>,
        expected_installed: Option<u64>,
        expected_source: Option<InstallSource>,
    ) -> Result<Value, String> {
        if crate::distribution::is_isolated() {
            return Err("隔离开发环境不能安装本机 Windows 应用，请在正式版中操作".into());
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        self.set_progress(&operation_id, "checking");
        let result = self
            .install_checked(
                source,
                expected_build,
                target,
                expected_installed,
                expected_source,
                &operation_id,
            )
            .await;
        self.clear_progress(&operation_id);
        result
    }

    async fn install_checked(
        &self,
        source: InstallSource,
        expected_build: u64,
        target: Option<String>,
        expected_installed: Option<u64>,
        expected_source: Option<InstallSource>,
        operation_id: &str,
    ) -> Result<Value, String> {
        let _guard = windows::guard()?;
        let requested_source = source_name(source);
        let plan = self
            .checked
            .lock()
            .map_err(|_| "安装状态不可用")?
            .clone()
            .ok_or("请先检查可安装版本")?;
        let (current_target, current_build) = selected(target.as_deref()).await?;
        if plan.requested_source != requested_source
            || plan.actual_source.as_str() != expected_source.map(source_name).unwrap_or_default()
            || plan.release.build != expected_build
            || plan.installed != current_build
            || expected_installed != current_build
            || !support::same_path(&plan.target, &current_target)
            || expected_build <= current_build.unwrap_or(0)
        {
            return Err("版本或安装状态已变化，请重新检查".into());
        }
        if windows::codex_running().await? {
            return Err("请先退出 Codex，再执行安装或更新".into());
        }

        let (package_file, downloaded_sha256) =
            download_package(&plan.release, operation_id, |phase| {
                self.set_progress(operation_id, phase)
            })
            .await?;
        let expected_architecture = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => return Err("此 Windows 架构尚未接入 Codex 安装".into()),
        };

        self.set_progress(operation_id, "verifying_artifact");
        if let Some(expected) = plan.release.sha256.as_deref() {
            if downloaded_sha256 != expected {
                return Err("Windows 安装包 SHA-256 校验失败，未安装".into());
            }
        }
        if let (Some(checksum_url), Some(checksum_digest), Some(package_digest)) = (
            plan.release.checksum_url.as_deref(),
            plan.release.checksum_sha256.as_deref(),
            plan.release.sha256.as_deref(),
        ) {
            let checksum_text = fetch_text(checksum_url, 256 * 1024).await?;
            if sha256(checksum_text.as_bytes()) != checksum_digest {
                return Err("Windows 镜像校验文件 SHA-256 不匹配，未安装".into());
            }
            package::verify_mirror_checksum(
                &checksum_text,
                &format_package_name(&plan.release, expected_architecture),
                package_digest,
            )?;
        }
        let package_path = package_file.path().to_path_buf();
        let expected_version = plan.release.package_version.clone();
        let arch = expected_architecture.to_string();
        tokio::task::spawn_blocking(move || {
            package::read_msix_identity(&package_path, &expected_version, &arch)
        })
        .await
        .map_err(|_| "Windows 安装包身份检查未完成")??;

        self.set_progress(operation_id, "verifying_application");
        verify_authenticode(package_file.path()).await?;

        self.set_progress(operation_id, "applying");
        install_msix(package_file.path(), &plan.release.package_version).await?;
        let entries = support::discover("codex").await?;
        if entries.len() != 1 || entries[0].version != plan.release.package_version {
            return Err("Windows 未确认已安装目标 Codex 版本，请重新检测安装状态".into());
        }
        *self.checked.lock().map_err(|_| "安装状态不可用")? = None;
        Ok(json!({
            "step":"completed",
            "build":plan.release.build,
            "installed":true,
            "message":"Windows 已确认 Codex 安装完成"
        }))
    }

    fn set_progress(&self, operation_id: &str, phase: &str) {
        if let Ok(mut progress) = self.progress.lock() {
            *progress = Some(Progress {
                operation_id: operation_id.to_string(),
                phase: phase.to_string(),
            });
        }
    }

    fn clear_progress(&self, operation_id: &str) {
        if let Ok(mut progress) = self.progress.lock() {
            if progress
                .as_ref()
                .is_some_and(|current| current.operation_id == operation_id)
            {
                *progress = None;
            }
        }
    }
}

fn source_name(source: InstallSource) -> &'static str {
    match source {
        InstallSource::Auto => "auto",
        InstallSource::Official => "official",
        InstallSource::OsirMirror => "osir_mirror",
    }
}

async fn resolve_release(
    source: InstallSource,
    architecture: &str,
) -> Result<(package::PackageRelease, package::Source), String> {
    match source {
        InstallSource::OsirMirror => {
            Ok((mirror_release(architecture).await?, package::Source::Mirror))
        }
        InstallSource::Official => Ok((
            official_release(architecture).await?,
            package::Source::Official,
        )),
        InstallSource::Auto => {
            let (mirror, official) =
                tokio::join!(mirror_release(architecture), official_release(architecture));
            match (mirror, official) {
                (Ok(mirror), Ok(official)) if mirror.build > official.build => {
                    Ok((mirror, package::Source::Mirror))
                }
                (Ok(_), Ok(official)) => Ok((official, package::Source::Official)),
                (Ok(mirror), Err(_)) => Ok((mirror, package::Source::Mirror)),
                (Err(_), Ok(official)) => Ok((official, package::Source::Official)),
                (Err(_), Err(_)) => Err("Codex Windows 版本信息暂不可用，请重试".into()),
            }
        }
    }
}

async fn mirror_release(architecture: &str) -> Result<package::PackageRelease, String> {
    let text = fetch_text(GITHUB_RELEASES_API, MAX_API_BYTES).await?;
    package::parse_mirror_releases(&text, architecture)
}

async fn official_release(architecture: &str) -> Result<package::PackageRelease, String> {
    let url = support::package_url(architecture)?;
    let response = http_client(Duration::from_secs(20))?
        .head(url)
        .send()
        .await
        .map_err(|_| "官方版本信息暂不可用，请重试".to_string())?;
    if !response.status().is_success() {
        return Err("官方版本信息暂不可用，请稍后重试".into());
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| "官方安装包信息不完整".to_string())
    };
    if header("x-ms-meta-package_identity")? != package::PACKAGE_IDENTITY {
        return Err("官方安装包身份不符".into());
    }
    let version = header("x-ms-meta-package_version")?;
    let size = header("content-length")?
        .parse::<u64>()
        .map_err(|_| "官方安装包大小无效".to_string())?;
    package::official_release(&version, size, url, architecture)
}

async fn download_package(
    release: &package::PackageRelease,
    operation_id: &str,
    mut set_phase: impl FnMut(&str),
) -> Result<(tempfile::NamedTempFile, String), String> {
    set_phase("downloading");
    let response = http_client(Duration::from_secs(30 * 60))?
        .get(&release.url)
        .send()
        .await
        .map_err(|_| "Windows 安装包下载失败，请重试".to_string())?;
    if !response.status().is_success() {
        return Err("Windows 安装包暂不可下载，请重试或切换下载源".into());
    }
    let content_length = response.content_length().unwrap_or(release.size);
    if content_length != release.size || content_length == 0 || content_length > MAX_PACKAGE_BYTES {
        return Err("Windows 安装包大小与版本信息不符，已取消安装".into());
    }
    let temporary = tempfile::Builder::new()
        .prefix("osir-codex-")
        .suffix(".msix")
        .tempfile()
        .map_err(|_| "无法准备 Windows 安装包缓存".to_string())?;
    let standard_file = temporary
        .reopen()
        .map_err(|_| "无法写入 Windows 安装包缓存".to_string())?;
    let mut file = tokio::fs::File::from_std(standard_file);
    let mut stream = response.bytes_stream();
    let mut downloaded = 0u64;
    let mut hasher = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Windows 安装包下载中断，请重试".to_string())?;
        downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= release.size && *size <= MAX_PACKAGE_BYTES)
            .ok_or("Windows 安装包大小超出版本信息")?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|_| "无法保存 Windows 安装包".to_string())?;
    }
    file.flush()
        .await
        .map_err(|_| "无法完成 Windows 安装包缓存".to_string())?;
    file.sync_all()
        .await
        .map_err(|_| "无法完成 Windows 安装包缓存".to_string())?;
    if downloaded != release.size {
        return Err("Windows 安装包下载不完整，请重试".into());
    }
    let actual = format!("{:x}", hasher.finalize());
    if release
        .sha256
        .as_deref()
        .is_some_and(|expected| expected != actual)
    {
        return Err("Windows 安装包 SHA-256 校验失败，未安装".into());
    }
    log::info!(
        "Codex Windows package downloaded operation_id={} bytes={} sha256={}",
        operation_id,
        downloaded,
        &actual[..12]
    );
    Ok((temporary, actual))
}

async fn verify_authenticode(path: &std::path::Path) -> Result<(), String> {
    let path = support::ps_literal(&path.to_string_lossy());
    let script = format!(
        r#"
$securityModule = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\Modules\Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1'
Import-Module $securityModule -ErrorAction Stop
$sig = Get-AuthenticodeSignature -LiteralPath {path}
@{{ status = [string]$sig.Status; subject = if ($sig.SignerCertificate) {{ [string]$sig.SignerCertificate.Subject }} else {{ '' }}; issuer = if ($sig.SignerCertificate) {{ [string]$sig.SignerCertificate.Issuer }} else {{ '' }} }} | ConvertTo-Json -Compress
"#
    );
    let report = support::powershell(&script, Duration::from_secs(30)).await?;
    let status = report
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let subject = report
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let issuer = report
        .get("issuer")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if package::is_valid_marketplace_signature(status, subject, issuer) {
        Ok(())
    } else {
        Err("安装包的 Microsoft Marketplace / OpenAI 签名验证失败，未安装".into())
    }
}

async fn install_msix(path: &std::path::Path, expected_version: &str) -> Result<(), String> {
    let path = support::ps_literal(&path.to_string_lossy());
    let version = support::ps_literal(expected_version);
    let publisher = support::ps_literal(package::PACKAGE_PUBLISHER);
    let script = format!(
        r#"
Add-AppxPackage -Path {path} -ErrorAction Stop
$matches = @(Get-AppxPackage -Name '{identity}' -ErrorAction Stop | Where-Object {{ [string]$_.Version -eq {version} -and [string]$_.Publisher -eq {publisher} }})
if ($matches.Count -ne 1) {{ throw 'installed package identity mismatch' }}
@{{ name = [string]$matches[0].Name; version = [string]$matches[0].Version; publisher = [string]$matches[0].Publisher }} | ConvertTo-Json -Compress
"#,
        identity = package::PACKAGE_IDENTITY,
    );
    let result = support::powershell(&script, Duration::from_secs(10 * 60)).await?;
    if result.get("name").and_then(Value::as_str) != Some(package::PACKAGE_IDENTITY)
        || result.get("version").and_then(Value::as_str) != Some(expected_version)
        || result.get("publisher").and_then(Value::as_str) != Some(package::PACKAGE_PUBLISHER)
    {
        return Err("Windows 未确认已安装目标 Codex 版本，请重新检测安装状态".into());
    }
    Ok(())
}

async fn fetch_text(url: &str, max_bytes: usize) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("版本信息地址无效".into());
    }
    let response = http_client(Duration::from_secs(30))?
        .get(url)
        .header(reqwest::header::USER_AGENT, "OSIR-Client")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "Windows 版本信息暂不可用，请重试".to_string())?
        .error_for_status()
        .map_err(|_| "Windows 版本信息暂不可用，请重试".to_string())?;
    if response
        .content_length()
        .is_some_and(|size| size > max_bytes as u64)
    {
        return Err("Windows 版本信息过大".into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "无法读取 Windows 版本信息".to_string())?;
    if bytes.len() > max_bytes {
        return Err("Windows 版本信息过大".into());
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| "Windows 版本信息编码无效".into())
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| "无法准备 Windows 版本检查".into())
}

async fn selected(target: Option<&str>) -> Result<(String, Option<u64>), String> {
    let entries = support::discover("codex").await?;
    if entries.len() > 1 {
        return Err("检测到多个 Codex 注册版本，请先在 Windows 应用设置中确认安装".into());
    }
    let entry = entries.first();
    let actual = entry
        .map(|entry| entry.root.clone())
        .unwrap_or_else(|| "Windows 当前用户应用包".into());
    if target.is_some_and(|path| !support::same_path(path, &actual)) {
        return Err("Windows 应用安装状态已变化，请重新检测".into());
    }
    Ok((
        actual,
        entry
            .map(|entry| support::version_number(&entry.version))
            .transpose()?,
    ))
}

fn format_package_name(release: &package::PackageRelease, architecture: &str) -> String {
    format!(
        "OpenAI.Codex_{}_{}__2p2nqsd0c76g0.Msix",
        release.package_version, architecture
    )
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
