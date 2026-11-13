//! System signing policy is authenticated by the release payload. Absence of a
//! signature is a specific native result, never a catch-all verification error.
use super::manifest::{Artifact, SystemSignature};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MacSignature {
    Unsigned,
    AdHoc,
    DeveloperId,
    OtherSigned,
}

pub(super) fn macos_integrity(
    status: i32,
    identifier: bool,
    adhoc: bool,
) -> Result<MacSignature, String> {
    match (status, identifier, adhoc) {
        (-67062, false, false) => Ok(MacSignature::Unsigned),
        (0, true, true) => Ok(MacSignature::AdHoc),
        (0, true, false) => Ok(MacSignature::OtherSigned),
        _ => Err("应用或内嵌代码签名损坏，已阻止打开".into()),
    }
}

// Integrity must succeed before classifying any signed code. Notarization is
// independent of integrity/Developer ID and must be assessed on the mounted app.
pub(super) fn macos_policy(
    policy: SystemSignature,
    actual: MacSignature,
    notarized: bool,
) -> Result<(), String> {
    match (policy, actual, notarized) {
        (SystemSignature::Unsigned, MacSignature::Unsigned | MacSignature::AdHoc, _)
        | (SystemSignature::DeveloperIdNotarized, MacSignature::DeveloperId, true) => Ok(()),
        _ => Err("应用系统签名与发行声明不符，已阻止打开".into()),
    }
}

#[cfg(target_os = "macos")]
#[path = "osir_anywhere_install_macos.rs"]
pub(super) mod macos;

pub(super) fn windows_policy(
    policy: SystemSignature,
    status: &str,
    certificate: bool,
    product: &str,
) -> Result<(), String> {
    if product != "OSIR Anywhere" {
        return Err("安装器产品身份不匹配，已阻止打开".into());
    }
    match (policy, status, certificate) {
        (SystemSignature::Unsigned, "NotSigned", false)
        | (SystemSignature::Authenticode, "Valid", true) => Ok(()),
        _ => Err("安装器系统签名校验失败或与发行声明不符，已阻止打开".into()),
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at + 2)
            .ok_or("PE 安装器结构不完整")?
            .try_into()
            .map_err(|_| "PE 安装器结构无效")?,
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or("PE 安装器结构不完整")?
            .try_into()
            .map_err(|_| "PE 安装器结构无效")?,
    ))
}

