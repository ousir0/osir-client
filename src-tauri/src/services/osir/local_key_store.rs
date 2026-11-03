//! Model keys use CC Switch's local SQLite backup/restore lifecycle. Account
//! refresh tokens continue to use the OS credential store. Never expose rows
//! through renderer DTOs or log database errors that could contain payloads.
use super::credentials::{CredentialBackend, CredentialError, NativeKeychain};
use crate::database::Database;
use rusqlite::{params, OptionalExtension};
use std::sync::Arc;

pub struct LocalCredentialBackend<B = NativeKeychain> {
    db: Arc<Database>,
    legacy: B,
}

impl LocalCredentialBackend {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            legacy: NativeKeychain,
        }
    }
}

impl<B: CredentialBackend> LocalCredentialBackend<B> {
    #[cfg(test)]
    pub(crate) fn with_legacy(db: Arc<Database>, legacy: B) -> Self {
        Self { db, legacy }
    }
    // Missing row permits legacy lookup; a NULL payload is a durable deletion
    // marker. Retaining an old system item must never resurrect a deleted key.
    fn read_local(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Option<Vec<u8>>>, CredentialError> {
        let conn = self
            .db
            .conn
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        conn.query_row(
            "SELECT payload FROM osir_model_credentials WHERE service = ?1 AND account = ?2",
            params![service, account],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| CredentialError::Storage)
    }

    fn read_with_legacy(
        &self,
        service: &str,
        account: &str,
        interactive: bool,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        if let Some(local) = self.read_local(service, account)? {
            return Ok(local);
        }
        // No DB mutex is held across OS calls. The caller must validate all
        // bindings before commit_model_key_read persists the migration.
        if interactive {
            self.legacy.read_interactive(service, account)
        } else {
            self.legacy.read_without_prompt(service, account)
        }
    }

    fn validate_payload(bytes: &[u8]) -> Result<(), CredentialError> {
        if bytes.is_empty() || bytes.len() > super::key_vault::MAX_RECORD {
            return Err(CredentialError::InvalidFormat);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::osir::key_vault::{
        KeyBinding, KeyMetadata, KeyTool, KeyVault, KeyVaultError, NativeKeySecret, GATEWAY_ORIGIN,
    };
    use std::{collections::HashMap, sync::Mutex};

    const IDENTITY: &str = "com.osir.local.fixture";
    const ORIGIN: &str = "https://osirclaw.com";

    #[derive(Default)]
    struct State {
        items: HashMap<(String, String), Vec<u8>>,
        locked: bool,
        denied: bool,
        silent_reads: usize,
        interactive_reads: usize,
    }
    #[derive(Clone, Default)]
    struct Legacy(Arc<Mutex<State>>);
    impl CredentialBackend for Legacy {
        fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            self.0
                .lock()
                .unwrap()
                .items
                .insert((service.into(), account.into()), bytes.to_vec());
            Ok(())
        }
        fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            let mut state = self.0.lock().unwrap();
            state.silent_reads += 1;
            if state.locked {
                return Err(CredentialError::InteractionRequired);
            }
            Ok(state.items.get(&(service.into(), account.into())).cloned())
        }
        fn read_interactive(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<Vec<u8>>, CredentialError> {
            let mut state = self.0.lock().unwrap();
            state.interactive_reads += 1;
            if state.denied {
                return Err(CredentialError::InteractionRequired);
            }
            Ok(state.items.get(&(service.into(), account.into())).cloned())
        }
        fn remove(&self, _: &str, _: &str) -> Result<(), CredentialError> {
            panic!("migration and local deletion must preserve legacy recovery source")
        }
    }

    fn binding() -> KeyBinding {
        KeyBinding {
            account_origin: ORIGIN.into(),
            account_id: "42".into(),
            key_id: 7,
            group_id: Some(3),
            tool: KeyTool::Codex,
            provider_id: "synthetic-provider".into(),
            gateway_origin: GATEWAY_ORIGIN.into(),
        }
    }
    fn vault(db: &Arc<Database>, legacy: &Legacy) -> KeyVault<LocalCredentialBackend<Legacy>> {
        KeyVault::with_backend(
            ORIGIN,
            IDENTITY,
            LocalCredentialBackend::with_legacy(db.clone(), legacy.clone()),
        )
        .unwrap()
    }
    fn old_key(legacy: &Legacy) -> KeyMetadata {
        let old = KeyVault::with_backend(ORIGIN, IDENTITY, legacy.clone()).unwrap();
        let stored = old
            .store(
                &binding(),
                &NativeKeySecret::new("synthetic-model-key".into()).unwrap(),
            )
            .unwrap();
        legacy.0.lock().unwrap().silent_reads = 0;
        stored
    }
    fn count(db: &Database) -> i64 {
        db.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM osir_model_credentials", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn slot(meta: &KeyMetadata) -> String {
        serde_json::to_value(&meta.reference).unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn service() -> String {
        format!(
            "{IDENTITY}.osir.model-keys.v1.{}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        )
    }

    #[test]
    fn local_model_keys_save_and_restart_without_any_system_store_access() {
        let db = Arc::new(Database::memory().unwrap());
        let legacy = Legacy::default();
        legacy.0.lock().unwrap().locked = true;
        let meta = vault(&db, &legacy)
            .store(
                &binding(),
                &NativeKeySecret::new("synthetic-new-key".into()).unwrap(),
            )
            .unwrap();
        let restarted = vault(&db, &legacy);
        assert_eq!(
            restarted
                .load(&meta.reference, &meta.binding)
                .unwrap()
                .unwrap()
                .expose(),
            "synthetic-new-key"
        );
        assert_eq!(legacy.0.lock().unwrap().silent_reads, 0);
        assert_eq!(legacy.0.lock().unwrap().interactive_reads, 0);
    }

    #[test]
    fn legacy_model_keys_migrate_silently_and_keep_the_original_for_rollback() {
        let db = Arc::new(Database::memory().unwrap());
        let legacy = Legacy::default();
        let meta = old_key(&legacy);
        assert!(vault(&db, &legacy)
            .load(&meta.reference, &meta.binding)
            .unwrap()
            .is_some());
        assert_eq!(count(&db), 1);
        assert_eq!(legacy.0.lock().unwrap().items.len(), 1);
        legacy.0.lock().unwrap().locked = true;
        assert!(vault(&db, &legacy)
            .load(&meta.reference, &meta.binding)
            .unwrap()
            .is_some());
        assert_eq!(legacy.0.lock().unwrap().silent_reads, 1);
        assert_eq!(legacy.0.lock().unwrap().interactive_reads, 0);
    }

    #[test]
    fn locked_or_cancelled_migration_preserves_the_source_until_explicit_retry() {
        let db = Arc::new(Database::memory().unwrap());
        let legacy = Legacy::default();
        let meta = old_key(&legacy);
        {
            let mut state = legacy.0.lock().unwrap();
            state.locked = true;
            state.denied = true;
        }
        let local = vault(&db, &legacy);
        assert!(matches!(
            local.load(&meta.reference, &meta.binding),
            Err(KeyVaultError::InteractionRequired)
        ));
        assert_eq!(legacy.0.lock().unwrap().interactive_reads, 0);
        assert!(local.load_for_user(&meta.reference, &meta.binding).is_err());
        assert_eq!(count(&db), 0);
        assert_eq!(legacy.0.lock().unwrap().items.len(), 1);
        legacy.0.lock().unwrap().denied = false;
        assert!(local
            .load_for_user(&meta.reference, &meta.binding)
            .unwrap()
            .is_some());
        assert!(vault(&db, &legacy)
            .load(&meta.reference, &meta.binding)
            .unwrap()
            .is_some());
        assert_eq!(legacy.0.lock().unwrap().interactive_reads, 2);
    }

    #[test]
    fn failed_local_migration_write_retains_the_legacy_item_and_can_retry() {
        let db = Arc::new(Database::memory().unwrap());
        let legacy = Legacy::default();
        let meta = old_key(&legacy);
        db.conn.lock().unwrap().execute_batch("CREATE TRIGGER fail_migration BEFORE INSERT ON osir_model_credentials BEGIN SELECT RAISE(ABORT, 'synthetic disk failure'); END;").unwrap();
        assert!(matches!(
            vault(&db, &legacy).load(&meta.reference, &meta.binding),
            Err(KeyVaultError::Storage)
        ));
        assert_eq!(count(&db), 0);
        assert_eq!(legacy.0.lock().unwrap().items.len(), 1);
        db.conn
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_migration;")
            .unwrap();
        assert!(vault(&db, &legacy)
            .load(&meta.reference, &meta.binding)
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_mismatched_legacy_binding_is_neither_migrated_nor_erased() {
        let db = Arc::new(Database::memory().unwrap());
        let legacy = Legacy::default();
        let meta = old_key(&legacy);
        let mut wrong = binding();
        wrong.group_id = Some(4);
        assert!(matches!(
            vault(&db, &legacy).load(&meta.reference, &wrong),
            Err(KeyVaultError::BindingMismatch)
        ));
        assert_eq!(count(&db), 0);
        assert_eq!(legacy.0.lock().unwrap().items.len(), 1);
    }

    #[test]
    fn deletion_and_failed_concurrent_migration_never_resurrect_a_legacy_key() {
        let db = Arc::new(Database::memory().unwrap());
        let legacy = Legacy::default();
        let meta = old_key(&legacy);
        let backend = LocalCredentialBackend::with_legacy(db.clone(), legacy.clone());
        let bytes = backend.read(&service(), &slot(&meta)).unwrap().unwrap();
        backend.remove(&service(), &slot(&meta)).unwrap();
        assert!(backend
            .commit_model_key_read(&service(), &slot(&meta), &bytes)
            .is_err());
        assert!(vault(&db, &legacy)
            .load(&meta.reference, &meta.binding)
            .unwrap()
            .is_none());
        assert_eq!(legacy.0.lock().unwrap().silent_reads, 1);
        assert_eq!(legacy.0.lock().unwrap().items.len(), 1);
    }

    #[test]
    fn local_recovery_does_not_cache_keys_across_database_deletion_or_restore() {
        let db = Arc::new(Database::memory().unwrap());
        let legacy = Legacy::default();
        let meta = old_key(&legacy);
        let local = vault(&db, &legacy);
        local
            .load_for_user(&meta.reference, &meta.binding)
            .unwrap()
            .unwrap();
        let snapshot = db.snapshot_to_memory().unwrap();
        local.delete(&meta.reference, &meta.binding).unwrap();
        assert!(local
            .load(&meta.reference, &meta.binding)
            .unwrap()
            .is_none());
        {
            let mut conn = db.conn.lock().unwrap();
            rusqlite::backup::Backup::new(&snapshot, &mut conn)
                .unwrap()
                .step(-1)
                .unwrap();
        }
        snapshot
            .execute("DELETE FROM osir_model_credentials", [])
            .unwrap();
        legacy.0.lock().unwrap().locked = true;
        assert!(local
            .load(&meta.reference, &meta.binding)
            .unwrap()
            .is_some());
        {
            let mut conn = db.conn.lock().unwrap();
            rusqlite::backup::Backup::new(&snapshot, &mut conn)
                .unwrap()
                .step(-1)
                .unwrap();
        }
        assert!(local.load(&meta.reference, &meta.binding).is_err());
    }

    #[test]
    fn concurrent_reference_reappearance_blocks_cleanup_atomically() {
        let db = Arc::new(Database::memory().unwrap());
        let legacy = Legacy::default();
        let meta = old_key(&legacy);
        let local = vault(&db, &legacy);
        local.load(&meta.reference, &meta.binding).unwrap().unwrap();
        let raw = serde_json::json!({"osirBinding": {"reference": meta.reference}}).to_string();
        db.conn.lock().unwrap().execute("INSERT INTO providers (id, app_type, name, settings_config, meta) VALUES ('synthetic', 'codex', 'fixture', '{}', ?1)", [&raw]).unwrap();
        assert!(local.delete(&meta.reference, &meta.binding).is_err());
        assert!(local
            .load(&meta.reference, &meta.binding)
            .unwrap()
            .is_some());
    }
}

impl<B: CredentialBackend> CredentialBackend for LocalCredentialBackend<B> {
    fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
        Self::validate_payload(bytes)?;
        let conn = self
            .db
            .conn
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        conn.execute(
            "INSERT INTO osir_model_credentials (service, account, payload) VALUES (?1, ?2, ?3)",
            params![service, account, bytes],
        )
        .map(|_| ())
        .map_err(|_| CredentialError::Storage)
    }

    fn create_model_key(
        &self,
        service: &str,
        account: &str,
        bytes: &[u8],
    ) -> Result<(), CredentialError> {
        // Fresh keys never read the system keychain, even when it is locked.
        self.write(service, account, bytes)
    }

    fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
        self.read_with_legacy(service, account, false)
    }

    fn read_interactive(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        self.read_with_legacy(service, account, true)
    }

    fn commit_model_key_read(
        &self,
        service: &str,
        account: &str,
        bytes: &[u8],
    ) -> Result<(), CredentialError> {
        Self::validate_payload(bytes)?;
        let conn = self
            .db
            .conn
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        // Re-check under the same mutex as INSERT. A concurrent migration is
        // idempotent, but restore/deletion/conflicting payloads must win.
        let existing: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT payload FROM osir_model_credentials WHERE service = ?1 AND account = ?2",
                params![service, account],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| CredentialError::Storage)?;
        match existing {
            Some(Some(current)) if current == bytes => Ok(()),
            Some(_) => Err(CredentialError::Storage),
            None => conn.execute(
                "INSERT INTO osir_model_credentials (service, account, payload) VALUES (?1, ?2, ?3)",
                params![service, account, bytes],
            ).map(|_| ()).map_err(|_| CredentialError::Storage),
        }
        // Preserve the legacy item for rollback. Runtime uses the durable DB
        // copy from here on; no automatic ACL changes or legacy deletion.
    }

    fn cache_model_key_reads(&self) -> bool {
        false
    }

    fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
        let conn = self
            .db
            .conn
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        {
            // Re-check in the same critical section as deletion. Provider
            // restore can run between the journal's earlier absence check and
            // cleanup. Malformed metadata is never proof of non-use.
            let mut stmt = conn
                .prepare("SELECT meta FROM providers")
                .map_err(|_| CredentialError::Storage)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|_| CredentialError::Storage)?;
            for row in rows {
                let meta: serde_json::Value =
                    serde_json::from_str(&row.map_err(|_| CredentialError::Storage)?)
                        .map_err(|_| CredentialError::InvalidFormat)?;
                if !meta.is_object() {
                    return Err(CredentialError::InvalidFormat);
                }
                match meta.get("osirBinding") {
                    None | Some(serde_json::Value::Null) => (),
                    Some(binding) => {
                        let reference = binding
                            .get("reference")
                            .cloned()
                            .ok_or(CredentialError::InvalidFormat)?;
                        let reference: crate::services::osir::key_vault::KeyReference =
                            serde_json::from_value(reference)
                                .map_err(|_| CredentialError::InvalidFormat)?;
                        if reference.id() == account {
                            return Err(CredentialError::Storage);
                        }
                    }
                }
            }
        }
        conn.execute(
            "INSERT INTO osir_model_credentials (service, account, payload) VALUES (?1, ?2, NULL)
             ON CONFLICT(service, account) DO UPDATE SET payload = NULL",
            params![service, account],
        )
        .map(|_| ())
        .map_err(|_| CredentialError::Storage)
    }
}
