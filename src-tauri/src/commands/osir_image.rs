//! Registered native image commands and the standard Images proxy admission hook.
use super::osir::OsirAccountState;
use crate::services::osir::image_binding::{self, AppliedImageDto, ImageContractDto};
use crate::store::AppState;
use tauri::State;

#[tauri::command]
pub async fn osir_image_get(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
) -> Result<image_binding::GlobalImageDto, String> {
    image_binding::global_get(app.config().identifier.clone(), &account.auth, &state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn osir_image_save(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
    request: image_binding::GlobalImageSave,
) -> Result<image_binding::GlobalImageDto, String> {
    match request {
        image_binding::GlobalImageSave::Saved {
            provider_id,
            model,
            generation,
        } => {
            let prepared = image_binding::prepare_selection(
                &account.auth,
                &state.db,
                &provider_id,
                &model,
                generation,
            )
            .await
            .map_err(|e| e.to_string())?;
            let _guard = state.proxy_service.lock_switch_for_app("codex").await;
            image_binding::commit_selection(&account.auth, &state.db, generation, prepared)
                .map_err(|e| e.to_string())?;
        }
        image_binding::GlobalImageSave::Custom {
            endpoint,
            model,
            api_key,
        } => {
            let _guard = state.proxy_service.lock_switch_for_app("codex").await;
            image_binding::save_custom(
                &state.db,
                app.config().identifier.clone(),
                endpoint,
                model,
                api_key,
            )
            .await
            .map_err(|e| e.to_string())?;
        }
    }
    image_binding::global_get(app.config().identifier.clone(), &account.auth, &state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn osir_image_auto_default(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
    generation: u64,
) -> Result<image_binding::GlobalImageDto, String> {
    let prepared = image_binding::prepare_auto_default(&account.auth, &state.db, generation).await;
    if let Ok(Some(prepared)) = prepared {
        let _guard = state.proxy_service.lock_switch_for_app("codex").await;
        // User saves while discovery is in flight always win.
        if !image_binding::configured(&state.db).map_err(|e| e.to_string())? {
            image_binding::commit_selection(&account.auth, &state.db, generation, prepared)
                .map_err(|e| e.to_string())?;
        }
    }
    let mut dto =
        image_binding::global_get(app.config().identifier.clone(), &account.auth, &state.db)
            .await
            .map_err(|e| e.to_string())?;
    if dto.selection.is_none() {
        dto.status = "unavailable";
        dto.reason =
            Some("未找到有效且分组已开启生图权限的已存 OpenAI 密钥，或账户权限读取失败，请重试或选择其他密钥");
    }
    Ok(dto)
}

#[tauri::command]
pub async fn osir_image_contract(
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
    provider_id: String,
    generation: u64,
) -> Result<ImageContractDto, String> {
    image_binding::preview(&account.auth, &state.db, &provider_id, generation)
        .await
        .map_err(|e| e.to_string())
}

/// Explicit local binding only: never creates a cloud key, switches the chat
/// provider, modifies live auth, or starts/stops the proxy.
#[tauri::command]
pub async fn osir_image_bind(
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
    provider_id: String,
    model: String,
    generation: u64,
) -> Result<AppliedImageDto, String> {
    let contract = image_binding::prepare_selection(
        &account.auth,
        &state.db,
        &provider_id,
        &model,
        generation,
    )
    .await
    .map_err(|e| e.to_string())?;
    let _guard = state.proxy_service.lock_switch_for_app("codex").await;
    image_binding::commit_selection(&account.auth, &state.db, generation, contract)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn osir_image_unbind(
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
    expected: AppliedImageDto,
    generation: u64,
) -> Result<(), String> {
    let _guard = state.proxy_service.lock_switch_for_app("codex").await;
    image_binding::unbind(&account.auth, &state.db, generation, &expected)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn osir_image_binding(state: State<'_, AppState>) -> Result<Option<AppliedImageDto>, String> {
    image_binding::current(&state.db).map_err(|e| e.to_string())
}

/// The standalone Images handler calls this BEFORE resolve_stack_target.
/// None retains ordinary routing. Err must be returned, never swallowed/fallback.
/// Do not call for /responses or /alpha/search.
pub(crate) async fn osir_image_proxy_target(
    state: &crate::proxy::server::ProxyState,
    endpoint: &str,
    body: &mut serde_json::Value,
    headers: &mut http::HeaderMap,
) -> Result<Option<crate::mode::stack::StackTarget>, crate::proxy::ProxyError> {
    use tauri::Manager;
    let failure = |_| {
        crate::proxy::ProxyError::InvalidRequest(
            "图片绑定失效或服务端能力不可用，请重新选择图片模型".into(),
        )
    };
    if !image_binding::configured(&state.db).map_err(failure)? {
        return Ok(None);
    }
    let app = state
        .app_handle
        .as_ref()
        .ok_or_else(|| failure(image_binding::ImageBindingError))?;
    if let Some(target) =
        image_binding::custom_target(&state.db, app.config().identifier.clone(), endpoint, body)
            .await
            .map_err(failure)?
    {
        headers.remove("x-openai-actor-authorization");
        headers.remove("x-osir-codex-image-model");
        return Ok(Some(target));
    }
    let account = app
        .try_state::<OsirAccountState>()
        .ok_or_else(|| failure(image_binding::ImageBindingError))?;
    let target = image_binding::resolve_request(&account.auth, &state.db, endpoint, body)
        .await
        .map_err(failure)?;
    if target.is_some() {
        // This contract attests standard Images, never Codex extension headers.
        headers.remove("x-openai-actor-authorization");
        headers.remove("x-osir-codex-image-model");
    }
    Ok(target)
}
