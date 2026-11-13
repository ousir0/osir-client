use super::{manifest, AnywhereInstallPhase, OsirAnywhereInstaller};
use futures::StreamExt;
use manifest::Artifact;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::watch,
};

pub(super) fn target() -> Result<(&'static str, &'static str), String> {
    #[cfg(target_os = "macos")]
    {
        // Detect hardware even when Client itself is running under Rosetta.
        let mut arm: i32 = 0;
        let mut size = std::mem::size_of_val(&arm);
        let ok = unsafe {
            libc::sysctlbyname(
                c"hw.optional.arm64".as_ptr(),
                (&mut arm as *mut i32).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if ok == 0 && arm == 1 {
            return Ok(("darwin", "arm64"));
        }
        if std::env::consts::ARCH == "x86_64" {
            return Ok(("darwin", "x64"));
        }
        return Err("无法确认此 Mac 的 CPU 架构".into());
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn IsWow64Process2(
                process: windows_sys::Win32::Foundation::HANDLE,
                process_machine: *mut u16,
                native_machine: *mut u16,
            ) -> i32;
        }
        let (mut process, mut native) = (0, 0);
        if unsafe {
            IsWow64Process2(
                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                &mut process,
                &mut native,
            )
        } == 0
        {
            return Err("无法确认 Windows 的 CPU 架构".into());
        }
        return match native {
            0x8664 => Ok(("win32", "x64")),
            0xaa64 => Ok(("win32", "arm64")),
            _ => Err("此 Windows CPU 架构尚不支持".into()),
        };
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    Err("此系统尚不支持 Anywhere 桌面安装".into())
}

pub(super) async fn private_cache() -> Result<tempfile::TempDir, String> {
    let dir = tempfile::Builder::new()
        .prefix("osir-anywhere-")
        .tempdir()
        .map_err(|_| "无法创建安装缓存，请检查临时目录权限")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "无法保护安装缓存")?;
    }
    #[cfg(windows)]
    {
        use super::super::osir_tools::windows_support::{powershell, ps_literal};
        // No package bytes exist until a protected user/SYSTEM-only DACL is in
        // place. Installer files inherit this ACL, including custom TEMP roots.
        let script = format!(
            r#"
$path = {}
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = [Security.AccessControl.DirectorySecurity]::new()
$acl.SetOwner($sid)
$acl.SetAccessRuleProtection($true, $false)
foreach ($who in @($sid, [Security.Principal.SecurityIdentifier]::new('S-1-5-18'))) {{
  $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($who, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
}}
# This helper always runs Windows PowerShell 5.1/.NET Framework. Call its ACL
# APIs directly: an inherited PowerShell 7 PSModulePath can break Security
# cmdlet autoloading even with -NoProfile and an absolute powershell.exe.
[IO.Directory]::SetAccessControl($path, $acl)
$actual = [IO.Directory]::GetAccessControl($path)
if (-not $actual.AreAccessRulesProtected -or $actual.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid.Value) {{ throw 'cache protection' }}
'true'
"#,
            ps_literal(&dir.path().to_string_lossy())
        );
        powershell(&script, Duration::from_secs(15)).await?;
    }
    Ok(dir)
}

fn regular_file(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "安装缓存不可用")?;
    if !meta.is_file() || meta.is_symlink() {
        return Err("安装缓存身份发生变化".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0
        {
            return Err("安装缓存权限不安全".into());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err("安装缓存包含不安全链接".into());
        }
    }
    Ok(())
}

/// On Windows retain a read-only sharing handle across signature/hash checks and
/// ShellExecute, rejecting write/delete races, reparse points and hard links.
pub(super) fn pin_file(path: &Path) -> Result<std::fs::File, String> {
    regular_file(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x0020_0000); // FILE_SHARE_READ / OPEN_REPARSE_POINT
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_| "安装缓存被占用或已改变，请重试")?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0
            || info.dwFileAttributes & 0x400 != 0
            || info.nNumberOfLinks != 1
        {
            return Err("安装缓存包含不安全链接".into());
        }
    }
    Ok(file)
}

