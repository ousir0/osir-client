//! Explicit recovery of an existing native model-key grant. No cloud mutation
//! or configuration projection, and no credential material crosses IPC.
use crate::{
    app_config::AppType,
    database::Database,
    services::osir::{credentials::CredentialBackend, key_vault::KeyVault, managed},
    store::AppState,
};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialRecoveryStage {
    Restored,
    Missing,
    ConfirmationRequired,
    Unavailable,
    InvalidConnection,
    Changed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedCredentialRecovery {
    restored: bool,
    stage: CredentialRecoveryStage,
}
impl ManagedCredentialRecovery {
    fn at(stage: CredentialRecoveryStage) -> Self {
        Self {
            restored: stage == CredentialRecoveryStage::Restored,
            stage,
        }
    }
}

fn tool(value: &str) -> Result<AppType, String> {
    match value {
        "codex" => Ok(AppType::Codex),
        "claude" => Ok(AppType::Claude),
        "claude-desktop" => Ok(AppType::ClaudeDesktop),
        "opencode" => Ok(AppType::OpenCode),
        "gemini" => Ok(AppType::Gemini),
        _ => Err("此工具不支持恢复账户连接凭据".into()),
    }
}

fn recover_saved<B: CredentialBackend>(
    db: &Database,
    app: &AppType,
    provider_id: &str,
    vault: &KeyVault<B>,
) -> ManagedCredentialRecovery {
    use crate::services::osir::key_vault::KeyVaultError;
    use CredentialRecoveryStage::*;
    let Ok(_operation) = crate::services::osir::key_vault::recovery_lock() else {
        return ManagedCredentialRecovery::at(Unavailable);
    };
    let saved = match db.get_provider_by_id(provider_id, app.as_str()) {
        Ok(Some(saved)) => saved,
        Ok(None) => return ManagedCredentialRecovery::at(InvalidConnection),
        Err(_) => return ManagedCredentialRecovery::at(Unavailable),
    };
    if managed::validate_managed_provider_shape(app, &saved).is_err() {
        return ManagedCredentialRecovery::at(InvalidConnection);
    }
    let Some(binding) = saved
        .meta
        .as_ref()
        .and_then(|meta| meta.osir_binding.as_ref())
    else {
        return ManagedCredentialRecovery::at(InvalidConnection);
    };
    // The only interactive read: the binding and immutable reference come from
    // the fresh native row, never from a renderer-supplied credential tuple.
    let stage = match vault.load_for_user(&binding.reference, &binding.binding) {
        Ok(Some(_)) => Restored,
        Ok(None) => Missing,
        Err(KeyVaultError::InteractionRequired) => ConfirmationRequired,
        Err(KeyVaultError::InvalidFormat | KeyVaultError::BindingMismatch) => InvalidConnection,
        Err(KeyVaultError::Storage | KeyVaultError::Unavailable) => Unavailable,
    };
    if stage != Restored {
        return ManagedCredentialRecovery::at(stage);
    }
    // A user may change/delete the connection while the system dialog is open.
    // Do not publish success for a grant that no longer belongs to this row.
    let unchanged = db
        .get_provider_by_id(provider_id, app.as_str())
        .ok()
        .flatten()
        .is_some_and(|current| {
            managed::validate_managed_provider_shape(app, &current).is_ok()
                && current
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.osir_binding.as_ref())
                    == Some(binding)
        });
    if !unchanged {
        let _ = vault.forget(&binding.reference);
        return ManagedCredentialRecovery::at(Changed);
    }
    ManagedCredentialRecovery::at(Restored)
}

