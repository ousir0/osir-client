#![allow(non_snake_case)]

use serde_json::{json, Value};
use std::path::PathBuf;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::commands::sync_support::{
    post_sync_warning_from_result, run_post_import_sync, success_payload_with_warning,
};
use crate::database::backup::BackupEntry;
use crate::database::Database;
use crate::error::AppError;
use crate::services::provider::ProviderService;
use crate::services::skill::skill_state_write_guard;
use crate::services::sync_protocol::sync_mutex;
use crate::store::AppState;

async fn run_with_database_restore_lock<T, Start, Fut>(start_operation: Start) -> T
where
    Start: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _sync_guard = sync_mutex().lock().await;
    start_operation().await
}

// ─── File import/export ──────────────────────────────────────

/// 导出数据库为 SQL 备份
#[tauri::command]
pub async fn export_config_to_file(
    #[allow(non_snake_case)] filePath: String,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let target_path = PathBuf::from(&filePath);
        db.export_sql(&target_path)?;
        Ok::<_, AppError>(json!({
            "success": true,
            "message": "SQL exported successfully",
            "filePath": filePath
        }))
    })
    .await
    .map_err(|e| format!("导出配置失败: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

/// 从 SQL 备份导入数据库
#[tauri::command]
pub async fn import_config_from_file(
    #[allow(non_snake_case)] filePath: String,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let app_state_for_sync = state.inner().clone();
    let db = app_state_for_sync.db.clone();
    run_with_database_restore_lock(move || {
        tauri::async_runtime::spawn_blocking(move || {
            let path_buf = PathBuf::from(&filePath);
            let backup_id = {
                // SQL restore replaces the `skills` table. Exclude local Skill
                // mutations while the database image is being swapped.
                let _skill_state_guard = skill_state_write_guard();
                db.import_sql(&path_buf)?
            };
            let warning =
                post_sync_warning_from_result(Ok(run_post_import_sync(&app_state_for_sync)));
            if let Some(msg) = warning.as_ref() {
                log::warn!("[Import] post-import sync warning: {msg}");
            }
            Ok::<_, AppError>(success_payload_with_warning(backup_id, warning))
        })
    })
    .await
    .map_err(|e| format!("导入配置失败: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

#[tauri::command]
pub async fn sync_current_providers_live(state: State<'_, AppState>) -> Result<Value, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let app_state = AppState::new(db);
        ProviderService::sync_current_to_live(&app_state)?;
        Ok::<_, AppError>(json!({
            "success": true,
            "message": "Live configuration synchronized"
        }))
    })
    .await
    .map_err(|e| format!("同步当前供应商失败: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

// ─── File dialogs ────────────────────────────────────────────

/// 保存文件对话框
#[tauri::command]
pub async fn save_file_dialog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    #[allow(non_snake_case)] defaultName: String,
) -> Result<Option<String>, String> {
    let dialog = app.dialog();
    let result = dialog
        .file()
        .add_filter("SQL", &["sql"])
        .set_file_name(&defaultName)
        .blocking_save_file();

    Ok(result.map(|p| p.to_string()))
}

/// 打开文件对话框
#[tauri::command]
pub async fn open_file_dialog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Option<String>, String> {
    let dialog = app.dialog();
    let result = dialog
        .file()
        .add_filter("SQL", &["sql"])
        .blocking_pick_file();

    Ok(result.map(|p| p.to_string()))
}

/// 打开 ZIP 文件选择对话框
#[tauri::command]
pub async fn open_zip_file_dialog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Option<String>, String> {
    let dialog = app.dialog();
    let result = dialog
        .file()
        .add_filter("ZIP / Skill", &["zip", "skill"])
        .blocking_pick_file();

    Ok(result.map(|p| p.to_string()))
}

// ─── Database backup management ─────────────────────────────

/// Manually create a database backup
#[tauri::command]
pub async fn create_db_backup(state: State<'_, AppState>) -> Result<String, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || match db.backup_database_file()? {
        Some(path) => Ok(path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()),
        None => Err(AppError::Config(
            "Database file not found, backup skipped".to_string(),
        )),
    })
    .await
    .map_err(|e| format!("Backup failed: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

/// List all database backup files
#[tauri::command]
pub fn list_db_backups() -> Result<Vec<BackupEntry>, String> {
    Database::list_backups().map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreSyncPhase {
    LiveConfiguration,
    Prompts,
    ModelPricing,
    SettingsCache,
    RuntimeLogLevel,
    UsageCache,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreSyncStatus {
    Success,
    Failed,
    Skipped,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSyncStage {
    phase: RestoreSyncPhase,
    status: RestoreSyncStatus,
    // Fixed copy only: service errors can contain credentials or file contents.
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'static str>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbRestoreReport {
    database_restored: bool,
    safety_backup_id: String,
    post_import_sync: Vec<RestoreSyncStage>,
}

fn restore_with_sync_report<E>(
    restore: impl FnOnce() -> Result<String, E>,
    mut sync: impl FnMut(RestoreSyncPhase) -> Result<(), E>,
) -> Result<DbRestoreReport, E> {
    // A database failure is still a command error; no sync runs in that case.
    let safety_backup_id = restore()?;
    let mut stages = Vec::new();
    let mut interrupted = false;
    for phase in [
        RestoreSyncPhase::LiveConfiguration,
        RestoreSyncPhase::Prompts,
        RestoreSyncPhase::ModelPricing,
        RestoreSyncPhase::SettingsCache,
        RestoreSyncPhase::RuntimeLogLevel,
        RestoreSyncPhase::UsageCache,
    ] {
        let (status, message) = if interrupted {
            (
                RestoreSyncStatus::Skipped,
                Some("前一阶段异常中断，此阶段未执行。"),
            )
        } else {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sync(phase))) {
                Ok(Ok(())) => (RestoreSyncStatus::Success, None),
                Ok(Err(_)) => (
                    RestoreSyncStatus::Failed,
                    Some("此阶段未完整完成，请检查对应功能；无需重新恢复数据库。"),
                ),
                Err(_) => {
                    interrupted = true;
                    (
                        RestoreSyncStatus::Failed,
                        Some("此阶段异常中断，后续阶段未执行；数据库已恢复，请勿重复恢复。"),
                    )
                }
            }
        };
        stages.push(RestoreSyncStage {
            phase,
            status,
            message,
        });
    }
    Ok(DbRestoreReport {
        database_restored: true,
        safety_backup_id,
        post_import_sync: stages,
    })
}

/// Restore the database, then report each existing post-import sync stage.
#[tauri::command]
pub async fn restore_db_backup(
    state: State<'_, AppState>,
    filename: String,
) -> Result<DbRestoreReport, String> {
    let app_state_for_sync = state.inner().clone();
    let db = app_state_for_sync.db.clone();
    run_with_database_restore_lock(move || {
        tauri::async_runtime::spawn_blocking(move || {
            restore_with_sync_report(
                || {
                    let _skill_state_guard = skill_state_write_guard();
                    db.restore_from_backup(&filename)
                },
                // Keep the same service calls and order as run_post_import_sync.
                // Live configuration includes provider, MCP and Skills projection;
                // its aggregate result is not a per-tool verification report.
                |phase| match phase {
                    RestoreSyncPhase::LiveConfiguration => {
                        ProviderService::sync_current_to_live(&app_state_for_sync)
                    }
                    RestoreSyncPhase::Prompts => {
                        crate::services::PromptService::sync_all_to_live(&app_state_for_sync)
                    }
                    RestoreSyncPhase::ModelPricing => {
                        crate::services::model_pricing::sync_local_model_pricing(&db).map(|_| ())
                    }
                    RestoreSyncPhase::SettingsCache => crate::settings::reload_settings(),
                    RestoreSyncPhase::RuntimeLogLevel => match db.get_log_config() {
                        Ok(config) => {
                            log::set_max_level(config.to_level_filter());
                            Ok(())
                        }
                        Err(error) => {
                            log::set_max_level(log::LevelFilter::Info);
                            Err(error)
                        }
                    },
                    RestoreSyncPhase::UsageCache => {
                        app_state_for_sync.usage_cache.invalidate_all();
                        Ok(())
                    }
                },
            )
        })
    })
    .await
    .map_err(|e| format!("Restore failed: {e}"))?
    .map_err(|e: AppError| e.to_string())
}

/// Rename a database backup file
#[tauri::command]
pub fn rename_db_backup(
    #[allow(non_snake_case)] oldFilename: String,
    #[allow(non_snake_case)] newName: String,
) -> Result<String, String> {
    Database::rename_backup(&oldFilename, &newName).map_err(|e| e.to_string())
}

/// Delete a database backup file
#[tauri::command]
pub fn delete_db_backup(filename: String) -> Result<(), String> {
    Database::delete_backup(&filename).map_err(|e| e.to_string())
}

// ─── Backup storage overview ─────────────────────────────────

/// 列出本机各类备份的位置与大小
#[tauri::command]
pub async fn list_backup_locations(
) -> Result<Vec<crate::services::backup_storage::BackupLocation>, String> {
    tauri::async_runtime::spawn_blocking(crate::services::backup_storage::list_locations)
        .await
        .map_err(|e| e.to_string())
}

/// 删除一类备份（只认类别 id），返回释放的字节数
#[tauri::command]
pub async fn delete_backup_location(id: String) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::backup_storage::delete_location(&id)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// 在文件管理器中显示一类备份所在的目录
#[tauri::command]
pub async fn reveal_backup_location(id: String) -> Result<bool, String> {
    let dir = crate::services::backup_storage::location_dir(&id).map_err(|e| e.to_string())?;
    tauri_plugin_opener::reveal_item_in_dir(&dir).map_err(|e| format!("打开文件夹失败: {e}"))?;
    Ok(true)
}

#[cfg(test)]
mod restore_report_tests {
    use super::{restore_with_sync_report, RestoreSyncPhase, RestoreSyncStatus};

    #[test]
    fn reports_all_six_stages_and_safety_backup_after_restore() {
        let mut called = Vec::new();
        let report = restore_with_sync_report(
            || Ok::<_, &str>("safety-before-restore.db".to_owned()),
            |phase| {
                called.push(phase);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            called,
            vec![
                RestoreSyncPhase::LiveConfiguration,
                RestoreSyncPhase::Prompts,
                RestoreSyncPhase::ModelPricing,
                RestoreSyncPhase::SettingsCache,
                RestoreSyncPhase::RuntimeLogLevel,
                RestoreSyncPhase::UsageCache,
            ]
        );
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["databaseRestored"], true);
        assert_eq!(value["safetyBackupId"], "safety-before-restore.db");
        let stages = value["postImportSync"].as_array().unwrap();
        assert_eq!(stages.len(), 6);
        assert_eq!(stages[0]["phase"], "live_configuration");
        assert!(stages.iter().all(|stage| stage["status"] == "success"));
    }

    #[test]
    fn stage_errors_preserve_database_success_and_continue_without_exposing_errors() {
        let report = restore_with_sync_report(
            || Ok("safety.db".to_owned()),
            |phase| {
                if matches!(
                    phase,
                    RestoreSyncPhase::LiveConfiguration | RestoreSyncPhase::RuntimeLogLevel
                ) {
                    Err("Authorization: Bearer sk-secret; prompt body must stay native")
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert!(report.database_restored);
        assert_eq!(report.safety_backup_id, "safety.db");
        assert_eq!(report.post_import_sync[0].status, RestoreSyncStatus::Failed);
        assert_eq!(report.post_import_sync[4].status, RestoreSyncStatus::Failed);
        assert_eq!(
            report.post_import_sync[5].status,
            RestoreSyncStatus::Success
        );
        let wire = serde_json::to_string(&report).unwrap();
        assert!(!wire.contains("sk-secret"));
        assert!(!wire.contains("prompt body"));
    }

    #[test]
    fn database_failure_does_not_run_sync() {
        let mut called = false;
        let result = restore_with_sync_report(
            || Err("restore failed"),
            |_| {
                called = true;
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err(), "restore failed");
        assert!(!called);
    }

    #[test]
    fn interruption_keeps_safety_backup_and_marks_unexecuted_stages_skipped() {
        let mut called = 0;
        let report = restore_with_sync_report(
            || Ok::<_, &str>("safety.db".to_owned()),
            |phase| {
                called += 1;
                if phase == RestoreSyncPhase::Prompts {
                    panic!("simulated sync interruption");
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(report.database_restored);
        assert_eq!(report.safety_backup_id, "safety.db");
        assert_eq!(called, 2);
        assert_eq!(
            report.post_import_sync[0].status,
            RestoreSyncStatus::Success
        );
        assert_eq!(report.post_import_sync[1].status, RestoreSyncStatus::Failed);
        assert!(report.post_import_sync[2..]
            .iter()
            .all(|stage| stage.status == RestoreSyncStatus::Skipped));
    }
}

#[cfg(test)]
mod tests {
    use super::run_with_database_restore_lock;
    use crate::services::sync_protocol::sync_mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn manual_restore_starts_blocking_work_after_global_lock_acquisition() {
        let guard = sync_mutex().lock().await;
        let entered = Arc::new(AtomicBool::new(false));
        let entered_in_task = Arc::clone(&entered);
        let restore = run_with_database_restore_lock(move || {
            tokio::task::spawn_blocking(move || {
                entered_in_task.store(true, Ordering::SeqCst);
            })
        });
        tokio::pin!(restore);

        assert!(
            tokio::time::timeout(Duration::from_millis(40), restore.as_mut())
                .await
                .is_err(),
            "restore must wait while another sync operation holds the global lock"
        );
        assert!(!entered.load(Ordering::SeqCst));

        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), restore.as_mut())
            .await
            .expect("restore should start after lock release")
            .expect("blocking restore task should complete");
        assert!(entered.load(Ordering::SeqCst));
    }
}
