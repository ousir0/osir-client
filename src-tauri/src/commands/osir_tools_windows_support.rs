//! Windows metadata and fixed-source policy. Pure policy tests never launch apps.
use serde::Deserialize;

pub(crate) const CODEX_PACKAGE: &str = "OpenAI.Codex";
pub(crate) const CODEX_PUBLISHER: &str = "CN=OpenAI OpCo, LLC, O=OpenAI OpCo, LLC, C=US";
pub(crate) const STORE_URL: &str = "https://apps.microsoft.com/detail/9PLM9XGG6VKS";

pub(crate) fn package_url(arch: &str) -> Result<&'static str, String> {
    match arch {
        "x86_64" => Ok("https://persistent.oaistatic.com/codex-app-prod/ChatGPT-x64.msix"),
        "aarch64" => Ok("https://persistent.oaistatic.com/codex-app-prod/ChatGPT-arm64.msix"),
        _ => Err("此 Windows 架构尚无已核实的官方安装包".into()),
    }
}

/// Encode all four MSIX version components losslessly, within JS's safe integer.
pub(crate) fn version_number(version: &str) -> Result<u64, String> {
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err("无法读取 Windows 应用版本".into());
    }
    let mut number = 0u64;
    for part in parts {
        let value: u64 = part.parse().map_err(|_| "无法读取 Windows 应用版本")?;
        if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) || value > 65535 {
            return Err("此 Windows 应用版本暂不能安全比较，请使用 Microsoft Store 更新".into());
        }
        number = number
            .checked_mul(65536)
            .and_then(|n| n.checked_add(value))
            .ok_or("Windows 应用版本超出安全比较范围")?;
    }
    if number > 9_007_199_254_740_991 {
        return Err("此 Windows 应用版本暂不能安全比较，请使用 Microsoft Store 更新".into());
    }
    Ok(number)
}

pub(crate) fn path_key(path: &str) -> String {
    let value = path.replace('/', "\\").to_lowercase();
    let value = value.strip_prefix(r"\\?\").unwrap_or(&value);
    value.trim_end_matches('\\').to_owned()
}

pub(crate) fn same_path(a: &str, b: &str) -> bool {
    !a.is_empty() && path_key(a) == path_key(b)
}

pub(crate) fn entry_relative(path: &str) -> bool {
    matches!(
        path_key(path).as_str(),
        r"app\chatgpt.exe" | r"app\codex.exe"
    )
}

pub(crate) fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn session_url(id: &str) -> Result<String, String> {
    crate::session_manager::resume::arguments("codex", id)?;
    Ok(format!("codex://threads/{id}"))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Installation {
    pub root: String,
    pub executable: String,
    pub relative: String,
    pub version: String,
    pub family: Option<String>,
    pub app_id: Option<String>,
    pub protocol: bool,
    pub package: Option<String>,
    pub publisher: String,
}

impl Installation {
    pub fn validate(&self, id: &str) -> Result<(), String> {
        let relative = self.relative.replace('/', "\\");
        let root = path_key(&self.root);
        if root.len() < 3
            || root.as_bytes().get(1) != Some(&b':')
            || root.as_bytes().get(2) != Some(&b'\\')
            || self.root.chars().any(char::is_control)
            || self.executable.chars().any(char::is_control)
            || !same_path(&format!("{root}\\{relative}"), &self.executable)
            || relative.split('\\').any(|p| matches!(p, "" | "." | ".."))
        {
            return Err("应用安装路径无效，请重新安装后重试".into());
        }
        if id == "codex" {
            version_number(&self.version)?;
            if self.package.as_deref() != Some(CODEX_PACKAGE)
                || !self.publisher.eq_ignore_ascii_case(CODEX_PUBLISHER)
                || !entry_relative(&relative)
                || self.family.as_deref() != Some("OpenAI.Codex_2p2nqsd0c76g0")
                || self.app_id.as_deref() != Some("App")
            {
                return Err("无法确认官方 Codex Desktop 安装身份".into());
            }
        } else if id != "claude-desktop"
            || !relative.eq_ignore_ascii_case("Claude.exe")
            || !self.publisher.to_ascii_lowercase().contains("anthropic")
        {
            return Err("无法确认桌面应用身份".into());
        }
        Ok(())
    }
}

// Package identity survives the Codex -> ChatGPT display-name change. Select
// the manifest's actual GUI application, never a bundled CLI or native host.
pub(crate) const CODEX_DISCOVERY: &str = r#"
$packages = @(Get-AppxPackage -Name 'OpenAI.Codex' -ErrorAction Stop)
$result = @()
foreach ($p in $packages) {
  $m = Get-AppxPackageManifest -Package $p.PackageFullName -ErrorAction Stop
  $apps = @($m.Package.Applications.Application | Where-Object {
    $_.Executable -match '^app[/\\](ChatGPT|Codex)\.exe$'
  })
  if ($apps.Count -ne 1) { throw 'ambiguous entry' }
  $a = $apps[0]
  $result += @{
    root = [string]$p.InstallLocation
    executable = [IO.Path]::GetFullPath((Join-Path $p.InstallLocation $a.Executable))
    relative = [string]$a.Executable
    version = [string]$p.Version
    family = [string]$p.PackageFamilyName
    appId = [string]$a.Id
    protocol = @($a.SelectNodes('.//*[local-name()="Protocol" and @Name="codex"]')).Count -gt 0
    package = [string]$p.Name
    publisher = [string]$p.Publisher
  }
}
ConvertTo-Json -InputObject @($result) -Depth 5 -Compress
"#;

// Squirrel installs are discovered only in the current user's standard root;
// verify signed product metadata before accepting a candidate executable.
pub(crate) const CLAUDE_DISCOVERY: &str = r#"
$root = Join-Path $env:LOCALAPPDATA 'AnthropicClaude'
$result = @()
if (Test-Path -LiteralPath $root -PathType Container) {
  $candidates = @((Join-Path $root 'Claude.exe'))
  $candidates += @(Get-ChildItem -LiteralPath $root -Directory -ErrorAction Stop |
    Where-Object { $_.Name -match '^app-[0-9][0-9A-Za-z.\-]*$' } |
    Select-Object -First 64 | ForEach-Object { Join-Path $_.FullName 'Claude.exe' })
  foreach ($file in $candidates) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { continue }
    $sig = Get-AuthenticodeSignature -LiteralPath $file -ErrorAction Stop
    $v = (Get-Item -LiteralPath $file -ErrorAction Stop).VersionInfo
    if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'Anthropic' -or $v.ProductName -ne 'Claude') { throw 'invalid Claude identity' }
    $result += @{
      root = Split-Path -Parent $file; executable = $file; relative = 'Claude.exe'
      version = [string]$v.ProductVersion; family = $null; appId = $null
      protocol = $false; package = $null; publisher = [string]$sig.SignerCertificate.Subject
    }
  }
}
ConvertTo-Json -InputObject @($result) -Depth 5 -Compress
"#;

