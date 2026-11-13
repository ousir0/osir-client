//! Inspect the app inside the final image, never infer app trust from DMG signing.
//! Security SDK: unsigned objects can be created; CopySigningInformation's
//! identifier is absent only for unsigned code; kSecCodeSignatureAdhoc is 0x2.
use super::{MacSignature, SystemSignature};
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    time::Duration,
};

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        a: *const c_void,
        b: *const u8,
        n: isize,
        directory: u8,
    ) -> *const c_void;
    fn CFStringCreateWithCString(
        a: *const c_void,
        text: *const std::ffi::c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, kind: isize, value: *mut c_void) -> u8;
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFRelease(value: *const c_void);
}
#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecStaticCodeCreateWithPath(
        path: *const c_void,
        flags: u32,
        code: *mut *const c_void,
    ) -> i32;
    fn SecStaticCodeCheckValidity(
        code: *const c_void,
        flags: u32,
        requirement: *const c_void,
    ) -> i32;
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: u32,
        information: *mut *const c_void,
    ) -> i32;
    fn SecRequirementCreateWithString(
        text: *const c_void,
        flags: u32,
        requirement: *mut *const c_void,
    ) -> i32;
    static kSecCodeInfoIdentifier: *const c_void;
    static kSecCodeInfoFlags: *const c_void;
}
struct Owned(*const c_void);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

pub(super) fn signature(path: &Path) -> Result<MacSignature, String> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let url = Owned(unsafe {
        CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            path.is_dir() as u8,
        )
    });
    if url.0.is_null() {
        return Err("无法读取应用签名路径".into());
    }
    let mut raw = std::ptr::null();
    let created = unsafe { SecStaticCodeCreateWithPath(url.0, 0, &mut raw) };
    let code = Owned(raw);
    if created != 0 || code.0.is_null() {
        return Err("无法识别应用系统签名".into());
    }
    // Strict validation, all slices, and nested code. Integrity success does not
    // establish signer identity or notarization. No invalid status is unsigned.
    let status = unsafe { SecStaticCodeCheckValidity(code.0, 1 | 8 | 16, std::ptr::null()) };
    let mut raw = std::ptr::null();
    let copied = unsafe { SecCodeCopySigningInformation(code.0, 0, &mut raw) };
    let info = Owned(raw);
    if copied != 0 || info.0.is_null() {
        return Err("应用签名信息损坏".into());
    }
    let identifier = unsafe { CFDictionaryGetValue(info.0, kSecCodeInfoIdentifier) };
    if status == -67062 && identifier.is_null() {
        return super::macos_integrity(status, false, false);
    }
    if status != 0 || identifier.is_null() {
        return Err("应用或内嵌代码签名损坏，已阻止打开".into());
    }
    let flags = unsafe { CFDictionaryGetValue(info.0, kSecCodeInfoFlags) };
    let mut bits: i64 = 0;
    if flags.is_null()
        || unsafe {
            CFGetTypeID(flags) != CFNumberGetTypeID()
                || CFNumberGetValue(flags, 4, (&mut bits as *mut i64).cast()) == 0
        }
    {
        return Err("应用签名类型不可确认".into());
    }
    if bits & 2 != 0 {
        return super::macos_integrity(status, true, true);
    }
    // Explicit Developer ID certificate extension + Apple anchor. Valid ad-hoc,
    // self-signed and other Apple certificates cannot satisfy this requirement.
    let text = Owned(unsafe {
        CFStringCreateWithCString(std::ptr::null(), c"anchor apple generic and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and identifier \"com.osir.anywhere.desktop\"".as_ptr(), 0x08000100)
    });
    if text.0.is_null() {
        return Err("无法验证 Developer ID 身份".into());
    }
    let mut raw = std::ptr::null();
    let created = unsafe { SecRequirementCreateWithString(text.0, 0, &mut raw) };
    let requirement = Owned(raw);
    if created != 0 || requirement.0.is_null() {
        return Err("无法验证 Developer ID 身份".into());
    }
    let status = unsafe { SecStaticCodeCheckValidity(code.0, 1 | 8 | 16, requirement.0) };
    Ok(if status == 0 {
        MacSignature::DeveloperId
    } else {
        MacSignature::OtherSigned
    })
}

async fn output(command: &mut tokio::process::Command) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| "无法读取安装包元数据")?;
    let stdout = child.stdout.take().ok_or("无法读取安装包元数据")?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut bytes = Vec::new();
        stdout
            .take(256 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "无法读取安装包元数据")?;
        if bytes.len() > 256 * 1024 {
            return Err("安装包元数据过大".into());
        }
        if !child.wait().await.map_err(|_| "元数据读取中断")?.success() {
            return Err("安装包元数据无效".into());
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| "元数据读取超时")?;
    result
}