/// Invoke only from the user's explicit "恢复连接凭据" action. Startup,
/// background refresh, page reads, launch and proxy requests never call this.
#[tauri::command]
pub async fn osir_recover_connection_credential(
    state: State<'_, AppState>,
    app_type: String,
    provider_id: String,
) -> Result<ManagedCredentialRecovery, String> {
    let app = tool(&app_type)?;
    if provider_id.is_empty()
        || provider_id.len() > 256
        || provider_id.trim() != provider_id
        || provider_id.chars().any(char::is_control)
    {
        return Err("连接参数无效".into());
    }
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Ok(vault) = managed::vault() else {
            return ManagedCredentialRecovery::at(CredentialRecoveryStage::Unavailable);
        };
        recover_saved(&db, &app, &provider_id, &vault)
    })
    .await
    .map_err(|_| "恢复连接凭据未完成，请重试".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::{AuthBindingSource, ManagedBinding, Provider, ProviderMeta},
        services::osir::{
            credentials::CredentialError,
            key_vault::{KeyBinding, KeyTool, NativeKeySecret, GATEWAY_ORIGIN},
        },
    };
    use serde_json::json;
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    #[derive(Default)]
    struct MemoryState {
        items: Mutex<HashMap<(String, String), Vec<u8>>>,
        locked: AtomicBool,
        interactive_mode: AtomicU8,
        interactive_reads: AtomicUsize,
        writes: AtomicUsize,
        deletes: AtomicUsize,
        after_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }
    #[derive(Clone, Default)]
    struct Memory(Arc<MemoryState>);
    impl CredentialBackend for Memory {
        fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            self.0.writes.fetch_add(1, Ordering::SeqCst);
            self.0
                .items
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), bytes.into());
            Ok(())
        }
        fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            if self.0.locked.load(Ordering::SeqCst) {
                return Err(CredentialError::InteractionRequired);
            }
            Ok(self
                .0
                .items
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned())
        }
        fn read_interactive(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<Vec<u8>>, CredentialError> {
            self.0.interactive_reads.fetch_add(1, Ordering::SeqCst);
            match self.0.interactive_mode.load(Ordering::SeqCst) {
                1 => return Err(CredentialError::InteractionRequired),
                2 => return Err(CredentialError::Unavailable),
                3 => return Ok(None),
                _ => (),
            }
            let bytes = self
                .0
                .items
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned();
            let callback = self.0.after_read.lock().unwrap().take();
            if let Some(callback) = callback {
                callback();
            }
            Ok(bytes)
        }
        fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
            self.0.deletes.fetch_add(1, Ordering::SeqCst);
            self.0
                .items
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }
    struct Fixture {
        db: Arc<Database>,
        vault: KeyVault<Memory>,
        memory: Memory,
        provider: Provider,
    }
    fn fixture() -> Fixture {
        let memory = Memory::default();
        let vault = KeyVault::with_backend(
            &managed::trusted_account_origin().unwrap(),
            "com.osir.recovery.fixture",
            memory.clone(),
        )
        .unwrap();
        let binding = KeyBinding {
            account_origin: managed::trusted_account_origin().unwrap(),
            account_id: "synthetic-account".into(),
            key_id: 7,
            group_id: Some(3),
            tool: KeyTool::ClaudeCode,
            provider_id: "existing-managed-fixture".into(),
            gateway_origin: GATEWAY_ORIGIN.into(),
        };
        let meta = vault
            .store(
                &binding,
                &NativeKeySecret::new("synthetic-recovered-key".into()).unwrap(),
            )
            .unwrap();
        let mut provider = Provider::with_id(
            binding.provider_id.clone(),
            "Fixture".into(),
            json!({"env":{"ANTHROPIC_BASE_URL":GATEWAY_ORIGIN}}),
            None,
        );
        provider.meta = Some(ProviderMeta {
            osir_binding: Some(ManagedBinding {
                source: AuthBindingSource::ManagedAccount,
                binding,
                reference: meta.reference,
                masked_key: meta.masked_key,
            }),
            ..Default::default()
        });
        let db = Arc::new(Database::memory().unwrap());
        db.save_provider("claude", &provider).unwrap();
        memory.0.locked.store(true, Ordering::SeqCst);
        Fixture {
            db,
            vault,
            memory,
            provider,
        }
    }
    fn binding(provider: &Provider) -> &ManagedBinding {
        provider
            .meta
            .as_ref()
            .unwrap()
            .osir_binding
            .as_ref()
            .unwrap()
    }
    fn assert_no_mutation(f: &Fixture) {
        assert_eq!(
            f.memory.0.writes.load(Ordering::SeqCst),
            1,
            "no new or replacement key"
        );
        assert_eq!(f.memory.0.deletes.load(Ordering::SeqCst), 0);
        let row =
            f.db.get_provider_by_id(&f.provider.id, "claude")
                .unwrap()
                .unwrap();
        assert_eq!(
            serde_json::to_value(row).unwrap(),
            serde_json::to_value(&f.provider).unwrap()
        );
    }

    #[test]
    fn explicit_recovery_restores_only_original_native_grant_for_silent_proxy_auth() {
        let f = fixture();
        let b = binding(&f.provider);
        assert!(f.vault.load(&b.reference, &b.binding).is_err());
        assert_eq!(f.memory.0.interactive_reads.load(Ordering::SeqCst), 0);
        let result = recover_saved(&f.db, &AppType::Claude, &f.provider.id, &f.vault);
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({"restored":true,"stage":"restored"})
        );
        assert_eq!(f.memory.0.interactive_reads.load(Ordering::SeqCst), 1);
        assert_no_mutation(&f);
        // The backend still refuses all silent reads ("Allow once" semantics).
        // A later request resolves through the native recovered grant, not IPC.
        let provider =
            f.db.get_provider_by_id(&f.provider.id, "claude")
                .unwrap()
                .unwrap();
        managed::test_support::with_vault(f.vault, || {
            let resolved = managed::resolve_provider(&AppType::Claude, &provider).unwrap();
            assert_eq!(
                resolved.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
                "synthetic-recovered-key"
            );
        });
        assert_eq!(f.memory.0.interactive_reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn claude_desktop_explicit_recovery_keeps_desktop_binding_routes_and_cli_separate() {
        assert_eq!(tool("claude-desktop").unwrap(), AppType::ClaudeDesktop);
        let memory = Memory::default();
        let vault = KeyVault::with_backend(
            &managed::trusted_account_origin().unwrap(),
            "com.osir.desktop.recovery.fixture",
            memory.clone(),
        )
        .unwrap();
        let mut row = crate::services::osir::connection::provider_row(
            &crate::services::osir::connection::ConnectionRequest {
                tool: "claude-desktop".into(),
                key_id: 7,
                name: "Desktop recovery".into(),
                hide_aggregation_name: None,
                model: "desktop-model".into(),
                models: vec!["desktop-model".into(), "second-model".into()],
                generation: 1,
            },
            "existing-desktop-grant".into(),
        )
        .unwrap();
        let binding = KeyBinding {
            account_origin: managed::trusted_account_origin().unwrap(),
            account_id: "fixture-account".into(),
            key_id: 7,
            group_id: Some(3),
            tool: KeyTool::ClaudeDesktop,
            provider_id: row.id.clone(),
            gateway_origin: GATEWAY_ORIGIN.into(),
        };
        let stored = vault
            .store(
                &binding,
                &NativeKeySecret::new("desktop-recovered-key".into()).unwrap(),
            )
            .unwrap();
        row.meta.as_mut().unwrap().osir_binding = Some(ManagedBinding {
            source: AuthBindingSource::ManagedAccount,
            binding,
            reference: stored.reference,
            masked_key: stored.masked_key,
        });
        let db = Database::memory().unwrap();
        db.save_provider("claude-desktop", &row).unwrap();
        let before = serde_json::to_value(&row).unwrap();
        memory.0.locked.store(true, Ordering::SeqCst);
        let b = row.meta.as_ref().unwrap().osir_binding.as_ref().unwrap();
        assert!(vault.load(&b.reference, &b.binding).is_err());
        assert!(!recover_saved(&db, &AppType::Claude, &row.id, &vault).restored);
        assert_eq!(memory.0.interactive_reads.load(Ordering::SeqCst), 0);
        assert!(recover_saved(&db, &AppType::ClaudeDesktop, &row.id, &vault).restored);
        assert_eq!(memory.0.interactive_reads.load(Ordering::SeqCst), 1);
        assert_eq!(memory.0.writes.load(Ordering::SeqCst), 1);
        assert_eq!(memory.0.deletes.load(Ordering::SeqCst), 0);
        assert_eq!(
            serde_json::to_value(
                db.get_provider_by_id(&row.id, "claude-desktop")
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            before
        );
        managed::test_support::with_vault(vault, || {
            let resolved = managed::resolve_provider(&AppType::ClaudeDesktop, &row).unwrap();
            assert_eq!(
                resolved.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
                "desktop-recovered-key"
            );
            assert!(managed::resolve_provider(&AppType::Claude, &row).is_err());
        });
    }

    #[test]
    fn codex_explicit_recovery_preserves_saved_route_binding_and_aggregation_display() {
        use crate::mode::{stack, state::StackState};
        use crate::services::osir::key_vault::KeyVaultError;

        let memory = Memory::default();
        let vault = KeyVault::with_backend(
            &managed::trusted_account_origin().unwrap(),
            "com.osir.codex.recovery.fixture",
            memory.clone(),
        )
        .unwrap();
        let key_binding = KeyBinding {
            account_origin: managed::trusted_account_origin().unwrap(),
            account_id: "synthetic-codex-account".into(),
            key_id: 17,
            group_id: Some(3),
            tool: KeyTool::Codex,
            provider_id: "existing-codex-managed-fixture".into(),
            gateway_origin: GATEWAY_ORIGIN.into(),
        };
        let metadata = vault
            .store(
                &key_binding,
                &NativeKeySecret::new("synthetic-codex-recovered-key".into()).unwrap(),
            )
            .unwrap();
        let mut provider = Provider::with_id(
            key_binding.provider_id.clone(),
            "用户保存的 Codex 连接".into(),
            json!({
                "auth":{},
                "config":"model_provider = \"saved_osir_route\"\nmodel = \"shared-model\"\n[model_providers.saved_osir_route]\nname = \"Saved route\"\nbase_url = \"https://api.osirclaw.com/v1\"\nwire_api = \"responses\"\n",
                "modelCatalog":{"models":[{"model":"shared-model"}]},
                "mcpServers":{"user-owned":{"command":"fixture-mcp"}},
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            hide_aggregation_name: Some(false),
            osir_binding: Some(ManagedBinding {
                source: AuthBindingSource::ManagedAccount,
                binding: key_binding,
                reference: metadata.reference,
                masked_key: metadata.masked_key,
            }),
            ..Default::default()
        });
        managed::validate_managed_provider_shape(&AppType::Codex, &provider).unwrap();
        let db = Database::memory().unwrap();
        db.save_provider("codex", &provider).unwrap();
        let before = serde_json::to_value(&provider).unwrap();
        let mut stack = StackState::default();
        let key = stack::allocate_key(&mut stack, &provider);
        let machine_id = stack::encode(&AppType::Codex, &key, "shared-model", false);
        let published = stack::codex_model_ids(&key, &provider);
        assert_eq!(published, vec![machine_id.clone()]);
        let stack_before = serde_json::to_value(&stack).unwrap();

        memory.0.locked.store(true, Ordering::SeqCst);
        let original_binding = binding(&provider);
        assert!(matches!(
            vault.load(&original_binding.reference, &original_binding.binding),
            Err(KeyVaultError::InteractionRequired)
        ));
        assert_eq!(memory.0.interactive_reads.load(Ordering::SeqCst), 0);
        let recovered = recover_saved(&db, &AppType::Codex, &provider.id, &vault);
        assert_eq!(
            serde_json::to_value(recovered).unwrap(),
            json!({"restored":true,"stage":"restored"})
        );
        assert!(memory.0.locked.load(Ordering::SeqCst));
        let cached = vault
            .load(&original_binding.reference, &original_binding.binding)
            .unwrap()
            .unwrap();
        assert_eq!(cached.expose(), "synthetic-codex-recovered-key");
        let saved = db
            .get_provider_by_id(&provider.id, "codex")
            .unwrap()
            .unwrap();
        assert_eq!(serde_json::to_value(&saved).unwrap(), before);
        assert_eq!(binding(&saved), original_binding);
        assert_eq!(
            saved.meta.as_ref().unwrap().hide_aggregation_name,
            Some(false)
        );
        assert_eq!(stack::allocate_key(&mut stack, &saved), key);
        assert_eq!(stack::codex_model_ids(&key, &saved), published);
        assert_eq!(serde_json::to_value(&stack).unwrap(), stack_before);
        assert_eq!(
            stack::encode(&AppType::Codex, &key, "shared-model", false),
            machine_id
        );

        // The actual Codex materialization branch must consume that same cached
        // grant silently while leaving the persistent connection/config intact.
        managed::test_support::with_vault(vault, || {
            let resolved = managed::resolve_provider(&AppType::Codex, &saved).unwrap();
            assert_eq!(
                resolved.settings_config["auth"]["OPENAI_API_KEY"],
                "synthetic-codex-recovered-key"
            );
            assert_eq!(
                serde_json::to_value(&resolved.meta).unwrap(),
                serde_json::to_value(&saved.meta).unwrap()
            );
            let route: toml::Value = resolved.settings_config["config"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(route["model_provider"].as_str(), Some("saved_osir_route"));
            assert_eq!(route["model"].as_str(), Some("shared-model"));
            assert_eq!(
                resolved.settings_config["mcpServers"],
                saved.settings_config["mcpServers"]
            );
        });
        assert_eq!(memory.0.interactive_reads.load(Ordering::SeqCst), 1);
        assert_eq!(
            memory.0.writes.load(Ordering::SeqCst),
            1,
            "Only initial fixture storage; recovery creates no replacement key"
        );
        assert_eq!(memory.0.deletes.load(Ordering::SeqCst), 0);
        assert_eq!(
            serde_json::to_value(
                db.get_provider_by_id(&provider.id, "codex")
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            before
        );
    }

    #[test]
    fn denied_unavailable_and_missing_recovery_never_write_or_publish_a_grant() {
        let f = fixture();
        for (mode, expected) in [
            (1, CredentialRecoveryStage::ConfirmationRequired),
            (2, CredentialRecoveryStage::Unavailable),
            (3, CredentialRecoveryStage::Missing),
        ] {
            f.memory.0.interactive_mode.store(mode, Ordering::SeqCst);
            let result = recover_saved(&f.db, &AppType::Claude, &f.provider.id, &f.vault);
            assert!(!result.restored);
            assert_eq!(result.stage, expected);
            let b = binding(&f.provider);
            assert!(f.vault.load(&b.reference, &b.binding).is_err());
            assert_no_mutation(&f);
        }
    }

    #[test]
    fn unsupported_unmanaged_missing_or_invalid_connections_do_not_request_system_access() {
        let mut f = fixture();
        assert_eq!(tool("claude-desktop").unwrap(), AppType::ClaudeDesktop);
        assert!(tool("workbuddy").is_err());
        assert_eq!(
            recover_saved(&f.db, &AppType::Claude, "missing", &f.vault).stage,
            CredentialRecoveryStage::InvalidConnection
        );
        let managed_meta = f.provider.meta.clone();
        f.provider.meta = None;
        f.db.save_provider("claude", &f.provider).unwrap();
        assert_eq!(
            recover_saved(&f.db, &AppType::Claude, &f.provider.id, &f.vault).stage,
            CredentialRecoveryStage::InvalidConnection
        );
        f.provider.meta = managed_meta;
        f.provider.settings_config["env"]["ANTHROPIC_BASE_URL"] = json!("https://invalid.example");
        f.db.save_provider("claude", &f.provider).unwrap();
        assert_eq!(
            recover_saved(&f.db, &AppType::Claude, &f.provider.id, &f.vault).stage,
            CredentialRecoveryStage::InvalidConnection
        );
        assert_eq!(f.memory.0.interactive_reads.load(Ordering::SeqCst), 0);
        assert_no_mutation(&f);
    }

    #[test]
    fn recovery_rechecks_native_binding_after_system_confirmation_and_forgets_stale_grant() {
        let f = fixture();
        let db = f.db.clone();
        let mut changed = f.provider.clone();
        changed
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .binding
            .key_id += 1;
        *f.memory.0.after_read.lock().unwrap() = Some(Box::new(move || {
            db.save_provider("claude", &changed).unwrap();
        }));
        let result = recover_saved(&f.db, &AppType::Claude, &f.provider.id, &f.vault);
        assert_eq!(result.stage, CredentialRecoveryStage::Changed);
        assert!(!result.restored);
        let b = binding(&f.provider);
        assert!(
            f.vault.load(&b.reference, &b.binding).is_err(),
            "the old grant must be evicted"
        );
        assert_eq!(f.memory.0.writes.load(Ordering::SeqCst), 1);
        assert_eq!(f.memory.0.deletes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn recovery_rejects_a_different_key_binding_even_when_the_item_is_readable() {
        let mut f = fixture();
        f.provider
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .binding
            .group_id = None;
        f.db.save_provider("claude", &f.provider).unwrap();
        assert_eq!(
            recover_saved(&f.db, &AppType::Claude, &f.provider.id, &f.vault).stage,
            CredentialRecoveryStage::InvalidConnection
        );
        assert_eq!(f.memory.0.interactive_reads.load(Ordering::SeqCst), 1);
        assert_no_mutation(&f);
    }
}