#[cfg(windows)]
pub(crate) fn system_directory() -> Result<std::path::PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }
    let mut buffer = vec![0u16; 32768];
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if len == 0 || len as usize >= buffer.len() {
        return Err("无法找到 Windows 系统组件".into());
    }
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..len as usize],
    )))
}

#[cfg(windows)]
pub(crate) async fn powershell(
    script: &str,
    budget: std::time::Duration,
) -> Result<serde_json::Value, String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    let exe = system_directory()?.join("WindowsPowerShell/v1.0/powershell.exe");
    let prelude = "$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false);\n";
    let mut command = tokio::process::Command::new(exe);
    command.creation_flags(0x0800_0000);
    let mut child = command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("{prelude}{script}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "无法运行 Windows 应用检查，请确认系统策略允许 PowerShell")?;
    let stdout = child.stdout.take().ok_or("Windows 应用检查未完成")?;
    let run = async {
        let mut bytes = Vec::new();
        stdout
            .take(512 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "无法读取 Windows 应用状态")?;
        if bytes.len() > 512 * 1024 {
            return Err("Windows 应用检测结果过大".into());
        }
        let status = child.wait().await.map_err(|_| "Windows 应用检查未完成")?;
        if !status.success() {
            return Err("Windows 未完成此操作，请检查应用安装、系统策略或权限后重试".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "无法读取 Windows 应用状态".into())
    };
    tokio::time::timeout(budget, run)
        .await
        .map_err(|_| "Windows 应用操作超时，请重新检测状态")?
}

#[cfg(windows)]
pub(crate) async fn discover(id: &str) -> Result<Vec<Installation>, String> {
    let script = match id {
        "codex" => CODEX_DISCOVERY,
        "claude-desktop" => CLAUDE_DISCOVERY,
        _ => return Err("此 Windows 桌面应用尚未接入检测与控制".into()),
    };
    let json = powershell(script, std::time::Duration::from_secs(5)).await?;
    let entries: Vec<Installation> =
        serde_json::from_value(json).map_err(|_| "无法读取 Windows 应用安装信息")?;
    for entry in &entries {
        entry.validate(id)?;
        if !std::path::Path::new(&entry.executable).is_file() {
            return Err("应用入口已不可用，请修复安装后重试".into());
        }
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "osir_tools_windows_policy_tests.rs"]
mod tests;
