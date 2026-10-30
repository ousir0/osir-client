//! Crash recovery metadata for managed model credentials. No secret is stored.
use crate::services::osir::credentials::CredentialBackend;
use crate::services::osir::key_vault::{KeyBinding, KeyMetadata, KeyReference, KeyVault};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Intent,
    Stored,
    Committed,
    CleanupPending,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u8,
    phase: Phase,
    reference: KeyReference,
    binding: KeyBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<PreviousGrant>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PreviousGrant {
    reference: KeyReference,
    binding: KeyBinding,
}

const MAX_JOURNAL_BYTES: usize = 32 * 1024;

pub(crate) struct Journal {
    path: PathBuf,
}
// Serializes startup recovery and the journal/store/DB commit sequence.
pub(crate) fn lock() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    crate::services::osir::key_vault::recovery_lock()
}

// Strict raw metadata scan: the normal provider reader defaults malformed meta
// to empty, which is not proof that a vault reference is unused.
fn referenced(
    conn: &rusqlite::Connection,
    reference: &KeyReference,
) -> Result<Option<bool>, String> {
    let mut stmt = conn
        .prepare("SELECT meta FROM providers")
        .map_err(|_| "恢复数据库读取失败")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| "恢复数据库读取失败")?;
    let mut unknown = false;
    for row in rows {
        let value = row
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let Some(value) = value.filter(|v| v.is_object()) else {
            unknown = true;
            continue;
        };
        match value.get("osirBinding") {
            None | Some(serde_json::Value::Null) => (),
            Some(binding) => match binding
                .get("reference")
                .cloned()
                .and_then(|v| serde_json::from_value::<KeyReference>(v).ok())
            {
                Some(found) if &found == reference => return Ok(Some(true)),
                Some(_) => (),
                None => unknown = true,
            },
        }
    }
    Ok(if unknown { None } else { Some(false) })
}

