//! Account-scoped admission journal for cloud key creation, never a replay queue.
//! Only opaque operation/account identifiers and a native completion bit persist.
//! Callers must hold OsirAccountState::connection_operation across every access.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const UNAVAILABLE: &str = "创建记录读取或保存失败，请重试；暂不能创建密钥";
const PENDING: &str = "上次创建结果尚未核对，请先刷新密钥列表并核对结果";
const CHANGED: &str = "创建记录已改变，请重新刷新并核对结果";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u8,
    account_scope: String,
    operation_id: String,
    native_confirmed: bool,
}

pub(crate) struct KeyCreationState {
    path: PathBuf,
    account_scope: String,
}

impl KeyCreationState {
    pub(crate) fn new(directory: &Path, account_id: &str) -> Result<Self, String> {
        if account_id.is_empty() || account_id.len() > 128 {
            return Err(UNAVAILABLE.into());
        }
        let account_scope = format!("{:x}", Sha256::digest(account_id.as_bytes()));
        Ok(Self {
            path: directory.join(format!("osir-key-creation-{account_scope}.json")),
            account_scope,
        })
    }

    pub(crate) fn pending(&self) -> Result<Option<String>, String> {
        Ok(self.read()?.map(|entry| entry.operation_id))
    }

    /// Persist before entering any code that can send a cloud create request.
    /// A crash/cancellation leaves this record locked, irrespective of generation.
    pub(crate) fn begin(&self) -> Result<String, String> {
        if self.read()?.is_some() {
            return Err(PENDING.into());
        }
        let entry = Entry {
            version: 1,
            account_scope: self.account_scope.clone(),
            operation_id: uuid::Uuid::new_v4().to_string(),
            native_confirmed: false,
        };
        self.write(&Some(&entry))?;
        Ok(entry.operation_id)
    }

    /// Retain even confirmed success until the renderer receives the result.
    /// Losing IPC after native completion must also survive a process restart.
    pub(crate) fn confirm_native(&self, operation_id: &str) -> Result<(), String> {
        let mut entry = self.matching(operation_id)?;
        entry.native_confirmed = true;
        self.write(&Some(&entry))
    }

    pub(crate) fn acknowledge(
        &self,
        operation_id: &str,
        received_success: bool,
    ) -> Result<(), String> {
        let entry = self.matching(operation_id)?;
        if received_success && !entry.native_confirmed {
            return Err(CHANGED.into());
        }
        // A tombstone uses the same durable replacement as admission. No TTL,
        // logout cleanup, list-name matching or automatic resend is permitted.
        self.write(&None)
    }

    fn matching(&self, operation_id: &str) -> Result<Entry, String> {
        self.read()?
            .filter(|entry| entry.operation_id == operation_id)
            .ok_or_else(|| CHANGED.into())
    }

