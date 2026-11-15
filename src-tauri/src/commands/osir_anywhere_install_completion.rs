//! Installation evidence, distinct from opening a DMG or an existing app.
//! Paths come only from native detection. No user configuration is read.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Marker {
    path: PathBuf,
    size: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(windows)]
    identity: (u32, u64),
    #[cfg(not(any(unix, windows)))]
    created: std::time::SystemTime,
}

pub(super) fn marker(bundle: &Path) -> Result<Marker, String> {
    let relative = if cfg!(target_os = "windows") {
        "resources/app.asar"
    } else {
        "Contents/Resources/app.asar"
    };
    let path = bundle.join(relative);
    for ancestor in path.ancestors() {
        let meta =
            std::fs::symlink_metadata(ancestor).map_err(|_| "无法确认应用安装文件，请重新检测")?;
        if meta.is_symlink() {
            return Err("应用安装文件不能使用链接".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err("应用安装文件不能使用重解析路径".into());
            }
        }
    }
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x0020_0000) // FILE_FLAG_OPEN_REPARSE_POINT
            .open(&path)
            .map_err(|_| "无法确认应用安装文件，请重新检测")?
    };
    #[cfg(windows)]
    let meta = file
        .metadata()
        .map_err(|_| "无法确认应用安装文件，请重新检测")?;
    #[cfg(not(windows))]
    let meta = std::fs::metadata(&path).map_err(|_| "无法确认应用安装文件，请重新检测")?;
    if !bundle.is_absolute() || !meta.is_file() || meta.len() == 0 {
        return Err("应用安装文件无效，请重新检测".into());
    }
    Ok(Marker {
        path,
        size: meta.len(),
        modified: meta.modified().map_err(|_| "无法确认应用安装时间")?,
        #[cfg(unix)]
        identity: {
            use std::os::unix::fs::MetadataExt;
            // ctime alone can change for quarantine/permissions without any
            // installed bytes changing. It must not complete a same-build install.
            (meta.dev(), meta.ino())
        },
        #[cfg(windows)]
        identity: {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            };
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            // The live handle binds metadata and identity to one ordinary file.
            // NTFS tunneling may preserve creation time across replacement.
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0
                || info.dwFileAttributes & 0x400 != 0
            {
                return Err("无法确认应用安装文件标识，请重新检测".into());
            }
            (
                info.dwVolumeSerialNumber,
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            )
        },
        #[cfg(not(any(unix, windows)))]
        created: meta.created().map_err(|_| "无法确认应用安装时间")?,
    })
}

#[derive(Clone)]
pub(super) struct Target {
    pub install_id: u64,
    pub version: String,
    pub revision: u32,
    pub before: Option<Marker>,
}
impl Target {
    pub fn confirmed(&self, version: &str, revision: u32, current: &Marker) -> bool {
        self.version == version
            && self.revision == revision
            && self
                .before
                .as_ref()
                .is_none_or(|before| before.replaced_by(current))
    }
}

impl Marker {
    fn replaced_by(&self, current: &Self) -> bool {
        if self.path != current.path {
            return false;
        }
        #[cfg(any(unix, windows))]
        {
            self.identity != current.identity
        }
        #[cfg(not(any(unix, windows)))]
        {
            self.created != current.created
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anywhere_install_completion_requires_target_and_replacement_even_for_same_version() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().canonicalize().unwrap();
        let relative = if cfg!(target_os = "windows") {
            "resources"
        } else {
            "Contents/Resources"
        };
        std::fs::create_dir_all(app.join(relative)).unwrap();
        let file = app.join(relative).join("app.asar");
        std::fs::write(&file, b"synthetic").unwrap();
        let before = marker(&app).unwrap();
        let target = Target {
            install_id: 1,
            version: "2.2.1".into(),
            revision: 1,
            before: Some(before.clone()),
        };
        assert!(
            !target.confirmed("2.2.1", 1, &before),
            "opening installer cannot confirm unchanged installation"
        );
        std::fs::write(&file, b"synthetic changed in place").unwrap();
        let same_file = marker(&app).unwrap();
        assert!(
            !target.confirmed("2.2.1", 1, &same_file),
            "modifying the existing file is not proof that the installer replaced it"
        );
        let replacement = file.with_extension("new");
        std::fs::write(&replacement, b"synthetic").unwrap();
        #[cfg(windows)]
        std::fs::remove_file(&file).unwrap();
        std::fs::rename(&replacement, &file).unwrap();
        let after = marker(&app).unwrap();
        assert!(
            target.confirmed("2.2.1", 1, &after),
            "byte-identical same-version replacement has new file identity"
        );
        assert!(!target.confirmed("2.2.1", 0, &after));
        assert!(!target.confirmed("2.2.0", 1, &after));
        let fresh = Target {
            before: None,
            ..target
        };
        assert!(fresh.confirmed("2.2.1", 1, &after));
    }
    #[cfg(unix)]
    #[test]
    fn anywhere_install_completion_rejects_linked_installation_evidence() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().canonicalize().unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        std::fs::write(app.join("synthetic"), b"fixture").unwrap();
        std::os::unix::fs::symlink(
            app.join("synthetic"),
            app.join("Contents/Resources/app.asar"),
        )
        .unwrap();
        assert!(marker(&app).is_err());
    }
}