/// Native HTTP clients do not inherit browser download metadata. Mark verified
/// downloads as Internet-origin before opening, including reused cache files,
/// so the OS retains its usual Gatekeeper/SmartScreen confirmation path.
pub(super) async fn mark_download_origin(path: &Path, artifact: &Artifact) -> Result<(), String> {
    regular_file(path)?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let file = pin_file(path)?;
        let name = c"com.apple.quarantine";
        let existing = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                0,
            )
        };
        if existing > 0 {
            return Ok(());
        }
        if existing < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOATTR) {
            return Err("无法保留安装包的系统安全确认，请重试".into());
        }
        let value = format!(
            "0081;{:x};OSIR Client;{}",
            chrono::Utc::now().timestamp(),
            uuid::Uuid::new_v4()
        );
        if unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        } != 0
        {
            return Err("无法设置安装包的系统安全确认，已阻止打开".into());
        }
        let _ = artifact;
        Ok(())
    }
    #[cfg(windows)]
    {
        let mut zone = path.as_os_str().to_os_string();
        zone.push(":Zone.Identifier");
        let zone = PathBuf::from(zone);
        if let Ok(file) = tokio::fs::File::open(&zone).await {
            let mut existing = Vec::new();
            file.take(4097)
                .read_to_end(&mut existing)
                .await
                .map_err(|_| "无法检查安装包的系统安全标记")?;
            if existing.len() > 4096 {
                return Err("安装包系统安全标记异常".into());
            }
            if String::from_utf8_lossy(&existing)
                .lines()
                .any(|line| matches!(line.trim(), "ZoneId=3" | "ZoneId=4"))
            {
                return Ok(());
            }
        }
        let value = format!(
            "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl={}\r\nHostUrl={}\r\n",
            manifest::FEED,
            artifact.url
        );
        tokio::fs::write(&zone, value)
            .await
            .map_err(|_| "无法设置安装包的系统安全确认，已阻止打开".into())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (path, artifact);
        Err("此系统尚不支持安装安全确认".into())
    }
}

async fn space(path: &Path, bytes: u64) -> Result<(), String> {
    let required = bytes.saturating_mul(4).saturating_add(512 * 1024 * 1024);
    #[cfg(unix)]
    let available = {
        use std::os::unix::ffi::OsStrExt;
        let path =
            std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| "缓存目录无效")?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err("无法检查安装所需空间".into());
        }
        let stat = unsafe { stat.assume_init() };
        (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
    };
    #[cfg(windows)]
    let available = {
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0;
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err("无法检查安装所需空间".into());
        }
        available
    };
    #[cfg(not(any(unix, windows)))]
    let available = 0;
    if available < required {
        return Err("可用空间不足，请清理临时目录或安装磁盘后重试".into());
    }
    Ok(())
}

struct Partial(PathBuf);
impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) async fn verify_file(
    path: &Path,
    artifact: &Artifact,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    regular_file(path)?;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| "无法读取安装缓存")?;
    if file
        .metadata()
        .await
        .map_err(|_| "无法读取安装包大小")?
        .len()
        != artifact.size
    {
        return Err("安装包大小校验失败".into());
    }
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let count = tokio::select! {
            _ = cancel.wait_for(|cancelled| *cancelled) => return Err("下载已取消".into()),
            count = file.read(&mut buffer) => count.map_err(|_| "安装缓存读取失败")?,
        };
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > artifact.size {
            return Err("安装包大小发生变化".into());
        }
        hash.update(&buffer[..count]);
    }
    if total != artifact.size
        || format!("{:x}", hash.finalize()) != artifact.sha256.to_ascii_lowercase()
    {
        return Err("安装包校验失败，已阻止打开，请重新下载".into());
    }
    // Reject HTML/error pages and the wrong container even with signed metadata.
    if artifact.platform == "darwin" {
        file.seek(std::io::SeekFrom::End(-512))
            .await
            .map_err(|_| "DMG 格式无效")?;
        let mut magic = [0; 4];
        file.read_exact(&mut magic)
            .await
            .map_err(|_| "DMG 格式无效")?;
        if &magic != b"koly" {
            return Err("DMG 格式无效".into());
        }
    } else {
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|_| "安装器格式无效")?;
        let mut header = [0; 64];
        file.read_exact(&mut header)
            .await
            .map_err(|_| "安装器格式无效")?;
        let offset =
            u32::from_le_bytes(header[60..64].try_into().map_err(|_| "安装器格式无效")?) as u64;
        if &header[..2] != b"MZ" || offset < 64 || offset > artifact.size.saturating_sub(4) {
            return Err("安装器格式无效".into());
        }
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| "安装器格式无效")?;
        let mut magic = [0; 4];
        file.read_exact(&mut magic)
            .await
            .map_err(|_| "安装器格式无效")?;
        if magic != *b"PE\0\0" {
            return Err("安装器格式无效".into());
        }
    }
    Ok(())
}

pub(super) async fn download(
    installer: &OsirAnywhereInstaller,
    root: &Path,
    artifact: &Artifact,
    cancel: &mut watch::Receiver<bool>,
) -> Result<PathBuf, String> {
    download_with_client(installer, root, artifact, cancel, manifest::http_client()?).await
}

