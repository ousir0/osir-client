//! Completion policy for irreversible cloud mutations and saved native grants.
//! Contains no credentials and never deletes a saved provider.

use crate::services::osir::{
    client::{CreateKeyError, CreateKeyRequest},
    key_creation_state::KeyCreationState,
};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationAction {
    Inspect,
    Received,
    Reviewed,
}

#[derive(Serialize)]
pub struct CreationStatus {
    operation_id: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum CreationReply {
    Status(CreationStatus),
    Created {
        #[serde(flatten)]
        metadata: crate::services::osir::account::KeyMetadata,
        operation_id: String,
    },
}

/// Shares the existing IPC route so no new command registration is required.
/// Inspect/receipt/review never enter the cloud POST path or write tool config.
#[tauri::command]
pub async fn osir_create_key(
    app: tauri::AppHandle,
    state: State<'_, super::OsirAccountState>,
    request: Option<CreateKeyRequest>,
    generation: u64,
    action: Option<CreationAction>,
    operation_id: Option<String>,
) -> Result<CreationReply, String> {
    let _operation = state
        .connection_operation
        .try_lock()
        .map_err(|_| "连接操作进行中，请稍后重试")?;
    let account_id = super::check_connected(&state.auth, generation)?;
    let directory = app.path().app_data_dir().map_err(|_| "创建记录不可用")?;
    let journal = KeyCreationState::new(&directory, &account_id)?;
    if let Some(action) = action {
        if request.is_some() {
            return Err("创建参数无效".into());
        }
        match action {
            CreationAction::Inspect => {
                if operation_id.is_some() {
                    return Err("创建参数无效".into());
                }
            }
            CreationAction::Received | CreationAction::Reviewed => {
                let operation_id = operation_id.ok_or("请先刷新并核对创建结果")?;
                super::check_connected(&state.auth, generation)?;
                journal.acknowledge(&operation_id, matches!(action, CreationAction::Received))?;
            }
        }
        let operation_id = journal.pending()?;
        super::check_connected(&state.auth, generation)?;
        return Ok(CreationReply::Status(CreationStatus { operation_id }));
    }
    if operation_id.is_some() {
        return Err("创建参数无效".into());
    }
    let request = request.ok_or("创建参数无效")?;
    request.validate().map_err(|_| "创建参数无效")?;
    if !state.auth.status().can_create_keys {
        return Err("创建密钥需要重新授权并允许密钥写入".into());
    }
    // Durable admission precedes AuthManager::create_key, including refresh.
    // All failures after this point conservatively retain the operation record.
    let operation_id = journal.begin()?;
    super::check_connected(&state.auth, generation)?;
    let created = state
        .auth
        .create_key(request, generation)
        .await
        .map_err(|e| e.to_string())?;
    let metadata = after_cloud_success(
        || {
            let session = state.session(generation)?;
            state
                .sync
                .invalidate(&session, None)
                .map_err(|_| super::STALE.to_string())?;
            super::check_connected(&state.auth, generation)?;
            journal.confirm_native(&operation_id)?;
            Ok::<_, String>(created.data.metadata())
        },
        || CreateKeyError::Uncertain.to_string(),
    )?;
    Ok(CreationReply::Created {
        metadata,
        operation_id,
    })
}

const SAVE_UNCONFIRMED: &str =
    "连接保存结果未确认，请检查供应商列表，勿直接重复保存；已保留连接凭据";
const CLEANUP_INCOMPLETE: &str = "连接未保存，本地凭据清理未完成；请勿直接重试";

/// After cloud success, a local/session failure cannot imply a safe POST retry.
pub(super) fn after_cloud_success<T, E>(
    finish: impl FnOnce() -> Result<T, E>,
    uncertain: impl FnOnce() -> String,
) -> Result<T, String> {
    finish().map_err(|_| uncertain())
}

/// A saved tool grant outlives account logout. Only confirmed row absence permits
/// deleting its vault record; failed/unknown DB outcomes retain the credential.
pub(super) fn save_connection<T>(
    save: impl FnOnce() -> Result<T, String>,
    row_absent: impl FnOnce() -> Result<bool, String>,
    cleanup: impl FnOnce() -> Result<(), String>,
    check_session: impl FnOnce() -> Result<(), String>,
) -> Result<T, String> {
    match save() {
        Ok(saved) => {
            check_session().map_err(|_| SAVE_UNCONFIRMED.to_string())?;
            Ok(saved)
        }
        Err(error) => match row_absent() {
            Ok(true) => {
                cleanup().map_err(|_| CLEANUP_INCOMPLETE.to_string())?;
                Err(error)
            }
            Ok(false) | Err(_) => Err(SAVE_UNCONFIRMED.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn post_creation_session_and_final_generation_failures_are_uncertain() {
        for fail_at in [0, 1, 2] {
            let result = after_cloud_success(
                || {
                    for step in 0..3 {
                        if step == fail_at {
                            return Err("stale or sync failure");
                        }
                    }
                    Ok(7)
                },
                || "uncertain: reconcile before creating again".into(),
            );
            assert_eq!(
                result.unwrap_err(),
                "uncertain: reconcile before creating again"
            );
        }
        assert_eq!(
            after_cloud_success(|| Ok::<_, ()>(7), || panic!("success")),
            Ok(7)
        );
    }

    #[test]
    fn logout_after_save_preserves_row_and_vault() {
        let row = Cell::new(false);
        let secret = Cell::new(true);
        let result = save_connection(
            || {
                row.set(true);
                Ok(7)
            },
            || panic!("successful save needs no rollback lookup"),
            || {
                secret.set(false);
                Ok(())
            },
            || Err("stale".into()),
        );
        assert_eq!(result.unwrap_err(), SAVE_UNCONFIRMED);
        assert!(row.get());
        assert!(secret.get());
    }

    #[test]
    fn failed_or_unknown_save_with_possible_row_never_deletes_vault() {
        for lookup in [Ok(false), Err("DB unavailable".into())] {
            let result = save_connection(
                || Err::<(), _>("save failed".into()),
                || lookup,
                || panic!("must retain recoverable secret"),
                || panic!("save failed"),
            );
            assert_eq!(result.unwrap_err(), SAVE_UNCONFIRMED);
        }
    }

    #[test]
    fn confirmed_absence_cleans_vault_and_preserves_original_failure() {
        let secret = Cell::new(true);
        let result = save_connection(
            || Err::<(), _>("save failed".into()),
            || Ok(true),
            || {
                secret.set(false);
                Ok(())
            },
            || panic!("save failed"),
        );
        assert_eq!(result.unwrap_err(), "save failed");
        assert!(!secret.get());
    }

    #[test]
    fn cleanup_failure_is_explicit_and_does_not_claim_success() {
        let result = save_connection(
            || Err::<(), _>("save failed".into()),
            || Ok(true),
            || Err("vault unavailable".into()),
            || panic!("save failed"),
        );
        assert_eq!(result.unwrap_err(), CLEANUP_INCOMPLETE);
    }

    #[test]
    fn successful_save_returns_result_without_cleanup() {
        assert_eq!(
            save_connection(|| Ok(7), || panic!("saved"), || panic!("saved"), || Ok(())),
            Ok(7)
        );
    }
}
