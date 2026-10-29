//! Windows Credential Manager. No password UI, plaintext file or renderer IPC.
use super::CredentialError;
use sha2::{Digest, Sha256};

const MAX_BLOB: usize = 2560;

fn target(service: &str, account: &str) -> Result<String, CredentialError> {
    if service.is_empty() || account.is_empty() || service.len() > 1024 || account.len() > 4096 {
        return Err(CredentialError::InvalidFormat);
    }
    let tuple =
        serde_json::to_vec(&(service, account)).map_err(|_| CredentialError::InvalidFormat)?;
    Ok(format!("OSIR:{:x}", Sha256::digest(tuple)))
}

fn validate_payload(bytes: &[u8]) -> Result<(), CredentialError> {
    if bytes.is_empty() || bytes.len() > MAX_BLOB {
        return Err(CredentialError::InvalidFormat);
    }
    Ok(())
}

/// All OSIR writers of one target serialize, so an add-only request cannot
/// overwrite another process's fresh item between the existence check and write.
fn write_once(
    bytes: &[u8],
    create: bool,
    exists: impl FnOnce() -> Result<bool, CredentialError>,
    save: impl FnOnce() -> Result<(), CredentialError>,
) -> Result<(), CredentialError> {
    validate_payload(bytes)?;
    if create && exists()? {
        return Err(CredentialError::Storage);
    }
    save()
}

#[cfg(target_os = "windows")]
mod native {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, ERROR_NOT_FOUND, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
        },
        Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
            CRED_TYPE_GENERIC,
        },
        System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    struct TargetLock(HANDLE);
    impl Drop for TargetLock {
        fn drop(&mut self) {
            unsafe {
                ReleaseMutex(self.0);
                CloseHandle(self.0);
            }
        }
    }
    fn lock(name: &str) -> Result<TargetLock, CredentialError> {
        let name = wide(&format!(
            "Local\\OSIR-Credential-{}",
            name.trim_start_matches("OSIR:")
        ));
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(CredentialError::Unavailable);
        }
        match unsafe { WaitForSingleObject(handle, 1500) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(TargetLock(handle)),
            _ => {
                unsafe {
                    CloseHandle(handle);
                }
                Err(CredentialError::Unavailable)
            }
        }
    }

    struct Credential(*mut CREDENTIALW);
    impl Drop for Credential {
        fn drop(&mut self) {
            unsafe {
                CredFree(self.0.cast());
            }
        }
    }
    fn lookup(name: &[u16]) -> Result<Option<Credential>, CredentialError> {
        let mut pointer = std::ptr::null_mut();
        if unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) } == 0 {
            return if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(CredentialError::Storage)
            };
        }
        if pointer.is_null() {
            return Err(CredentialError::Storage);
        }
        Ok(Some(Credential(pointer)))
    }

    pub(crate) fn write(
        service: &str,
        account: &str,
        bytes: &[u8],
        create: bool,
    ) -> Result<(), CredentialError> {
        let name = target(service, account)?;
        let _lock = lock(&name)?;
        let name = wide(&name);
        write_once(
            bytes,
            create,
            || Ok(lookup(&name)?.is_some()),
            || {
                let mut user = wide("OSIR");
                let mut record = CREDENTIALW::default();
                record.Type = CRED_TYPE_GENERIC;
                record.TargetName = name.as_ptr().cast_mut();
                record.CredentialBlobSize = bytes.len() as u32;
                record.CredentialBlob = bytes.as_ptr().cast_mut();
                record.Persist = CRED_PERSIST_LOCAL_MACHINE;
                record.UserName = user.as_mut_ptr();
                if unsafe { CredWriteW(&record, 0) } != 0 {
                    Ok(())
                } else {
                    Err(CredentialError::Storage)
                }
            },
        )
    }
    pub(crate) fn read(service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
        let name = target(service, account)?;
        let _lock = lock(&name)?;
        let name = wide(&name);
        let Some(credential) = lookup(&name)? else {
            return Ok(None);
        };
        let record = unsafe { &*credential.0 };
        if record.CredentialBlobSize == 0
            || record.CredentialBlobSize as usize > MAX_BLOB
            || record.CredentialBlob.is_null()
        {
            return Err(CredentialError::InvalidFormat);
        }
        Ok(Some(
            unsafe {
                std::slice::from_raw_parts(
                    record.CredentialBlob,
                    record.CredentialBlobSize as usize,
                )
            }
            .to_vec(),
        ))
    }
    pub(crate) fn remove(service: &str, account: &str) -> Result<(), CredentialError> {
        let name = target(service, account)?;
        let _lock = lock(&name)?;
        let name = wide(&name);
        if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0
            || unsafe { GetLastError() } == ERROR_NOT_FOUND
        {
            Ok(())
        } else {
            Err(CredentialError::Storage)
        }
    }
}

#[cfg(target_os = "windows")]
pub(super) use native::{read, remove, write};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_namespace_is_tuple_bound_and_hides_account_labels() {
        let beta = target("com.osir.client.beta.release", "fixture-account").unwrap();
        assert_ne!(
            beta,
            target("com.osir.client.release", "fixture-account").unwrap()
        );
        assert_ne!(target("a", "bc").unwrap(), target("ab", "c").unwrap());
        assert_ne!(
            beta,
            target("com.osir.client.beta.release", "fixture-account-new-slot").unwrap()
        );
        assert!(!beta.contains("fixture-account"));
        assert_eq!(beta.len(), 69);
    }
    #[test]
    fn add_only_does_not_overwrite_existing_item_or_fallback_after_save_failure() {
        assert_eq!(
            write_once(
                b"fixture",
                true,
                || Ok(true),
                || panic!("must not overwrite")
            ),
            Err(CredentialError::Storage)
        );
        assert_eq!(
            write_once(
                b"fixture",
                true,
                || Ok(false),
                || Err(CredentialError::Storage)
            ),
            Err(CredentialError::Storage)
        );
        assert!(write_once(
            b"fixture",
            false,
            || panic!("update needs no read"),
            || Ok(())
        )
        .is_ok());
    }
    #[test]
    fn unsupported_blob_size_fails_before_any_os_access() {
        for bytes in [Vec::new(), vec![0; MAX_BLOB + 1]] {
            assert_eq!(
                write_once(
                    &bytes,
                    true,
                    || panic!("must not read"),
                    || panic!("must not write")
                ),
                Err(CredentialError::InvalidFormat)
            );
        }
        assert!(write_once(&vec![0; MAX_BLOB], true, || Ok(false), || Ok(())).is_ok());
    }
}
