//! Windows snapshot protection: reject reparse/hard links and require a DACL
//! granting access only to the current owner and SYSTEM. No shell or elevation.
use super::{BackupError, Result};
use std::{
    fs::{File, OpenOptions},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
        *,
    },
    Storage::FileSystem::{
        CreateDirectoryW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub(super) fn is_reparse(meta: &std::fs::Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub(super) fn read_options(options: &mut OpenOptions) {
    options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ);
}

pub(super) fn verify_file(file: &File, private: bool) -> Result<()> {
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || info.nNumberOfLinks != 1
        {
            return Err(BackupError::UnsafePath);
        }
    }
    if private {
        verify_acl(file)?;
    }
    Ok(())
}

fn open_metadata(path: &Path) -> Result<File> {
    OpenOptions::new()
        .access_mode(0x0002_0080) // READ_CONTROL | FILE_READ_ATTRIBUTES
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| BackupError::Storage)
}

pub(super) fn verify_path(path: &Path) -> Result<()> {
    let file = open_metadata(path)?;
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(BackupError::UnsafePath);
        }
    }
    verify_acl(&file)
}

// Set owner and protected ACL atomically, including when an elevated account's
// default owner would otherwise be Administrators. Children inherit private ACEs.
pub(super) fn create_private_directory(path: &Path) -> std::io::Result<()> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let user =
            current_user().map_err(|_| std::io::Error::other("Cannot inspect backup owner"))?;
        let mut sid_text = ptr::null_mut();
        if ConvertSidToStringSidW(
            (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid,
            &mut sid_text,
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut len = 0;
        while *sid_text.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_text, len));
        LocalFree(sid_text.cast());
        let sddl: Vec<u16> = format!("O:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})\0")
            .encode_utf16()
            .collect();
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let ok = CreateDirectoryW(path.as_ptr(), &attrs);
        let error = std::io::Error::last_os_error();
        LocalFree(descriptor);
        if ok == 0 {
            return Err(error);
        }
    }
    Ok(())
}

fn current_user() -> Result<Vec<usize>> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(BackupError::Storage);
        }
        let mut size = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
        if size == 0 || size > 65536 {
            CloseHandle(token);
            return Err(BackupError::Storage);
        }
        // usize-aligned storage for TOKEN_USER and its trailing SID.
        let mut buffer = vec![
            0usize;
            (size as usize + std::mem::size_of::<usize>() - 1)
                / std::mem::size_of::<usize>()
        ];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(BackupError::Storage);
        }
        Ok(buffer)
    }
}

fn verify_acl(file: &File) -> Result<()> {
    unsafe {
        let user_buffer = current_user()?;
        let user = (*(user_buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid;
        let mut descriptor = ptr::null_mut();
        let mut owner = ptr::null_mut();
        let mut acl = ptr::null_mut();
        if GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        ) != 0
        {
            return Err(BackupError::Storage);
        }
        let result = (|| {
            let owner_is_user = !owner.is_null() && EqualSid(owner, user) != 0;
            // Elevated Windows processes can create files owned by the built-in
            // Administrators SID. This does not grant data access: the DACL
            // below must still allow only the current user and SYSTEM, and an
            // OWNER RIGHTS grant is accepted only for a user-owned object.
            let trusted_owner = owner_is_user
                || (!owner.is_null()
                    && (IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
                        || IsWellKnownSid(owner, WinLocalSystemSid) != 0));
            if !trusted_owner || acl.is_null() {
                return Err(BackupError::UnsafePath);
            }
            let mut allowed_owner = false;
            for index in 0..(*acl).AceCount as u32 {
                let mut ace = ptr::null_mut();
                if GetAce(acl, index, &mut ace) == 0 || ace.is_null() {
                    return Err(BackupError::UnsafePath);
                }
                let header = &*ace.cast::<ACE_HEADER>();
                // Explicit deny entries cannot broaden access; unsupported allow
                // types are rejected rather than trying to interpret object ACEs.
                if header.AceType == 1 {
                    continue;
                } // ACCESS_DENIED_ACE_TYPE
                if header.AceType != 0 {
                    return Err(BackupError::UnsafePath);
                }
                let ace = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                let sid = ptr::addr_of!(ace.SidStart).cast_mut().cast();
                let own = EqualSid(sid, user) != 0
                    || (owner_is_user && IsWellKnownSid(sid, WinCreatorOwnerRightsSid) != 0);
                if !own && IsWellKnownSid(sid, WinLocalSystemSid) == 0 {
                    return Err(BackupError::UnsafePath);
                }
                allowed_owner |= own && ace.Mask != 0;
            }
            if !allowed_owner {
                return Err(BackupError::UnsafePath);
            }
            Ok(())
        })();
        LocalFree(descriptor);
        result
    }
}
