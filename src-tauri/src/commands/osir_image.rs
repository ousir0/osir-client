//! Registered native image commands and the standard Images proxy admission hook.
use super::osir::OsirAccountState;
use crate::services::osir::image_binding::{self, AppliedImageDto, ImageContractDto};
use crate::store::AppState;
use tauri::State;

// Selection commit and local projection are separate operations. Startup retries
// the latter. Never log its raw error, which may contain paths or config values.
pub(crate) async fn resync_committed_image_locked(state: &AppState) -> bool {
    if crate::mode::controller::resync_route_locked(state, &crate::app_config::AppType::Codex)
        .await
        .is_err()
    {
        log::warn!("[OSIR Images] Image selection committed; Codex configuration sync deferred");
        return false;
    }
    true
}

pub(crate) fn committed_image_reply(
    refreshed: Result<image_binding::GlobalImageDto, image_binding::ImageBindingError>,
    selection: image_binding::GlobalImageSelectionDto,
    synced: bool,
) -> image_binding::GlobalImageDto {
    let mut dto = refreshed.unwrap_or_else(|_| image_binding::GlobalImageDto {
        status: "unverified",
        reason: Some("图片配置已保存，状态暂时无法读取，请刷新后重试"),
        selection: Some(selection),
        candidates: vec![],
    });
    if !synced {
        dto.reason =
            Some("图片配置已保存，Codex 设置暂未更新。请重新打开 OSIR 客户端后再打开 Codex");
    }
    dto
}

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
    let (selection, synced) = match request {
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
            let applied =
                image_binding::commit_selection(&account.auth, &state.db, generation, prepared)
                    .map_err(|e| e.to_string())?;
            (
                applied.summary(),
                resync_committed_image_locked(&state).await,
            )
        }
        image_binding::GlobalImageSave::Custom {
            endpoint,
            model,
            api_key,
        } => {
            let _guard = state.proxy_service.lock_switch_for_app("codex").await;
            let selection = image_binding::save_custom(
                &state.db,
                app.config().identifier.clone(),
                endpoint,
                model,
                api_key,
            )
            .await
            .map_err(|e| e.to_string())?;
            (selection, resync_committed_image_locked(&state).await)
        }
    };
    Ok(committed_image_reply(
        image_binding::global_get(app.config().identifier.clone(), &account.auth, &state.db).await,
        selection,
        synced,
    ))
}

#[tauri::command]
pub async fn osir_image_auto_default(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
    generation: u64,
) -> Result<image_binding::GlobalImageDto, String> {
    let prepared = image_binding::prepare_auto_default(&account.auth, &state.db, generation).await;
    let mut committed = None;
    if let Ok(Some(prepared)) = prepared {
        let _guard = state.proxy_service.lock_switch_for_app("codex").await;
        // User saves while discovery is in flight always win.
        if !image_binding::configured(&state.db).map_err(|e| e.to_string())? {
            let applied =
                image_binding::commit_selection(&account.auth, &state.db, generation, prepared)
                    .map_err(|e| e.to_string())?;
            committed = Some((
                applied.summary(),
                resync_committed_image_locked(&state).await,
            ));
        }
    }
    let result =
        image_binding::global_get(app.config().identifier.clone(), &account.auth, &state.db).await;
    let mut dto = match committed {
        Some((selection, synced)) => committed_image_reply(result, selection, synced),
        None => result.map_err(|e| e.to_string())?,
    };
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
    let applied = image_binding::commit_selection(&account.auth, &state.db, generation, contract)
        .map_err(|e| e.to_string())?;
    resync_committed_image_locked(&state).await;
    Ok(applied)
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
        .map_err(|e| e.to_string())?;
    resync_committed_image_locked(&state).await;
    Ok(())
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
        if image_binding::has_local_image_marker(headers) {
            return Err(failure(image_binding::ImageBindingError));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_image_reply_warns_even_when_refreshed_status_is_ready() {
        let selection = || image_binding::GlobalImageSelectionDto {
            source: "custom",
            provider_id: None,
            endpoint: "https://images.example/v1".into(),
            model: "fixture-image".into(),
            version: "fixture".into(),
            has_key: true,
        };
        let dto = committed_image_reply(
            Ok(image_binding::GlobalImageDto {
                status: "ready",
                reason: None,
                selection: Some(selection()),
                candidates: vec![],
            }),
            selection(),
            false,
        );
        assert_eq!(dto.status, "ready");
        let json = serde_json::to_value(dto).unwrap();
        assert!(json["reason"].as_str().unwrap().contains("暂未更新"));
        assert_eq!(json["selection"]["model"], "fixture-image");
    }
}