    fn reject_non_file(&self) -> Result<(), String> {
        match fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.is_file() => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(UNAVAILABLE.into()),
        }
    }

    fn read(&self) -> Result<Option<Entry>, String> {
        self.reject_non_file()?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(UNAVAILABLE.into()),
        };
        if !file.metadata().map_err(|_| UNAVAILABLE)?.is_file() {
            return Err(UNAVAILABLE.into());
        }
        let mut bytes = Vec::new();
        file.take(2049)
            .read_to_end(&mut bytes)
            .map_err(|_| UNAVAILABLE)?;
        if bytes.len() > 2048 {
            return Err(UNAVAILABLE.into());
        }
        let entry: Option<Entry> = serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)?;
        if let Some(entry) = &entry {
            if entry.version != 1
                || entry.account_scope != self.account_scope
                || uuid::Uuid::parse_str(&entry.operation_id).is_err()
            {
                return Err(UNAVAILABLE.into());
            }
        }
        Ok(entry)
    }

    fn write(&self, entry: &Option<&Entry>) -> Result<(), String> {
        self.reject_non_file()?;
        let bytes = serde_json::to_vec(entry).map_err(|_| UNAVAILABLE)?;
        let staged = crate::config::stage_write(&self.path, &bytes, Some(0o600), true)
            .map_err(|_| UNAVAILABLE)?;
        #[cfg(windows)]
        if super::windows_durable::commit(staged.tmp_path(), &self.path).is_err() {
            let _ = fs::remove_file(staged.tmp_path());
            return Err(UNAVAILABLE.into());
        }
        #[cfg(not(windows))]
        staged.commit().map_err(|_| UNAVAILABLE)?;
        #[cfg(unix)]
        fs::File::open(self.path.parent().ok_or(UNAVAILABLE)?)
            .and_then(|file| file.sync_all())
            .map_err(|_| UNAVAILABLE)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_and_logout_never_release_admission_and_accounts_are_isolated() {
        let directory = tempfile::tempdir().unwrap();
        let a = KeyCreationState::new(directory.path(), "synthetic-account-a").unwrap();
        let operation = a.begin().unwrap();
        let restarted = KeyCreationState::new(directory.path(), "synthetic-account-a").unwrap();
        assert_eq!(restarted.pending().unwrap(), Some(operation.clone()));
        assert_eq!(restarted.begin().unwrap_err(), PENDING);
        let b = KeyCreationState::new(directory.path(), "synthetic-account-b").unwrap();
        assert!(b.pending().unwrap().is_none());
        let other = b.begin().unwrap();
        assert!(b.acknowledge(&operation, false).is_err());
        b.acknowledge(&other, false).unwrap();
        assert_eq!(a.pending().unwrap(), Some(operation));
    }

    #[test]
    fn explicit_review_unlocks_without_post_and_late_completion_cannot_clear_new_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let journal = KeyCreationState::new(directory.path(), "fixture").unwrap();
        let first = journal.begin().unwrap();
        assert!(journal.acknowledge(&first, true).is_err());
        journal.acknowledge(&first, false).unwrap();
        let next = journal.begin().unwrap();
        assert!(journal.confirm_native(&first).is_err());
        assert!(journal.acknowledge(&first, false).is_err());
        assert_eq!(journal.pending().unwrap(), Some(next));
    }

    #[test]
    fn native_success_stays_locked_until_ipc_receipt_and_disk_has_only_allowlisted_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let journal = KeyCreationState::new(directory.path(), "synthetic-account-private").unwrap();
        let operation = journal.begin().unwrap();
        journal.confirm_native(&operation).unwrap();
        let restarted =
            KeyCreationState::new(directory.path(), "synthetic-account-private").unwrap();
        assert!(restarted.begin().is_err());
        let bytes = fs::read(&journal.path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 4);
        assert!(!String::from_utf8(bytes)
            .unwrap()
            .contains("synthetic-account-private"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&journal.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        restarted.acknowledge(&operation, true).unwrap();
        assert!(journal.pending().unwrap().is_none());
    }

    #[test]
    fn corrupt_oversized_or_wrong_account_records_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let journal = KeyCreationState::new(directory.path(), "fixture").unwrap();
        for bytes in [b"{".to_vec(), vec![b' '; 2049]] {
            fs::write(&journal.path, bytes).unwrap();
            assert!(journal.pending().is_err());
            assert!(journal.begin().is_err());
        }
        fs::remove_file(&journal.path).unwrap();
        journal.begin().unwrap();
        let b = KeyCreationState::new(directory.path(), "other-fixture").unwrap();
        fs::copy(&journal.path, &b.path).unwrap();
        assert!(b.begin().is_err());
    }

    #[test]
    fn persistence_failure_prevents_the_caller_from_sending() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("file");
        fs::write(&blocked, b"fixture").unwrap();
        let journal = KeyCreationState::new(&blocked, "fixture").unwrap();
        let mut posts = 0;
        if journal.begin().is_ok() {
            posts += 1;
        }
        assert_eq!(posts, 0);
    }
}