/// Parse PE certificate-directory presence independently of PowerShell's
/// NotSigned label; malformed certificate entries cannot masquerade as absent.
/// Also require NSIS's actual firstheader at a 512-byte boundary in the overlay.
pub(super) fn pe_nsis_certificate(path: &Path) -> Result<bool, String> {
    let mut file = std::fs::File::open(path).map_err(|_| "无法读取 Windows 安装器")?;
    let size = file
        .metadata()
        .map_err(|_| "无法读取 Windows 安装器大小")?
        .len();
    let mut dos = [0; 64];
    file.read_exact(&mut dos)
        .map_err(|_| "PE 安装器结构不完整")?;
    let pe_offset = u32_at(&dos, 60)? as u64;
    if &dos[..2] != b"MZ" || pe_offset < 64 || pe_offset + 24 > size {
        return Err("PE 安装器格式无效".into());
    }
    file.seek(SeekFrom::Start(pe_offset))
        .map_err(|_| "PE 安装器结构无效")?;
    let mut header = [0; 24];
    file.read_exact(&mut header)
        .map_err(|_| "PE 安装器结构不完整")?;
    let sections = u16_at(&header, 6)? as usize;
    let optional_size = u16_at(&header, 20)? as usize;
    if &header[..4] != b"PE\0\0"
        || !matches!(u16_at(&header, 4)?, 0x14c | 0x8664 | 0xaa64)
        || u16_at(&header, 22)? & 2 == 0
        || sections == 0
        || sections > 96
        || optional_size < 96
        || optional_size > 4096
    {
        return Err("PE 安装器结构无效".into());
    }
    let mut optional = vec![0; optional_size];
    file.read_exact(&mut optional)
        .map_err(|_| "PE 安装器结构不完整")?;
    let (directories, table) = match u16_at(&optional, 0)? {
        0x10b => (92, 96),
        0x20b => (108, 112),
        _ => return Err("PE 安装器结构无效".into()),
    };
    let (certificate_offset, certificate_size) = if u32_at(&optional, directories)? > 4 {
        (
            u32_at(&optional, table + 32)? as u64,
            u32_at(&optional, table + 36)? as u64,
        )
    } else {
        (0, 0)
    };
    let certificate = certificate_offset != 0 || certificate_size != 0;
    if certificate
        && (certificate_offset == 0
            || certificate_size < 8
            || certificate_offset % 8 != 0
            || certificate_offset + certificate_size > size)
    {
        return Err("安装器证书表损坏，已阻止打开".into());
    }
    let mut overlay = pe_offset + 24 + optional_size as u64 + sections as u64 * 40;
    for _ in 0..sections {
        let mut section = [0; 40];
        file.read_exact(&mut section)
            .map_err(|_| "PE 区段结构不完整")?;
        let end = u32_at(&section, 20)? as u64 + u32_at(&section, 16)? as u64;
        if end > size {
            return Err("PE 区段超出安装包边界".into());
        }
        overlay = overlay.max(end);
    }
    let data_end = if certificate {
        certificate_offset
    } else {
        size
    };
    if overlay > data_end || overlay > 64 * 1024 * 1024 {
        return Err("NSIS 安装器结构无效".into());
    }
    let first = (overlay + 511) / 512 * 512;
    let scan_end = data_end.min(first + 64 * 1024);
    for offset in (first..scan_end.saturating_sub(27)).step_by(512) {
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| "NSIS 安装器结构无效")?;
        let mut nsis = [0; 28];
        file.read_exact(&mut nsis)
            .map_err(|_| "NSIS 安装器结构不完整")?;
        if &nsis[4..20] != b"\xef\xbe\xad\xdeNullsoftInst" {
            continue;
        }
        let flags = u32_at(&nsis, 0)?;
        let header_size = u32_at(&nsis, 20)? as u64;
        let all_size = u32_at(&nsis, 24)? as u64;
        if flags & !15 == 0
            && flags & 1 == 0
            && header_size > 0
            && all_size >= 28
            && offset + all_size <= data_end
        {
            return Ok(certificate);
        }
        return Err("NSIS 安装器数据边界无效".into());
    }
    Err("此文件不是受支持的 NSIS 安装器".into())
}

pub(super) async fn verify(path: &Path, artifact: &Artifact, version: &str) -> Result<(), String> {
    let policy = artifact.signature_policy()?;
    #[cfg(target_os = "macos")]
    {
        if artifact.platform != "darwin" {
            return Err("安装包平台不匹配".into());
        }
        macos::verify_dmg(path, policy, version).await
    }
    #[cfg(windows)]
    {
        let _ = version;
        if artifact.platform != "win32" {
            return Err("安装包平台不匹配".into());
        }
        use super::super::osir_tools::windows_support::{powershell, ps_literal};
        let owned_path = path.to_owned();
        let certificate = tokio::task::spawn_blocking(move || pe_nsis_certificate(&owned_path))
            .await
            .map_err(|_| "PE 安装器检查中断")??;
        let script = format!(
            r#"
$file = {}
$sig = Get-AuthenticodeSignature -LiteralPath $file -ErrorAction Stop
$version = (Get-Item -LiteralPath $file -ErrorAction Stop).VersionInfo
ConvertTo-Json -Compress -InputObject @{{status=[string]$sig.Status;product=[string]$version.ProductName}}
"#,
            ps_literal(&path.to_string_lossy())
        );
        let report = powershell(&script, Duration::from_secs(90)).await?;
        windows_policy(
            policy,
            report
                .get("status")
                .and_then(|v| v.as_str())
                .ok_or("系统签名结果无效")?,
            certificate,
            report
                .get("product")
                .and_then(|v| v.as_str())
                .ok_or("安装器产品信息无效")?,
        )
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (path, policy, version);
        Err("此系统尚不支持安装包验证".into())
    }
}