pub(crate) fn metadata(metadata: &serde_json::Value, expected: &str) -> Result<(), String> {
    for (field, value) in [
        ("CFBundleIdentifier", "com.osir.anywhere.desktop"),
        ("CFBundleName", "OSIR Anywhere"),
        ("CFBundleDisplayName", "OSIR Anywhere"),
        ("CFBundleExecutable", "OSIR Anywhere"),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", expected),
    ] {
        if metadata.get(field).and_then(|v| v.as_str()) != Some(value) {
            return Err("安装包内应用身份或版本与发行清单不符".into());
        }
    }
    Ok(())
}

pub(crate) fn package(path: &Path, expected: &str) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|_| "无法读取应用产品信息")?;
    let size = file.metadata().map_err(|_| "无法读取应用产品信息")?.len();
    let mut prefix = [0u8; 16];
    file.read_exact(&mut prefix)
        .map_err(|_| "应用产品信息不完整")?;
    let word = |at| u32::from_le_bytes(prefix[at..at + 4].try_into().unwrap()) as u64;
    let header_size = word(4);
    let json_size = word(12);
    if word(0) != 4
        || header_size < 8
        || header_size > 8 * 1024 * 1024
        || json_size > header_size - 8
        || header_size + 8 > size
    {
        return Err("应用产品信息结构无效".into());
    }
    let mut bytes = vec![0; json_size as usize];
    file.read_exact(&mut bytes)
        .map_err(|_| "应用产品信息不完整")?;
    let header: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "应用产品信息无效")?;
    let entry = &header["files"]["package.json"];
    if entry.get("link").is_some()
        || entry
            .get("unpacked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return Err("应用产品信息不能使用外部链接".into());
    }
    let offset = entry
        .get("offset")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .ok_or("应用产品信息偏移无效")?
        .parse::<u64>()
        .map_err(|_| "应用产品信息偏移无效")?;
    let length = entry
        .get("size")
        .and_then(|v| v.as_u64())
        .filter(|n| (2..=256 * 1024).contains(n))
        .ok_or("应用产品信息大小无效")?;
    let offset = 8u64
        .checked_add(header_size)
        .and_then(|n| n.checked_add(offset))
        .ok_or("应用产品信息边界无效")?;
    if offset.checked_add(length).filter(|n| *n <= size).is_none() {
        return Err("应用产品信息超出包边界".into());
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "无法读取应用产品信息")?;
    bytes.resize(length as usize, 0);
    file.read_exact(&mut bytes)
        .map_err(|_| "应用产品信息不完整")?;
    let metadata: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "应用产品信息无效")?;
    for (key, value) in [
        ("name", "osir-anywhere-desktop-workbench"),
        ("productName", "OSIR Anywhere"),
        ("main", "dist/electron/main.js"),
        ("version", expected),
    ] {
        if metadata.get(key).and_then(|v| v.as_str()) != Some(value) {
            return Err("应用产品或版本与发行声明不符".into());
        }
    }
    Ok(())
}

fn contained_file(app: &Path, relative: &str, limit: u64) -> Result<PathBuf, String> {
    let path = app.join(relative);
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "安装包缺少应用文件")?;
    if !meta.is_file()
        || meta.is_symlink()
        || meta.len() > limit
        || !path
            .canonicalize()
            .map_err(|_| "应用路径无效")?
            .starts_with(app)
    {
        return Err("安装包应用文件路径或大小无效".into());
    }
    Ok(path)
}

fn verify_nested(app: &Path) -> Result<(), String> {
    use std::io::Read;
    // An unsigned root has no seal covering nested code. Inspect every Mach-O
    // and nested bundle independently so a damaged embedded signature still fails.
    let started = std::time::Instant::now();
    let mut queue = vec![app.to_owned()];
    let mut count = 0;
    while let Some(path) = queue.pop() {
        count += 1;
        if count > 100_000 || started.elapsed() > Duration::from_secs(180) {
            return Err("应用内容检查超过安全限制".into());
        }
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "无法读取应用内容")?;
        if meta.is_symlink() {
            if !path
                .canonicalize()
                .map_err(|_| "应用链接损坏")?
                .starts_with(app)
            {
                return Err("应用链接超出安装包".into());
            }
            continue;
        }
        if meta.is_dir() {
            if path != app
                && matches!(
                    path.extension().and_then(|x| x.to_str()),
                    Some("app" | "framework" | "xpc" | "appex" | "bundle")
                )
            {
                signature(&path)?;
            }
            for entry in std::fs::read_dir(&path).map_err(|_| "无法读取应用目录")? {
                if queue.len() + count >= 100_000 {
                    return Err("应用内容检查超过安全限制".into());
                }
                queue.push(entry.map_err(|_| "无法读取应用目录")?.path());
            }
        } else if meta.is_file() && meta.len() >= 4 {
            let mut magic = [0; 4];
            std::fs::File::open(&path)
                .and_then(|mut f| f.read_exact(&mut magic))
                .map_err(|_| "无法读取应用代码")?;
            if matches!(
                u32::from_be_bytes(magic),
                0xfeedface
                    | 0xfeedfacf
                    | 0xcefaedfe
                    | 0xcffaedfe
                    | 0xcafebabe
                    | 0xbebafeca
                    | 0xcafebabf
                    | 0xbfbafeca
            ) {
                signature(&path)?;
            }
        } else if !meta.is_file() {
            return Err("安装包包含不支持的文件类型".into());
        }
    }
    Ok(())
}