pub(crate) fn recover_database<B: CredentialBackend>(
    journal: &Journal,
    db: &crate::database::Database,
    vault: &KeyVault<B>,
) -> Result<(), String> {
    // Keep the DB lock through deletion; a concurrent commit cannot introduce
    // a reference after the absence check and before vault cleanup.
    let conn = db.conn.lock().map_err(|_| "恢复数据库不可用")?;
    if let Some(entry) = journal.read_entry()? {
        let tool = match entry.binding.tool {
            crate::services::osir::key_vault::KeyTool::Codex => "codex",
            crate::services::osir::key_vault::KeyTool::ClaudeCode => "claude",
            crate::services::osir::key_vault::KeyTool::ClaudeDesktop => "claude-desktop",
            crate::services::osir::key_vault::KeyTool::OpenCode => "opencode",
            crate::services::osir::key_vault::KeyTool::GeminiCli => "gemini",
        };
        if entry.previous.is_some() && crate::mode::operation::has_pending(tool) {
            return Err("工具配置恢复尚未完成，已保留新旧凭据".into());
        }
    }
    journal.recover(vault, |_, reference| referenced(&conn, reference))
}
impl Journal {
    pub(crate) fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("osir-managed-key-recovery.json"),
        }
    }
    pub(crate) fn ensure_clear(&self) -> Result<(), String> {
        match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err("先前凭据恢复尚未完成，请勿重复保存".into()),
        }
    }
    pub(crate) fn record(&self, metadata: &KeyMetadata, phase: Phase) -> Result<(), String> {
        self.record_with_previous(metadata, None, phase)
    }
    pub(crate) fn record_replacement(
        &self,
        metadata: &KeyMetadata,
        previous: &crate::provider::ManagedBinding,
        phase: Phase,
    ) -> Result<(), String> {
        if metadata.reference == previous.reference
            || metadata.binding.provider_id != previous.binding.provider_id
            || metadata.binding.tool != previous.binding.tool
        {
            return Err("换钥恢复绑定无效".into());
        }
        self.record_with_previous(
            metadata,
            Some(PreviousGrant {
                reference: previous.reference.clone(),
                binding: previous.binding.clone(),
            }),
            phase,
        )
    }
    fn record_with_previous(
        &self,
        metadata: &KeyMetadata,
        previous: Option<PreviousGrant>,
        phase: Phase,
    ) -> Result<(), String> {
        let entry = Entry {
            version: if previous.is_some() { 2 } else { 1 },
            phase,
            reference: metadata.reference.clone(),
            binding: metadata.binding.clone(),
            previous,
        };
        if let Some(pending) = self.read_entry()? {
            if pending.reference != entry.reference
                || pending.binding != entry.binding
                || pending.previous != entry.previous
            {
                return Err("先前凭据恢复尚未完成，禁止覆盖恢复日志".into());
            }
        }
        self.write_entry(&entry)
    }
    fn write_entry(&self, entry: &Entry) -> Result<(), String> {
        let dir = self.path.parent().ok_or("恢复日志路径无效")?;
        fs::create_dir_all(dir).map_err(|_| "恢复日志不可用")?;
        self.reject_symlink()?;
        let bytes = serde_json::to_vec(entry).map_err(|_| "恢复日志不可用")?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err("恢复日志超出限制".into());
        }
        crate::config::stage_write(&self.path, &bytes, Some(0o600), true)
            .and_then(|staged| staged.commit())
            .map_err(|_| "恢复日志不可用")?;
        self.sync_directory()
    }
    fn reject_symlink(&self) -> Result<(), String> {
        match fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.file_type().is_file() => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err("恢复日志文件类型无效".into()),
        }
    }
    fn sync_directory(&self) -> Result<(), String> {
        let directory = self.path.parent().ok_or("恢复日志路径无效")?;
        #[cfg(unix)]
        {
            fs::File::open(directory)
                .and_then(|file| file.sync_all())
                .map_err(|_| "恢复日志同步失败".to_string())
        }
        // Fail closed on platforms without a supported directory fsync. A
        // successful rename alone must not be advertised as durable recovery.
        #[cfg(not(unix))]
        {
            let _ = directory;
            Err("此平台不支持恢复日志目录同步".into())
        }
    }

    pub(crate) fn clear(&self) -> Result<(), String> {
        self.reject_symlink()?;
        match fs::remove_file(&self.path) {
            Ok(()) => self.sync_directory(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("恢复日志清理失败".into()),
        }
    }
    fn read_entry(&self) -> Result<Option<Entry>, String> {
        self.reject_symlink()?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("恢复日志读取失败".into()),
        };
        let meta = file.metadata().map_err(|_| "恢复日志读取失败")?;
        if !meta.is_file() || meta.len() > MAX_JOURNAL_BYTES as u64 {
            return Err("恢复日志超出限制或类型无效".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "恢复日志读取失败")?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err("恢复日志超出限制".into());
        }
        let entry: Entry = serde_json::from_slice(&bytes).map_err(|_| "恢复日志格式无效")?;
        if !matches!(
            (entry.version, entry.previous.is_some()),
            (1, false) | (2, true)
        ) {
            return Err("恢复日志版本无效".into());
        }
        Ok(Some(entry))
    }
    pub(crate) fn recover<B: CredentialBackend>(
        &self,
        vault: &KeyVault<B>,
        mut row_references: impl FnMut(&KeyBinding, &KeyReference) -> Result<Option<bool>, String>,
    ) -> Result<(), String> {
        let Some(mut entry) = self.read_entry()? else {
            return Ok(());
        };
        if let Some(previous) = &entry.previous {
            let new_used = row_references(&entry.binding, &entry.reference)?;
            let old_used = row_references(&previous.binding, &previous.reference)?;
            match (new_used, old_used) {
                (Some(true), Some(false)) if entry.phase == Phase::Committed => {
                    vault
                        .delete(&previous.reference, &previous.binding)
                        .map_err(|_| "旧凭据清理未完成，已保留恢复日志")?;
                    return self.clear();
                }
                (Some(false), Some(_)) => {
                    // Confirmed new-reference absence permits abort cleanup only.
                    entry.phase = Phase::CleanupPending;
                    self.write_entry(&entry)?;
                    vault
                        .delete(&entry.reference, &entry.binding)
                        .map_err(|_| "新凭据清理未完成，已保留恢复日志")?;
                    return self.clear();
                }
                _ => return Err("换钥提交或配置状态未确认，已保留新旧凭据与恢复日志".into()),
            }
        }
        match row_references(&entry.binding, &entry.reference)? {
            Some(true) => self.clear()?,
            Some(false) => {
                // Persist retry intent before touching the credential store.
                let metadata = KeyMetadata {
                    reference: entry.reference.clone(),
                    binding: entry.binding.clone(),
                    masked_key: String::new(),
                };
                self.record(&metadata, Phase::CleanupPending)?;
                vault
                    .delete(&entry.reference, &entry.binding)
                    .map_err(|_| "孤立凭据清理失败")?;
                self.clear()?;
            }
            None => return Ok(()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::osir::{
        credentials::CredentialError,
        key_vault::{KeyTool, NativeKeySecret, GATEWAY_ORIGIN},
    };
    use std::collections::HashMap;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    };
    #[derive(Clone, Default)]
    struct Memory {
        items: Arc<Mutex<HashMap<(String, String), Vec<u8>>>>,
        deletes: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
    }
    impl CredentialBackend for Memory {
        fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            self.items
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), bytes.to_vec());
            Ok(())
        }
        fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned())
        }
        fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(CredentialError::Storage);
            }
            self.items
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }
    fn fixture() -> (
        tempfile::TempDir,
        Journal,
        KeyVault<Memory>,
        KeyMetadata,
        Memory,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path());
        let memory = Memory::default();
        let vault = KeyVault::with_backend(
            "https://osirclaw.com",
            "com.osir.recovery.fixture",
            memory.clone(),
        )
        .unwrap();
        let binding = KeyBinding {
            account_origin: "https://osirclaw.com".into(),
            account_id: "fixture-account".into(),
            key_id: 1,
            group_id: Some(2),
            tool: KeyTool::Codex,
            provider_id: "fixture-provider".into(),
            gateway_origin: GATEWAY_ORIGIN.into(),
        };
        let metadata = vault
            .store(
                &binding,
                &NativeKeySecret::new("fixture-recovery-secret".into()).unwrap(),
            )
            .unwrap();
        journal.record(&metadata, Phase::Stored).unwrap();
        (dir, journal, vault, metadata, memory)
    }
    fn replacement_fixture() -> (
        tempfile::TempDir,
        Journal,
        KeyVault<Memory>,
        KeyMetadata,
        crate::provider::ManagedBinding,
        Memory,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path());
        let memory = Memory::default();
        let vault = KeyVault::with_backend(
            "https://osirclaw.com",
            "com.osir.recovery.fixture",
            memory.clone(),
        )
        .unwrap();
        let old_binding = KeyBinding {
            account_origin: "https://osirclaw.com".into(),
            account_id: "fixture-account".into(),
            key_id: 1,
            group_id: Some(2),
            tool: KeyTool::Codex,
            provider_id: "fixture-provider".into(),
            gateway_origin: GATEWAY_ORIGIN.into(),
        };
        let old = vault
            .store(
                &old_binding,
                &NativeKeySecret::new("fixture-old-replacement-secret".into()).unwrap(),
            )
            .unwrap();
        let mut new_binding = old_binding.clone();
        new_binding.account_id = "replacement-account".into();
        new_binding.key_id = 2;
        new_binding.group_id = Some(4);
        let new = vault
            .store(
                &new_binding,
                &NativeKeySecret::new("fixture-new-replacement-secret".into()).unwrap(),
            )
            .unwrap();
        let previous = crate::provider::ManagedBinding {
            source: crate::provider::AuthBindingSource::ManagedAccount,
            binding: old.binding,
            reference: old.reference,
            masked_key: old.masked_key,
        };
        (dir, journal, vault, new, previous, memory)
    }

    #[test]
    fn replacement_log_cannot_overwrite_an_unrelated_pending_operation() {
        let (_dir, journal, _vault, new, old, _) = replacement_fixture();
        journal
            .record_replacement(&new, &old, Phase::Intent)
            .unwrap();
        let before = fs::read(&journal.path).unwrap();
        assert!(journal
            .record_replacement(&new, &old, Phase::Committed)
            .is_ok());
        let committed = fs::read(&journal.path).unwrap();
        let mut other = new.clone();
        other.reference = serde_json::from_value(serde_json::json!({
            "version": 1, "id": "550e8400-e29b-41d4-a716-446655440001"
        }))
        .unwrap();
        assert!(journal
            .record_replacement(&other, &old, Phase::Intent)
            .is_err());
        assert_eq!(fs::read(&journal.path).unwrap(), committed);
        let mut other_binding = new.clone();
        other_binding.binding.account_id = "other-account".into();
        let mut other_old_reference = old.clone();
        other_old_reference.reference = other.reference.clone();
        let mut other_old_binding = old.clone();
        other_old_binding.binding.group_id = Some(99);
        for (next, previous) in [
            (other_binding, old.clone()),
            (new.clone(), other_old_reference),
            (new.clone(), other_old_binding),
        ] {
            assert!(journal
                .record_replacement(&next, &previous, Phase::Intent)
                .is_err());
            assert_eq!(fs::read(&journal.path).unwrap(), committed);
        }
        assert!(journal.record(&new, Phase::Intent).is_err());
        assert_eq!(fs::read(&journal.path).unwrap(), committed);
        assert_ne!(before, committed);
        let parsed: Entry = serde_json::from_slice(&committed).unwrap();
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.previous.as_ref().unwrap().reference, old.reference);
    }

    #[test]
    fn failed_replacement_cleans_only_confirmed_unused_new_reference() {
        for old_used in [false, true] {
            let (_dir, journal, vault, new, old, memory) = replacement_fixture();
            journal
                .record_replacement(&new, &old, Phase::Stored)
                .unwrap();
            journal
                .recover(&vault, |binding, reference| {
                    if reference == &new.reference {
                        Ok(Some(false))
                    } else if reference == &old.reference && binding == &old.binding {
                        Ok(Some(old_used))
                    } else {
                        panic!("unexpected journal reference")
                    }
                })
                .unwrap();
            assert!(vault.load(&new.reference, &new.binding).unwrap().is_none());
            assert!(vault.load(&old.reference, &old.binding).unwrap().is_some());
            assert_eq!(memory.deletes.load(Ordering::SeqCst), 1);
            journal.ensure_clear().unwrap();
        }
    }

    #[test]
    fn committed_replacement_cleans_old_only_after_database_confirms_new_reference() {
        let (_dir, journal, vault, new, old, memory) = replacement_fixture();
        journal
            .record_replacement(&new, &old, Phase::Committed)
            .unwrap();
        let mut reads = Vec::new();
        journal
            .recover(&vault, |binding, reference| {
                reads.push(reference.clone());
                if reference == &new.reference {
                    Ok(Some(true))
                } else if reference == &old.reference && binding == &old.binding {
                    Ok(Some(false))
                } else {
                    panic!("unexpected journal reference")
                }
            })
            .unwrap();
        assert_eq!(reads, vec![new.reference.clone(), old.reference.clone()]);
        assert!(vault.load(&new.reference, &new.binding).unwrap().is_some());
        assert!(vault.load(&old.reference, &old.binding).unwrap().is_none());
        assert_eq!(memory.deletes.load(Ordering::SeqCst), 1);
        journal.ensure_clear().unwrap();
    }

    #[test]
    fn uncommitted_replacement_with_new_database_reference_retains_both_grants_and_log() {
        // A failed DB rollback can leave DB=new and live=old without a pending
        // projection. Database reference alone does not prove a completed commit.
        for phase in [Phase::Intent, Phase::Stored, Phase::CleanupPending] {
            let (_dir, journal, vault, new, old, memory) = replacement_fixture();
            journal.record_replacement(&new, &old, phase).unwrap();
            let before = fs::read(&journal.path).unwrap();
            let mut reads = Vec::new();
            assert!(journal
                .recover(&vault, |binding, reference| {
                    reads.push(reference.clone());
                    if reference == &new.reference && binding == &new.binding {
                        Ok(Some(true))
                    } else if reference == &old.reference && binding == &old.binding {
                        Ok(Some(false))
                    } else {
                        panic!("unexpected journal reference")
                    }
                })
                .is_err());
            assert_eq!(reads, vec![new.reference.clone(), old.reference.clone()]);
            assert_eq!(fs::read(&journal.path).unwrap(), before);
            assert!(vault.load(&new.reference, &new.binding).unwrap().is_some());
            assert!(vault.load(&old.reference, &old.binding).unwrap().is_some());
            assert_eq!(memory.deletes.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn replacement_cleanup_failure_retains_grants_and_retry_deletes_only_the_unused_reference() {
        for committed in [false, true] {
            let (_dir, journal, vault, new, old, memory) = replacement_fixture();
            let phase = if committed {
                Phase::Committed
            } else {
                Phase::Stored
            };
            journal.record_replacement(&new, &old, phase).unwrap();
            let row_references = |binding: &KeyBinding, reference: &KeyReference| {
                if reference == &new.reference && binding == &new.binding {
                    Ok(Some(committed))
                } else if reference == &old.reference && binding == &old.binding {
                    Ok(Some(!committed))
                } else {
                    panic!("unexpected journal reference")
                }
            };
            memory.fail.store(true, Ordering::SeqCst);
            assert!(journal.recover(&vault, row_references).is_err());
            let entry = journal.read_entry().unwrap().unwrap();
            assert_eq!(
                entry.phase,
                if committed {
                    Phase::Committed
                } else {
                    Phase::CleanupPending
                }
            );
            assert_eq!(entry.reference, new.reference);
            assert_eq!(entry.previous.unwrap().reference, old.reference);
            assert!(vault.load(&new.reference, &new.binding).unwrap().is_some());
            assert!(vault.load(&old.reference, &old.binding).unwrap().is_some());
            assert_eq!(memory.deletes.load(Ordering::SeqCst), 1);
            let log = fs::read_to_string(&journal.path).unwrap();
            for secret in [
                "fixture-old-replacement-secret",
                "fixture-new-replacement-secret",
            ] {
                assert!(!log.contains(secret));
            }
            memory.fail.store(false, Ordering::SeqCst);
            journal.recover(&vault, row_references).unwrap();
            assert_eq!(
                vault.load(&new.reference, &new.binding).unwrap().is_some(),
                committed
            );
            assert_eq!(
                vault.load(&old.reference, &old.binding).unwrap().is_some(),
                !committed
            );
            journal.ensure_clear().unwrap();
            journal
                .recover(&vault, |_, _| panic!("cleared journal"))
                .unwrap();
            assert_eq!(memory.deletes.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn replacement_database_read_failure_retains_both_grants_and_log() {
        for phase in [Phase::Stored, Phase::Committed] {
            for fail_new_read in [false, true] {
                let (_dir, journal, vault, new, old, memory) = replacement_fixture();
                journal.record_replacement(&new, &old, phase).unwrap();
                let before = fs::read(&journal.path).unwrap();
                assert!(journal
                    .recover(&vault, |binding, reference| {
                        if reference == &new.reference && binding == &new.binding {
                            if fail_new_read {
                                Err("database unavailable".into())
                            } else {
                                Ok(Some(phase == Phase::Committed))
                            }
                        } else if reference == &old.reference && binding == &old.binding {
                            Err("database unavailable".into())
                        } else {
                            panic!("unexpected journal reference")
                        }
                    })
                    .is_err());
                assert_eq!(fs::read(&journal.path).unwrap(), before);
                assert!(vault.load(&new.reference, &new.binding).unwrap().is_some());
                assert!(vault.load(&old.reference, &old.binding).unwrap().is_some());
                assert_eq!(memory.deletes.load(Ordering::SeqCst), 0);
            }
        }
    }

    #[test]
    fn replacement_unknown_or_inconsistent_database_state_retains_both_grants_and_log() {
        for phase in [
            Phase::Intent,
            Phase::Stored,
            Phase::Committed,
            Phase::CleanupPending,
        ] {
            for (new_state, old_state) in [
                (None, Some(false)),
                (Some(true), None),
                (Some(false), None),
                (None, Some(true)),
                (None, None),
                (Some(true), Some(true)),
            ] {
                let (_dir, journal, vault, new, old, memory) = replacement_fixture();
                journal.record_replacement(&new, &old, phase).unwrap();
                let before = fs::read(&journal.path).unwrap();
                let mut calls = 0;
                let result = journal.recover(&vault, |_, reference| {
                    calls += 1;
                    if reference == &new.reference {
                        Ok(new_state)
                    } else {
                        Ok(old_state)
                    }
                });
                assert!(result.is_err());
                assert_eq!(calls, 2);
                assert_eq!(fs::read(&journal.path).unwrap(), before);
                assert!(vault.load(&new.reference, &new.binding).unwrap().is_some());
                assert!(vault.load(&old.reference, &old.binding).unwrap().is_some());
                assert_eq!(memory.deletes.load(Ordering::SeqCst), 0);
            }
        }
    }
    #[test]
    fn durable_intent_recovers_crashes_before_and_after_vault_write() {
        for write_vault in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let journal = Journal::new(dir.path());
            let memory = Memory::default();
            let vault = KeyVault::with_backend(
                "https://osirclaw.com",
                "com.osir.intent.fixture",
                memory.clone(),
            )
            .unwrap();
            let binding = KeyBinding {
                account_origin: "https://osirclaw.com".into(),
                account_id: "fixture-account".into(),
                key_id: 1,
                group_id: None,
                tool: KeyTool::Codex,
                provider_id: "fixture".into(),
                gateway_origin: GATEWAY_ORIGIN.into(),
            };
            let secret = NativeKeySecret::new("fixture-intent-secret".into()).unwrap();
            let prepared = vault.prepare_store(&binding, &secret).unwrap();
            assert!(memory.items.lock().unwrap().is_empty());
            journal.record(&prepared, Phase::Intent).unwrap();
            assert!(!fs::read_to_string(&journal.path)
                .unwrap()
                .contains(secret.expose()));
            if write_vault {
                vault.store_prepared(&prepared, &secret).unwrap();
            }
            // Reopen from disk as a fresh process would, without a phase update.
            Journal::new(dir.path())
                .recover(&vault, |_, reference| {
                    assert_eq!(reference, &prepared.reference);
                    Ok(Some(false))
                })
                .unwrap();
            assert!(vault.load(&prepared.reference, &binding).unwrap().is_none());
            journal.ensure_clear().unwrap();
            assert_eq!(
                memory.deletes.load(Ordering::SeqCst),
                usize::from(write_vault)
            );
        }
    }
    #[test]
    fn prepared_reference_cannot_overwrite_a_grant() {
        let (_dir, _journal, vault, metadata, _) = fixture();
        let other = NativeKeySecret::new("different-fixture-secret".into()).unwrap();
        assert!(vault.store_prepared(&metadata, &other).is_err());
        assert_eq!(
            vault
                .load(&metadata.reference, &metadata.binding)
                .unwrap()
                .unwrap()
                .expose(),
            "fixture-recovery-secret"
        );
    }
    #[test]
    fn oversized_journal_is_rejected_without_cleanup() {
        let (_dir, journal, vault, _, memory) = fixture();
        fs::write(&journal.path, vec![b' '; MAX_JOURNAL_BYTES + 1]).unwrap();
        assert!(journal.recover(&vault, |_, _| panic!("oversized")).is_err());
        assert_eq!(memory.deletes.load(Ordering::SeqCst), 0);
        assert!(journal.path.exists());
    }
    #[cfg(unix)]
    #[test]
    fn journal_is_private_and_refuses_symlink_reads_and_writes() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let (dir, journal, vault, metadata, memory) = fixture();
        assert_eq!(
            fs::metadata(&journal.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let target = dir.path().join("untouched");
        fs::write(&target, b"untouched").unwrap();
        journal.clear().unwrap();
        symlink(&target, &journal.path).unwrap();
        assert!(journal.recover(&vault, |_, _| panic!("symlink")).is_err());
        assert!(journal.record(&metadata, Phase::Intent).is_err());
        assert!(journal.clear().is_err());
        assert_eq!(fs::read(target).unwrap(), b"untouched");
        assert_eq!(memory.deletes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn referenced_records_never_delete_for_any_phase() {
        for phase in [
            Phase::Intent,
            Phase::Stored,
            Phase::Committed,
            Phase::CleanupPending,
        ] {
            let (_dir, journal, vault, metadata, memory) = fixture();
            journal.record(&metadata, phase).unwrap();
            journal.recover(&vault, |_, _| Ok(Some(true))).unwrap();
            assert_eq!(memory.deletes.load(Ordering::SeqCst), 0);
            assert!(vault
                .load(&metadata.reference, &metadata.binding)
                .unwrap()
                .is_some());
            journal.ensure_clear().unwrap();
        }
    }
    #[test]
    fn unknown_and_failed_db_reads_retain_journal_and_secret() {
        for result in [Ok(None), Err("database unavailable".into())] {
            let (_dir, journal, vault, metadata, memory) = fixture();
            let before = fs::read(&journal.path).unwrap();
            let _ = journal.recover(&vault, |_, _| result.clone());
            assert_eq!(fs::read(&journal.path).unwrap(), before);
            assert!(journal.ensure_clear().is_err());
            assert_eq!(memory.deletes.load(Ordering::SeqCst), 0);
            assert!(vault
                .load(&metadata.reference, &metadata.binding)
                .unwrap()
                .is_some());
        }
    }
    #[test]
    fn absent_record_cleanup_failure_is_retryable_and_idempotent() {
        let (_dir, journal, vault, metadata, memory) = fixture();
        memory.fail.store(true, Ordering::SeqCst);
        assert!(journal.recover(&vault, |_, _| Ok(Some(false))).is_err());
        let bytes = fs::read(&journal.path).unwrap();
        assert_eq!(
            serde_json::from_slice::<Entry>(&bytes).unwrap().phase,
            Phase::CleanupPending
        );
        assert!(!String::from_utf8(bytes)
            .unwrap()
            .contains("fixture-recovery-secret"));
        assert!(vault
            .load(&metadata.reference, &metadata.binding)
            .unwrap()
            .is_some());
        memory.fail.store(false, Ordering::SeqCst);
        journal.recover(&vault, |_, _| Ok(Some(false))).unwrap();
        journal.ensure_clear().unwrap();
        assert!(vault
            .load(&metadata.reference, &metadata.binding)
            .unwrap()
            .is_none());
        journal
            .recover(&vault, |_, _| panic!("cleared journal"))
            .unwrap();
        assert_eq!(memory.deletes.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn malformed_journal_is_retained_without_vault_access() {
        let (_dir, journal, vault, _, memory) = fixture();
        fs::write(&journal.path, b"broken").unwrap();
        assert!(journal
            .recover(&vault, |_, _| panic!("invalid journal"))
            .is_err());
        assert_eq!(fs::read(&journal.path).unwrap(), b"broken");
        assert_eq!(memory.deletes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn raw_database_metadata_scan_never_defaults_corruption_to_absence() {
        let (_dir, _journal, _vault, metadata, _) = fixture();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE providers(meta TEXT);")
            .unwrap();
        assert_eq!(referenced(&conn, &metadata.reference).unwrap(), Some(false));
        conn.execute("INSERT INTO providers VALUES (?1)", ["broken"])
            .unwrap();
        assert_eq!(referenced(&conn, &metadata.reference).unwrap(), None);
        let value = serde_json::json!({"osirBinding":{"reference":metadata.reference}}).to_string();
        conn.execute("INSERT INTO providers VALUES (?1)", [value])
            .unwrap();
        assert_eq!(referenced(&conn, &metadata.reference).unwrap(), Some(true));
        conn.execute_batch(
            "DELETE FROM providers; INSERT INTO providers VALUES ('{}'), ('{\"osirBinding\":{}}');",
        )
        .unwrap();
        assert_eq!(referenced(&conn, &metadata.reference).unwrap(), None);
        conn.execute_batch("DROP TABLE providers;").unwrap();
        assert!(referenced(&conn, &metadata.reference).is_err());
    }
}
