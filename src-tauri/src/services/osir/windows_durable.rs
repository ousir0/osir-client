//! Publish a previously flushed same-directory file on Windows.
//! MoveFileEx with WRITE_THROUGH completes the rename before reporting success;
//! Unix directory fsync is not an available Windows persistence primitive.
use std::{io, path::Path};

pub(crate) fn commit(tmp: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    if tmp.parent() != target.parent() || tmp == target {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journal replacement must use a different file in the same directory",
        ));
    }
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
        if path.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid path"));
        }
        path.push(0);
        Ok(path)
    };
    let source = wide(tmp)?;
    let destination = wide(target)?;
    // Both UTF-16 buffers remain alive throughout the call. Cross-volume copy
    // is deliberately disabled: journal publication must be a rename.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
