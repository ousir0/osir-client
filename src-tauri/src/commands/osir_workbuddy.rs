use super::{check_connected, OsirAccountState, STALE};
use crate::services::osir::{gateway, workbuddy};
use tauri::State;

#[tauri::command]
pub async fn osir_workbuddy_config() -> Result<workbuddy::ConfigView, String> {
    tokio::task::spawn_blocking(workbuddy::read)
        .await
        .map_err(|_| "WorkBuddy 配置读取未完成，请重试".to_string())?
}

/// Explicit native configuration application, independent of provider switching.
/// IPC carries a key ID and model IDs; secrets and the destination stay native.
#[tauri::command]
pub async fn osir_workbuddy_save_models(
    state: State<'_, OsirAccountState>,
    request: workbuddy::SaveRequest,
) -> Result<workbuddy::ConfigView, String> {
    request.validate()?;
    let _operation = state
        .connection_operation
        .try_lock()
        .map_err(|_| "连接操作进行中，请稍后重试")?;
    let account_id = check_connected(&state.auth, request.generation)?;
    let key = state
        .auth
        .read_key(request.key_id, request.generation)
        .await
        .map_err(|e| e.to_string())?;
    if key.generation != request.generation || key.data.id != request.key_id {
        return Err(STALE.into());
    }
    check_connected(&state.auth, request.generation)?;
    let group_id = key.data.group_id;
    let secret = key.data.into_active_secret().map_err(|e| e.to_string())?;
    let models = gateway::models_for_account_key(
        &secret,
        &state.auth.account_origin(),
        &account_id,
        request.key_id,
        group_id,
        request.generation,
        &state.auth.request_budget(),
    )
    .await?;
    if request
        .model_ids
        .iter()
        .any(|id| !models.iter().any(|model| &model.id == id))
    {
        return Err("所选模型已不可用，请重新读取模型目录".into());
    }
    let auth = state.auth.clone();
    tokio::task::spawn_blocking(move || {
        workbuddy::save(
            &workbuddy::config_path(),
            &request,
            &account_id,
            &secret,
            || check_connected(&auth, request.generation).map(|_| ()),
        )
    })
    .await
    .map_err(|_| "WorkBuddy 配置保存未完成，请重新读取状态".to_string())?
}
