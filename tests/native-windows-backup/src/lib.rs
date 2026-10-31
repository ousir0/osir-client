//! Fast native acceptance of the exact production path/ACL code, without Tauri.
#![cfg(windows)]
#![allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
enum BackupError {
    Storage,
    UnsafePath,
}
type Result<T> = std::result::Result<T, BackupError>;
#[path = "../../../src-tauri/src/services/osir/tool_backup_path.rs"]
mod path_safety;
#[path = "../../../src-tauri/src/services/osir/tool_backup_windows.rs"]
mod windows;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("OSIR 原生 backup {} {unique}", std::process::id()));
            fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn absolute_and_verbatim_paths_inspect_root_then_real_ancestors() {
        let fixture = Fixture::new();
        let path = fixture.0.join("missing/settings.json");
        path_safety::safe_path(&path).unwrap();
        let text = path.to_string_lossy();
        assert!(text.starts_with(r"\\?\"));
        let ordinary = PathBuf::from(text.strip_prefix(r"\\?\").unwrap());
        path_safety::safe_path(&ordinary).unwrap();
        assert_eq!(
            path_safety::safe_path(std::path::Path::new(r"C:relative")),
            Err(BackupError::UnsafePath)
        );
        assert_eq!(
            path_safety::safe_path(&PathBuf::from(format!(
                r"{}\..\escape",
                fixture.0.display()
            ))),
            Err(BackupError::UnsafePath)
        );
        fs::write(fixture.0.join("file"), b"synthetic").unwrap();
        assert_eq!(
            path_safety::safe_path(&fixture.0.join("file").join("child")),
            Err(BackupError::UnsafePath)
        );
    }
    #[test]
    fn private_acl_creation_file_inheritance_and_hardlink_rejection() {
        let fixture = Fixture::new();
        let private = fixture.0.join("private");
        windows::create_private_directory(&private).unwrap();
        windows::verify_path(&private).unwrap();
        let payload = private.join("payload.bin");
        fs::write(&payload, b"synthetic backup").unwrap();
        windows::verify_path(&payload).unwrap();
        let mut options = fs::OpenOptions::new();
        options.read(true);
        windows::read_options(&mut options);
        let file = options.open(&payload).unwrap();
        windows::verify_file(&file, true).unwrap();
        drop(file);
        let alias = private.join("hardlink.bin");
        fs::hard_link(&payload, &alias).unwrap();
        let file = options.open(&payload).unwrap();
        assert_eq!(
            windows::verify_file(&file, true),
            Err(BackupError::UnsafePath)
        );
    }
    #[test]
    fn inherited_public_acl_is_refused() {
        let fixture = Fixture::new();
        let public = fixture.0.join("public");
        fs::create_dir(&public).unwrap();
        assert_eq!(windows::verify_path(&public), Err(BackupError::UnsafePath));
    }
}