// Production always supplies the fixed HTTPS/no-redirect client above. Injection
// is private to this module tree for loopback fixture tests, never IPC or env.
pub(super) async fn download_with_client(
    installer: &OsirAnywhereInstaller,
    root: &Path,
    artifact: &Artifact,
    cancel: &mut watch::Receiver<bool>,
    client: reqwest::Client,
) -> Result<PathBuf, String> {
    let extension = if artifact.platform == "darwin" {
        "dmg"
    } else {
        "exe"
    };
    let path = root.join(format!(
        "{}.{}",
        artifact.sha256.to_ascii_lowercase(),
        extension
    ));
    if path.exists() {
        if verify_file(&path, artifact, cancel).await.is_ok() {
            return Ok(path);
        }
        if *cancel.borrow() {
            return Err("下载已取消".into());
        }
        tokio::fs::remove_file(&path)
            .await
            .map_err(|_| "无法清理损坏的安装缓存")?;
    }
    // Check both download volume and the usual application volume. A custom
    // NSIS destination is checked again by the system installer itself.
    space(root, artifact.size).await?;
    #[cfg(target_os = "macos")]
    space(Path::new("/Applications"), artifact.size).await?;
    #[cfg(windows)]
    if let Some(home) = dirs::data_local_dir() {
        space(&home, artifact.size).await?;
    }
    let partial = Partial(root.join(format!("{}.part", uuid::Uuid::new_v4())));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&partial.0)
        .await
        .map_err(|_| "无法写入安装缓存，请检查权限")?;
    let transfer = async {
        let response = client
            .get(&artifact.url)
            .header("Accept-Encoding", "identity")
            .send()
            .await
            .map_err(|_| "安装包下载失败，请检查网络后重试")?;
        if response.status() != reqwest::StatusCode::OK
            || response.url().as_str() != artifact.url
            || response
                .headers()
                .contains_key(reqwest::header::CONTENT_ENCODING)
            || response
                .content_length()
                .is_some_and(|size| size != artifact.size)
        {
            return Err("安装包响应与发行清单不符，未跟随跳转".into());
        }
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|value| {
                let value = value.to_ascii_lowercase();
                value.contains("text/") || value.contains("json") || value.contains("html")
            })
        {
            return Err("下载地址未返回安装包".into());
        }
        let mut stream = response.bytes_stream();
        let mut downloaded = 0u64;
        let mut hash = Sha256::new();
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(60), stream.next())
            .await
            .map_err(|_| "下载长时间没有响应，请重试")?
        {
            let chunk = chunk.map_err(|_| "下载中断，请重试")?;
            downloaded = downloaded
                .checked_add(chunk.len() as u64)
                .ok_or("安装包过大")?;
            if downloaded > artifact.size {
                return Err("安装包超过声明大小，已停止下载".into());
            }
            file.write_all(&chunk)
                .await
                .map_err(|_| "安装包写入失败，请检查空间及权限")?;
            hash.update(&chunk);
            installer.progress(
                AnywhereInstallPhase::Downloading,
                downloaded,
                Some(artifact.size),
            );
        }
        if downloaded != artifact.size
            || format!("{:x}", hash.finalize()) != artifact.sha256.to_ascii_lowercase()
        {
            return Err("安装包大小或校验值不符，已阻止打开".into());
        }
        file.flush().await.map_err(|_| "安装包写入失败")?;
        file.sync_all().await.map_err(|_| "安装包保存失败")?;
        Ok::<(), String>(())
    };
    tokio::select! {
        _ = cancel.wait_for(|cancelled| *cancelled) => return Err("下载已取消".into()),
        result = tokio::time::timeout(Duration::from_secs(2 * 60 * 60), transfer) => result.map_err(|_| "安装包下载超时，请重试")??,
    }
    drop(file);
    verify_file(&partial.0, artifact, cancel).await?;
    tokio::fs::rename(&partial.0, &path)
        .await
        .map_err(|_| "无法保存已校验的安装包")?;
    Ok(path)
}

pub(super) async fn system_command(
    command: &mut tokio::process::Command,
    budget: Duration,
) -> Result<(), String> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(budget, command.status())
        .await
        .map_err(|_| "系统安装包检查超时，请重试")?
        .map_err(|_| "系统无法处理此安装包")?;
    if !status.success() {
        return Err("系统未信任此安装包，已阻止打开，请联系发行方".into());
    }
    Ok(())
}

pub(super) async fn verify_system_package(
    path: &Path,
    artifact: &Artifact,
    version: &str,
) -> Result<(), String> {
    super::signature::verify(path, artifact, version).await
}

pub(super) async fn open_installer(path: &Path) -> Result<(), String> {
    regular_file(path)?;
    #[cfg(target_os = "macos")]
    {
        // Launch Services mounts the validated DMG and shows its drag-to-Applications UI.
        system_command(
            tokio::process::Command::new("/usr/bin/open")
                .args(["-b", "com.apple.DiskImageMounter"])
                .arg(path),
            Duration::from_secs(30),
        )
        .await
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // ShellExecute preserves NSIS elevation/confirmation and directory choice.
        // No /S, registry command string, shell interpolation or automatic restart.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        if result as isize <= 32 {
            return Err("安装器未打开或系统确认已取消，请重试".into());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        Err("此系统尚不支持打开安装器".into())
    }
}
