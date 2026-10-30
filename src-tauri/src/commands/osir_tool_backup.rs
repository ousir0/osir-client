//! Self-contained adapter: register this module; no services/mod edit required.
#[cfg(test)]
#[path = "../services/provider/runtime_acceptance_tests.rs"]
mod runtime_acceptance_tests;
#[path = "../services/osir/tool_backup.rs"]
mod service;
pub use service::{BackupError, BackupState, Preview, RestoreReport, Snapshot, Tool};
/// Native writers call this before a live projection under the existing tool
/// switch lock. Do not call worker(): reacquiring that lock would deadlock.
pub(crate) fn snapshot_before_osir_live_write(
    app: &crate::app_config::AppType,
    target: &crate::provider::Provider,
    previous: Option<&crate::provider::Provider>,
) -> Result<(), crate::error::AppError> {
    use crate::app_config::AppType;
    let managed =
        |p: &crate::provider::Provider| p.meta.as_ref().is_some_and(|m| m.osir_binding.is_some());
    if !managed(target) && !previous.is_some_and(managed) {
        return Ok(());
    }
    let tool = match app {
        AppType::Codex => Tool::Codex,
        AppType::Claude => Tool::Claude,
        AppType::OpenCode => Tool::Opencode,
        AppType::Gemini => Tool::Gemini,
        _ => return Ok(()),
    };
    service::ToolBackups::configured(tool)
        .before_write()
        .map(|_| ())
        .map_err(|_| {
            crate::error::AppError::Message(
                "工具配置快照未保存，本次未修改配置；请检查备份容量、目录权限或待恢复操作后重试"
                    .into(),
            )
        })
}
async fn worker<T: Send + 'static>(
    state: &crate::store::AppState,
    tool: Tool,
    f: impl FnOnce(service::ToolBackups) -> Result<T, BackupError> + Send + 'static,
) -> Result<T, BackupError> {
    // Match provider writes: switch lock first, then the service engine lock.
    // Move the owned guard into the worker so cancellation cannot release it
    // while a blocking restore is still publishing or rolling back files.
    let switch_guard = state.proxy_service.lock_switch_for_app(tool.id()).await;
    tokio::task::spawn_blocking(move || {
        let _switch_guard = switch_guard;
        f(service::ToolBackups::configured(tool))
    })
    .await
    .map_err(|_| BackupError::Storage)?
}
#[tauri::command]
pub async fn osir_tool_backups_list(
    state: tauri::State<'_, crate::store::AppState>,
    tool: Tool,
) -> Result<BackupState, BackupError> {
    worker(state.inner(), tool, |s| s.list()).await
}
#[tauri::command]
pub async fn osir_tool_backup_create(
    state: tauri::State<'_, crate::store::AppState>,
    tool: Tool,
) -> Result<Snapshot, BackupError> {
    worker(state.inner(), tool, |s| s.create()).await
}
#[tauri::command]
pub async fn osir_tool_backup_preview(
    state: tauri::State<'_, crate::store::AppState>,
    tool: Tool,
    snapshot_id: String,
) -> Result<Preview, BackupError> {
    worker(state.inner(), tool, move |s| s.preview(&snapshot_id)).await
}
#[tauri::command]
pub async fn osir_tool_backup_restore(
    state: tauri::State<'_, crate::store::AppState>,
    tool: Tool,
    preview_id: String,
    confirmed: bool,
) -> Result<RestoreReport, BackupError> {
    worker(state.inner(), tool, move |s| {
        s.restore(&preview_id, confirmed)
    })
    .await
}
#[tauri::command]
pub async fn osir_tool_backup_recover(
    state: tauri::State<'_, crate::store::AppState>,
    tool: Tool,
) -> Result<RestoreReport, BackupError> {
    worker(state.inner(), tool, |s| s.recover()).await
}