struct Mount {
    root: Option<tempfile::TempDir>,
    path: PathBuf,
}
impl Drop for Mount {
    fn drop(&mut self) {
        if let Some(root) = self.root.take() {
            // Never recursively delete a mountpoint if detachment fails. Normal
            // paths explicitly detach below; shutdown retains the private root.
            let _ = root.keep();
            let _ = std::process::Command::new("/usr/bin/hdiutil")
                .arg("detach")
                .arg(&self.path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}

pub(super) async fn verify_dmg(
    image: &Path,
    policy: SystemSignature,
    version: &str,
) -> Result<(), String> {
    super::super::io::system_command(
        tokio::process::Command::new("/usr/bin/hdiutil")
            .args(["verify", "-quiet"])
            .arg(image),
        Duration::from_secs(90),
    )
    .await?;
    let root = super::super::io::private_cache().await?;
    let path = root.path().join("volume");
    std::fs::create_dir(&path).map_err(|_| "无法创建只读检查目录")?;
    let mut mount = Mount {
        root: Some(root),
        path,
    };
    // Always detach, including a timeout/partial attach. Neither Finder nor app
    // launch occurs while verifying the final release image.
    let result = async {
        super::super::io::system_command(
            tokio::process::Command::new("/usr/bin/hdiutil")
                .args([
                    "attach",
                    "-readonly",
                    "-nobrowse",
                    "-noautoopen",
                    "-mountpoint",
                ])
                .arg(&mount.path)
                .arg(image),
            Duration::from_secs(90),
        )
        .await?;
        let app = mounted_app(&mount.path)?;
        let plist = contained_file(&app, "Contents/Info.plist", 256 * 1024)?;
        let bytes = output(
            tokio::process::Command::new("/usr/bin/plutil")
                .args(["-convert", "json", "-o", "-", "--"])
                .arg(plist),
        )
        .await?;
        let info = serde_json::from_slice(&bytes).map_err(|_| "应用元数据无效")?;
        metadata(&info, version)?;
        let asar = contained_file(&app, "Contents/Resources/app.asar", u64::MAX)?;
        package(&asar, version)?;
        let executable = contained_file(&app, "Contents/MacOS/OSIR Anywhere", u64::MAX)?;
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(executable)
            .map_err(|_| "应用主程序不存在")?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err("应用主程序不可执行".into());
        }
        let inspect = app.clone();
        let actual = tokio::task::spawn_blocking(move || {
            let actual = signature(&inspect)?;
            verify_nested(&inspect)?;
            Ok::<_, String>(actual)
        })
        .await
        .map_err(|_| "应用签名检查中断")??;
        let notarized = if policy == SystemSignature::DeveloperIdNotarized
            && actual == MacSignature::DeveloperId
        {
            // Match release inspection: the final image has a stapled ticket,
            // and Gatekeeper assesses the contained app as executable code.
            super::super::io::system_command(
                tokio::process::Command::new("/usr/bin/xcrun")
                    .args(["stapler", "validate"])
                    .arg(image),
                Duration::from_secs(90),
            )
            .await?;
            super::super::io::system_command(
                tokio::process::Command::new("/usr/sbin/spctl")
                    .args(["--assess", "--type", "execute"])
                    .arg(&app),
                Duration::from_secs(90),
            )
            .await?;
            true
        } else {
            false
        };
        super::macos_policy(policy, actual, notarized)
    }
    .await;
    let detach = super::super::io::system_command(
        tokio::process::Command::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg(&mount.path),
        Duration::from_secs(30),
    )
    .await;
    if detach.is_ok() {
        drop(mount.root.take());
    } else {
        return Err("安装包检查卷未能卸载，请重试或联系支持".into());
    }
    result
}

pub(crate) fn mounted_app(root: &Path) -> Result<PathBuf, String> {
    let mut apps = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|_| "无法读取安装包")? {
        let entry = entry.map_err(|_| "无法读取安装包")?;
        if entry.path().extension().and_then(|x| x.to_str()) == Some("app") {
            apps.push(entry.path());
        }
    }
    if apps.len() != 1 || apps[0].file_name().and_then(|n| n.to_str()) != Some("OSIR Anywhere.app")
    {
        return Err("安装包应包含唯一 OSIR Anywhere 应用".into());
    }
    let app = apps.pop().ok_or("安装包缺少应用")?;
    let meta = std::fs::symlink_metadata(&app).map_err(|_| "应用路径无效")?;
    if !meta.is_dir() || meta.is_symlink() {
        return Err("安装包应用不能是链接".into());
    }
    let canonical = app.canonicalize().map_err(|_| "应用路径无效")?;
    if !canonical.starts_with(root.canonicalize().map_err(|_| "检查目录无效")?) {
        return Err("应用超出安装包".into());
    }
    Ok(canonical)
}
