//! Inspect real ancestors; a Windows drive/share prefix alone is not a path.
use super::{BackupError, Result};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub(super) fn safe_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(BackupError::UnsafePath);
    }
    let mut cursor = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(BackupError::UnsafePath);
        }
        cursor.push(component);
        // In particular \\?\C: is invalid until its root separator is added;
        // C: instead means the drive's current directory. Inspect neither.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&cursor) {
            #[cfg(windows)]
            Ok(metadata) if super::windows::is_reparse(&metadata) => {
                return Err(BackupError::UnsafePath);
            }
            Ok(metadata)
                if metadata.file_type().is_symlink() || (cursor != path && !metadata.is_dir()) =>
            {
                return Err(BackupError::UnsafePath);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(BackupError::Storage),
        }
    }
    Ok(())
}
