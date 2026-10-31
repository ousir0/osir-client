use crate::services::osir::{
    auth::{AuthConfig, AuthManager},
    client::AccountResource,
    models::{DesktopAuthCapability, LogoutReport, SessionStatus},
    resources,
    sync::{Config, QueryClass, ReadPolicy, RequestKey, Session, SyncCoordinator, SystemClock},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

const SESSION_EVENT: &str = "osir-account-session-changed";
const STALE: &str = "账户会话已改变，请重新同步";
const STARTUP_SESSION_RECOVERY_TIMEOUT: Duration = Duration::from_secs(35);

fn connection_tool(tool: &str) -> Result<crate::app_config::AppType, String> {
    use crate::app_config::AppType;
    match tool {
        "codex" => Ok(AppType::Codex),
        "claude" => Ok(AppType::Claude),
        "claude-desktop" => Ok(AppType::ClaudeDesktop),
        "gemini" => Ok(AppType::Gemini),
        "opencode" => Ok(AppType::OpenCode),
        _ => Err("此工具不支持账户连接".into()),
    }
}

#[tauri::command]
pub async fn osir_get_connection_mode(
    state: State<'_, crate::store::AppState>,
    tool: String,
) -> Result<crate::mode::controller::ConnectionModeView, String> {
    crate::mode::controller::connection_mode(state.inner(), &connection_tool(&tool)?).await
}

#[tauri::command]
pub async fn osir_set_connection_mode(
    state: State<'_, crate::store::AppState>,
    tool: String,
    mode: String,
    provider_id: Option<String>,
    aggregation: Option<bool>,
) -> Result<crate::mode::controller::ConnectionModeView, String> {
    crate::mode::controller::set_connection_mode(
        state.inner(),
        &connection_tool(&tool)?,
        &mode,
        provider_id.as_deref(),
        aggregation.unwrap_or(true),
    )
    .await
}

#[path = "osir_mutation.rs"]
mod mutation;
#[path = "osir_recovery.rs"]
mod recovery;
#[path = "osir_workbuddy.rs"]
mod workbuddy;
pub use workbuddy::*;

pub struct OsirAccountState {
    pub auth: Arc<AuthManager>,
    pub sync: SyncCoordinator,
    binding: Mutex<Option<(u64, Session)>>,
    connection_operation: tokio::sync::Mutex<()>,
}
impl OsirAccountState {
    pub fn new(identity: &str) -> Result<Self, String> {
        Ok(Self {
            auth: Arc::new(
                AuthManager::new(AuthConfig::default(), identity).map_err(|e| e.to_string())?,
            ),
            sync: SyncCoordinator::new(Config::default(), Arc::new(SystemClock::default()))
                .map_err(|_| "账户同步服务初始化失败")?,
            binding: Mutex::new(None),
            connection_operation: tokio::sync::Mutex::new(()),
        })
    }
    pub fn new_persistent(identity: &str, directory: &std::path::Path) -> Result<Self, String> {
        let mut state = Self::new(identity)?;
        state.auth = Arc::new(
            AuthManager::new_persistent(AuthConfig::default(), identity, directory)
                .map_err(|e| e.to_string())?,
        );
        Ok(state)
    }
    /// Startup never waits for HTTP or Keychain on the UI/setup thread.
    pub fn restore_in_background(app: tauri::AppHandle) {
        tauri::async_runtime::spawn(async move {
            use tauri::Manager;
            let state = app.state::<OsirAccountState>();
            if !state.auth.enabled() {
                return;
            }
            match tokio::time::timeout(
                STARTUP_SESSION_RECOVERY_TIMEOUT,
                state.auth.restore_startup(),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(_)) => log::warn!("OSIR account startup recovery incomplete"),
                Err(_) => log::warn!("OSIR account startup recovery timed out"),
            }
            let status = state.reconcile();
            if matches!(
                status.recovery,
                crate::services::osir::models::SessionRecovery::UnlockRequired
                    | crate::services::osir::models::SessionRecovery::Retryable
                    | crate::services::osir::models::SessionRecovery::Blocked
            ) {
                crate::client_telemetry::record_credential_failure(app.clone());
            }
            emit_status(&app, &status);
        });
    }
    /// Independent of account authorization; saved native grants survive logout.
    pub fn recover_credentials_in_background(app: tauri::AppHandle) {
        tauri::async_runtime::spawn_blocking(move || {
            let result = (|| -> Result<(), String> {
                let _operation = recovery::lock()?;
                let directory = app.path().app_data_dir().map_err(|_| "恢复日志不可用")?;
                let journal = recovery::Journal::new(&directory);
                let vault =
                    crate::services::osir::managed::vault().map_err(|_| "凭据恢复不可用")?;
                let state = app.state::<crate::store::AppState>();
                recovery::recover_database(&journal, &state.db, &vault)
            })();
            if result.is_err() {
                log::warn!("OSIR credential recovery incomplete; journal retained");
            }
            let state = app.state::<crate::store::AppState>();
            let failed = super::osir_credential_recovery::recover_connections_at_startup(&state.db);
            if failed || result.is_err() {
                crate::client_telemetry::record_credential_failure(app.clone());
            }
        });
    }
    fn clear_locked(&self, binding: &mut Option<(u64, Session)>) {
        if let Some((_, session)) = binding.take() {
            let _ = self.sync.logout(&session);
        }
    }
    pub(super) fn session(&self, generation: u64) -> Result<Session, String> {
        let mut binding = self.binding.lock().unwrap();
        let status = self.auth.status();
        if !status.connected || status.generation != generation {
            // Stale IPC input cannot invalidate the current binding.
            return Err(STALE.into());
        }
        if let Some((bound, session)) = binding.as_ref() {
            if *bound == generation {
                return Ok(session.clone());
            }
        }
        self.clear_locked(&mut binding);
        let account = status.profile.ok_or(STALE)?.id.to_string();
        let session = self
            .sync
            .start_session(&self.auth.account_origin(), &account)
            .map_err(|_| STALE)?;
        *binding = Some((generation, session.clone()));
        Ok(session)
    }
    pub(super) fn reconcile(&self) -> SessionStatus {
        let mut binding = self.binding.lock().unwrap();
        let status = self.auth.status();
        if !status.connected
            || binding
                .as_ref()
                .is_some_and(|(g, _)| *g != status.generation)
        {
            self.clear_locked(&mut binding);
        }
        status
    }
}
// Native cache freshness follows docs/OSIR-API-INTEGRATION.md. Visibility
// scheduling and announcement polling jitter belong to the caller.
fn resource_ttl(resource: AccountResource) -> Duration {
    Duration::from_secs(match resource {
        AccountResource::Profile => 120,
        AccountResource::Keys | AccountResource::Groups | AccountResource::GroupRates => 300,
        AccountResource::Subscriptions | AccountResource::Usage | AccountResource::UsageStats => {
            180
        }
        AccountResource::UsageSnapshot => 60,
        AccountResource::Announcements => 60,
    })
}
fn emit_status(app: &tauri::AppHandle, status: &SessionStatus) {
    crate::client_telemetry::session_changed(app);
    let _ = app.emit(SESSION_EVENT, status);
}

#[tauri::command]
pub async fn osir_desktop_auth_capability(
    state: State<'_, OsirAccountState>,
) -> Result<DesktopAuthCapability, String> {
    Ok(state.auth.remote_capability().await)
}
#[tauri::command]
pub async fn osir_account_session(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
    retry_saved: Option<bool>,
    generation: Option<u64>,
) -> Result<SessionStatus, String> {
    if retry_saved.unwrap_or(false) {
        let expected = generation.ok_or(STALE)?;
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            state.auth.recover_existing_for_generation(expected),
        )
        .await;
        let status = state.reconcile();
        emit_status(&app, &status);
        if status.generation != expected {
            return Err(STALE.into());
        }
        // Temporary failures are represented by safe recovery metadata, never
        // an invitation to start a new authorization or expose transport errors.
        if !matches!(result, Ok(Ok(_)))
            && matches!(
                status.recovery,
                crate::services::osir::models::SessionRecovery::Idle
            )
        {
            return Err("账户会话恢复未完成".into());
        }
        return Ok(status);
    }
    Ok(state.reconcile())
}
#[tauri::command]
pub async fn osir_account_authorize(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
    allow_key_creation: Option<bool>,
    generation: Option<u64>,
) -> Result<SessionStatus, String> {
    let status = state.reconcile();
    if generation.is_some_and(|expected| expected != status.generation) {
        return Err(STALE.into());
    }
    if status.connected
        || !matches!(
            status.recovery,
            crate::services::osir::models::SessionRecovery::Idle
        )
    {
        return Err("已有账户状态尚未恢复，请先重试恢复登录或退出账户".into());
    }
    let capability = state.auth.remote_capability().await;
    if !matches!(
        capability.status,
        crate::services::osir::models::DesktopAuthStatus::Available
    ) {
        return Err(capability.reason);
    }
    if state.auth.status().generation != status.generation {
        return Err(STALE.into());
    }
    let attempt = state
        .auth
        .start_for_generation(allow_key_creation.unwrap_or(false), Some(status.generation))
        .await
        .map_err(|e| e.to_string())?;
    let generation = attempt.generation();
    state.reconcile();
    let result = async {
        app.opener()
            .open_url(attempt.browser_url(), None::<String>)
            .map_err(|_| "无法打开系统浏览器".to_string())?;
        let grant = attempt.wait().await.map_err(|e| e.to_string())?;
        state.auth.complete(grant).await.map_err(|e| e.to_string())
    }
    .await;
    if result.is_err() {
        state.auth.cancel_if_generation(generation);
    }
    let status = state.reconcile();
    emit_status(&app, &status);
    result?;
    if status.generation != generation || !status.connected {
        return Err(STALE.into());
    }
    // complete() has exchanged the one-time code, verified the account and
    // committed its credentials. Recheck when the UI task runs: logout or a
    // replacement attempt must not let an old callback bring the app forward.
    let foreground_app = app.clone();
    if app
        .run_on_main_thread(move || {
            let state = foreground_app.state::<OsirAccountState>();
            let current = state.reconcile();
            if current.connected && current.generation == generation {
                crate::tray::show_main_window(&foreground_app);
                // Existing observers receive safe session metadata; a rebuilt
                // lightweight window also reads osir_account_session on mount.
                emit_status(&foreground_app, &current);
            }
        })
        .is_err()
    {
        // Window activation failure cannot undo a successfully stored session.
        log::warn!("OSIR account signed in; main window activation could not be scheduled");
    }
    Ok(status)
}
#[tauri::command]
pub async fn osir_account_cancel_authorization(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
) -> Result<SessionStatus, String> {
    state.auth.cancel();
    let status = state.reconcile();
    emit_status(&app, &status);
    Ok(status)
}
#[tauri::command]
pub fn osir_account_abandon_saved_login(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
    generation: u64,
) -> Result<SessionStatus, String> {
    let status = {
        let mut binding = state.binding.lock().unwrap();
        let status = state
            .auth
            .abandon_saved_login(generation)
            .map_err(|error| error.to_string())?;
        state.clear_locked(&mut binding);
        status
    };
    emit_status(&app, &status);
    Ok(status)
}
#[tauri::command]
pub async fn osir_account_logout(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
) -> Result<LogoutReport, String> {
    let ticket = {
        let mut binding = state.binding.lock().unwrap();
        let ticket = state.auth.begin_logout();
        state.clear_locked(&mut binding);
        ticket
    };
    emit_status(&app, &state.auth.status());
    Ok(state.auth.finish_logout(ticket).await)
}
#[derive(Serialize)]
pub struct AnnouncementReadReply {
    generation: u64,
}

#[tauri::command]
pub async fn osir_announcement_mark_read(
    state: State<'_, OsirAccountState>,
    id: i64,
    generation: u64,
) -> Result<AnnouncementReadReply, String> {
    if id <= 0 {
        return Err("公告标识无效".into());
    }
    let session = state.session(generation)?;
    let reply = state
        .auth
        .mark_announcement_read_for_generation(id, generation)
        .await
        .map_err(|e| e.to_string())?;
    let status = state.auth.status();
    if reply.generation != generation || status.generation != generation || !status.connected {
        return Err(STALE.into());
    }
    // invalidate checks the opaque sync session; a race cannot clear a replacement.
    state
        .sync
        .invalidate(&session, Some("announcements"))
        .map_err(|_| STALE)?;
    let status = state.auth.status();
    if status.generation != generation || !status.connected {
        return Err(STALE.into());
    }
    Ok(AnnouncementReadReply { generation })
}

#[derive(Serialize)]
pub struct AccountResourceReply {
    data: Value,
    generation: u64,
    fetched_at: String,
}

#[tauri::command]
pub async fn osir_account_resource(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
    resource: AccountResource,
    parameters: Value,
    generation: u64,
    refresh: Option<bool>,
) -> Result<AccountResourceReply, String> {
    let query =
        resources::validate_query(resource, parameters.clone()).map_err(|e| e.to_string())?;
    // Also bounds IPC query size before any request or cache insertion.
    let name = serde_json::to_value(resource).map_err(|_| "账户资源无效")?;
    let key = RequestKey::new(name.as_str().ok_or("账户资源无效")?, parameters)
        .map_err(|_| "账户查询参数过大")?;
    let session = state.session(generation)?;
    let auth = state.auth.clone();
    let policy = ReadPolicy {
        ttl: resource_ttl(resource),
        class: if matches!(
            resource,
            AccountResource::UsageSnapshot | AccountResource::UsageStats
        ) {
            QueryClass::Heavy
        } else {
            QueryClass::Standard
        },
    };
    let fetch = move || async move {
        let status = auth.status();
        if status.generation != generation || !status.connected {
            return Err(STALE.to_string());
        }
        let reply = auth
            .read_account::<Value>(resource, &query)
            .await
            .map_err(|e| e.to_string())?;
        if reply.generation != generation {
            return Err(STALE.to_string());
        }
        // Only the endpoint allowlist projection ever enters shared cache.
        let data = resources::sanitize(resource, reply.data).map_err(|e| e.to_string())?;
        let status = auth.status();
        if status.generation != generation || !status.connected {
            return Err(STALE.to_string());
        }
        Ok::<_, String>(serde_json::json!({
            "data": data,
            "fetched_at": chrono::Utc::now().to_rfc3339()
        }))
    };
    let result = if refresh.unwrap_or(false) {
        state.sync.read_fresh(&session, key, policy, fetch).await
    } else {
        state.sync.read(&session, key, policy, fetch).await
    };
    let status = state.reconcile();
    if status.generation != generation || !status.connected {
        emit_status(&app, &status);
        return Err(STALE.into());
    }
    let cached = result.map_err(|_| "账户资源同步失败，请重试".to_string())?;
    Ok(AccountResourceReply {
        data: cached.get("data").cloned().ok_or("账户缓存无效")?,
        generation,
        fetched_at: cached
            .get("fetched_at")
            .and_then(Value::as_str)
            .ok_or("账户缓存无效")?
            .into(),
    })
}

/// Bounded user-side export. Reuses the authenticated read cache and safe DTO;
/// IPC never supplies file contents, a path, or an arbitrary account endpoint.
#[tauri::command]
pub async fn osir_export_usage_page(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
    parameters: Value,
    generation: u64,
) -> Result<Option<usize>, String> {
    use std::io::Write;
    use tauri_plugin_dialog::DialogExt;
    let reply = osir_account_resource(
        app.clone(),
        state.clone(),
        AccountResource::Usage,
        parameters,
        generation,
        None,
    )
    .await?;
    let (csv, rows) = usage_export::encode(reply.data)?;
    check_connected(&state.auth, generation)?;
    let destination = tokio::task::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("CSV", &["csv"])
            .set_file_name(format!(
                "osir-usage-{}.csv",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ))
            .blocking_save_file()
    })
    .await
    .map_err(|_| "导出窗口未能打开，请重试".to_string())?;
    let Some(destination) = destination else {
        return Ok(None);
    };
    let destination = destination
        .into_path()
        .map_err(|_| "保存位置不可用，请选择本地目录")?;
    // A dialog may remain open across logout/account switching. Do not write
    // the previous account's data after that switch.
    check_connected(&state.auth, generation)?;
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(destination)
            .map_err(|_| "文件无法创建，请选择可写目录和未使用的文件名".to_string())?;
        file.write_all(csv.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| "导出未完成，请检查所选文件并重新导出".to_string())?;
        Ok(Some(rows))
    })
    .await
    .map_err(|_| "导出未完成，请重试".to_string())?
}

#[path = "osir_usage_export.rs"]
mod usage_export;

#[tauri::command]
pub async fn osir_codex_desktop_detection(
    state: State<'_, super::osir_installer::OsirInstallerState>,
) -> Result<crate::services::osir::codex_desktop::DesktopDetection, String> {
    #[cfg(target_os = "windows")]
    {
        let _ = state;
        return super::osir_tools::windows::desktop_detection().await;
    }
    #[cfg(target_os = "macos")]
    let installer = state.manager()?.clone();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = state;
    #[cfg(not(target_os = "windows"))]
    tokio::task::spawn_blocking(move || {
        let mut detection = crate::services::osir::codex_desktop::detect();
        #[cfg(target_os = "macos")]
        {
            detection.recovery_targets = installer.recovery_targets().unwrap_or_default();
        }
        detection
    })
    .await
    .map_err(|_| "Desktop 安装状态检测未完成，请重试".into())
}

fn check_connected(auth: &AuthManager, generation: u64) -> Result<String, String> {
    let status = auth.status();
    if !status.connected || status.generation != generation {
        return Err(STALE.into());
    }
    Ok(status.profile.ok_or(STALE)?.id.to_string())
}

/// This cloud mutation never applies local tool configuration or retries.
#[tauri::command]
pub async fn osir_create_key(
    state: State<'_, OsirAccountState>,
    request: crate::services::osir::client::CreateKeyRequest,
    generation: u64,
) -> Result<crate::services::osir::account::KeyMetadata, String> {
    let _operation = state
        .connection_operation
        .try_lock()
        .map_err(|_| "连接操作进行中，请稍后重试")?;
    check_connected(&state.auth, generation)?;
    let created = state
        .auth
        .create_key(request, generation)
        .await
        .map_err(|e| e.to_string())?;
    mutation::after_cloud_success(
        || {
            let session = state.session(generation)?;
            state
                .sync
                .invalidate(&session, None)
                .map_err(|_| STALE.to_string())?;
            check_connected(&state.auth, generation)?;
            Ok::<_, String>(created.data.metadata())
        },
        || crate::services::osir::client::CreateKeyError::Uncertain.to_string(),
    )
}

#[derive(Serialize)]
pub struct KeyModelsReply {
    generation: u64,
    models: Vec<crate::services::model_fetch::FetchedModel>,
}

/// A saved tool grant is independent of the browser account session. Only the
/// database binding may select the native credential or fixed gateway.
#[tauri::command]
pub async fn osir_provider_models(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
    tool: String,
    provider_id: String,
) -> Result<Vec<crate::services::model_fetch::FetchedModel>, String> {
    use crate::services::osir::managed;
    let app_type = match tool.as_str() {
        "codex" => crate::app_config::AppType::Codex,
        "claude" => crate::app_config::AppType::Claude,
        "claude-desktop" => crate::app_config::AppType::ClaudeDesktop,
        "opencode" => crate::app_config::AppType::OpenCode,
        "gemini" => crate::app_config::AppType::Gemini,
        _ => return Err("不支持此工具".into()),
    };
    let (secret, binding) = tokio::task::spawn_blocking(move || {
        let application = app.state::<crate::store::AppState>();
        let providers = application
            .db
            .get_all_providers(app_type.as_str())
            .map_err(|_| "连接读取失败".to_string())?;
        let provider = providers.get(&provider_id).ok_or("连接不存在")?;
        managed::validate_managed_provider_shape(&app_type, provider)
            .map_err(|_| "连接配置无效".to_string())?;
        let binding = provider
            .meta
            .as_ref()
            .and_then(|m| m.osir_binding.as_ref())
            .ok_or("此连接不是账户托管连接")?;
        let secret = managed::vault()
            .map_err(|_| "凭据库不可用".to_string())?
            .load(&binding.reference, &binding.binding)
            .map_err(|_| "连接凭据不可用".to_string())?
            .ok_or_else(|| "连接凭据不可用".to_string())?;
        Ok::<_, String>((secret, binding.clone()))
    })
    .await
    .map_err(|_| "连接读取未完成".to_string())??;
    crate::services::osir::gateway::models_for_binding(
        &secret,
        &binding,
        &state.auth.request_budget(),
    )
    .await
}

/// Fetches only the fixed gateway model catalog. Key bytes never enter IPC/cache.
#[tauri::command]
pub async fn osir_account_key_models(
    state: State<'_, OsirAccountState>,
    key_id: i64,
    generation: u64,
) -> Result<KeyModelsReply, String> {
    let _operation = state
        .connection_operation
        .try_lock()
        .map_err(|_| "连接操作进行中，请稍后重试")?;
    let account_id = check_connected(&state.auth, generation)?;
    let key = state
        .auth
        .read_key(key_id, generation)
        .await
        .map_err(|e| e.to_string())?;
    if key.generation != generation || key.data.id != key_id {
        return Err(STALE.into());
    }
    check_connected(&state.auth, generation)?;
    let group_id = key.data.group_id;
    let secret = key.data.into_active_secret().map_err(|e| e.to_string())?;
    let account_origin = state.auth.account_origin();
    let models = crate::services::osir::gateway::models_for_account_key(
        &secret,
        &account_origin,
        &account_id,
        key_id,
        group_id,
        generation,
        &state.auth.request_budget(),
    )
    .await?;
    check_connected(&state.auth, generation)?;
    Ok(KeyModelsReply { generation, models })
}

/// Save only. Applying the connection remains a separate provider-controller action.
#[tauri::command]
pub async fn osir_connect_key(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
    request: crate::services::osir::connection::ConnectionRequest,
) -> Result<crate::provider::Provider, String> {
    use crate::services::osir::{
        connection,
        key_vault::{KeyBinding, KeyTool, GATEWAY_ORIGIN},
        managed,
    };
    let app_type = request.validate()?;
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
    let id = uuid::Uuid::new_v4().to_string();
    let binding = KeyBinding {
        account_origin: state.auth.account_origin(),
        account_id,
        key_id: key.data.id,
        group_id: key.data.group_id,
        provider_id: id.clone(),
        gateway_origin: GATEWAY_ORIGIN.into(),
        tool: match app_type {
            crate::app_config::AppType::Codex => KeyTool::Codex,
            crate::app_config::AppType::Claude => KeyTool::ClaudeCode,
            crate::app_config::AppType::ClaudeDesktop => KeyTool::ClaudeDesktop,
            crate::app_config::AppType::OpenCode => KeyTool::OpenCode,
            crate::app_config::AppType::Gemini => KeyTool::GeminiCli,
            _ => return Err("不支持此工具".into()),
        },
    };
    let secret = key.data.into_active_secret().map_err(|e| e.to_string())?;
    let models = crate::services::osir::gateway::models_for_account_key(
        &secret,
        &binding.account_origin,
        &binding.account_id,
        binding.key_id,
        binding.group_id,
        request.generation,
        &state.auth.request_budget(),
    )
    .await?;
    request.validate_catalog(&models)?;
    check_connected(&state.auth, request.generation)?;
    let mut provider = connection::provider_row(&request, id)?;
    let auth = state.auth.clone();
    let generation = request.generation;
    tokio::task::spawn_blocking(move || {
        check_connected(&auth, generation)?;
        let vault = managed::vault().map_err(|e| e.to_string())?;
        let _operation = recovery::lock()?;
        let journal = recovery::Journal::new(
            &app.path()
                .app_data_dir()
                .map_err(|_| "恢复日志不可用".to_string())?,
        );
        let application = app.state::<crate::store::AppState>();
        recovery::recover_database(&journal, &application.db, &vault)?;
        journal.ensure_clear()?;
        let prepared = vault
            .prepare_store(&binding, &secret)
            .map_err(|e| e.to_string())?;
        journal.record(&prepared, recovery::Phase::Intent)?;
        let stored = match vault.store_prepared(&prepared, &secret) {
            Ok(stored) => stored,
            Err(_) => {
                // A backend error may follow a successful write. Recovery uses
                // the already-durable reference, never assumes no secret exists.
                journal.record(&prepared, recovery::Phase::CleanupPending)?;
                recovery::recover_database(&journal, &application.db, &vault)?;
                return Err("连接凭据保存失败，请检查恢复状态后重试".into());
            }
        };
        // If this fails, the durable intent still owns the exact vault reference.
        journal.record(&stored, recovery::Phase::Stored)?;
        provider
            .meta
            .get_or_insert_with(Default::default)
            .osir_binding = Some(crate::provider::ManagedBinding {
            source: crate::provider::AuthBindingSource::ManagedAccount,
            binding: stored.binding.clone(),
            reference: stored.reference.clone(),
            masked_key: stored.masked_key.clone(),
        });
        let application = app.state::<crate::store::AppState>();
        mutation::save_connection(
            || {
                check_connected(&auth, generation)?;
                managed::validate_provider(&app_type, &provider).map_err(|e| e.to_string())?;
                crate::services::ProviderService::add_without_switching(
                    application.inner(),
                    app_type.clone(),
                    provider.clone(),
                )
                .map_err(|_| "连接保存失败，未应用到工具配置".to_string())?;
                Ok(provider.clone())
            },
            || {
                application
                    .db
                    .get_provider_by_id(&provider.id, app_type.as_str())
                    .map(|row| row.is_none())
                    .map_err(|_| "连接保存状态读取失败".to_string())
            },
            || {
                journal.record(&stored, recovery::Phase::CleanupPending)?;
                recovery::recover_database(&journal, &application.db, &vault)?;
                journal.ensure_clear()
            },
            || check_connected(&auth, generation).map(|_| ()),
        )
        .and_then(|saved| {
            journal.record(&stored, recovery::Phase::Committed)?;
            journal.clear()?;
            Ok(saved)
        })
    })
    .await
    .map_err(|_| "连接保存任务未完成，请检查供应商列表后重试".to_string())?
}

/// Replace one saved account grant without changing provider or route identity.
#[tauri::command]
pub async fn osir_replace_connection_key(
    app: tauri::AppHandle,
    state: State<'_, OsirAccountState>,
    request: crate::services::osir::connection::ReplaceConnectionKeyRequest,
) -> Result<crate::provider::Provider, String> {
    use crate::services::osir::{
        connection::TrustedKeyReplacement,
        key_vault::{KeyBinding, KeyMetadata, KeyTool, GATEWAY_ORIGIN},
        managed,
    };
    let app_type = request.validate()?;
    let _operation = state
        .connection_operation
        .try_lock()
        .map_err(|_| "连接操作进行中，请稍后重试")?;
    let generation = request.generation;
    let account_id = check_connected(&state.auth, generation)?;
    let preflight_app = app.clone();
    let preflight_tool = app_type.clone();
    let preflight_id = request.provider_id.clone();
    let preflight_reference = request.expected_reference.clone();
    tokio::task::spawn_blocking(move || {
        let application = preflight_app.state::<crate::store::AppState>();
        crate::services::ProviderService::check_osir_key_replacement(
            application.inner(),
            preflight_tool,
            &preflight_id,
            &preflight_reference,
        )
        .map_err(|_| {
            "此连接暂不能换钥，请刷新列表；OpenCode 已应用连接需先从工具配置移除".to_string()
        })
    })
    .await
    .map_err(|_| "连接状态检查未完成，请重试".to_string())??;
    check_connected(&state.auth, generation)?;
    let key = state
        .auth
        .read_key(request.key_id, generation)
        .await
        .map_err(|e| e.to_string())?;
    if key.generation != generation || key.data.id != request.key_id {
        return Err(STALE.into());
    }
    check_connected(&state.auth, generation)?;
    let binding = KeyBinding {
        account_origin: state.auth.account_origin(),
        account_id,
        key_id: key.data.id,
        group_id: key.data.group_id,
        provider_id: request.provider_id.clone(),
        gateway_origin: GATEWAY_ORIGIN.into(),
        tool: match app_type {
            crate::app_config::AppType::Codex => KeyTool::Codex,
            crate::app_config::AppType::Claude => KeyTool::ClaudeCode,
            crate::app_config::AppType::ClaudeDesktop => KeyTool::ClaudeDesktop,
            crate::app_config::AppType::Gemini => KeyTool::GeminiCli,
            crate::app_config::AppType::OpenCode => KeyTool::OpenCode,
            _ => return Err("此工具不支持账户换钥".into()),
        },
    };
    let secret = key.data.into_active_secret().map_err(|e| e.to_string())?;
    // Re-read the catalog natively: renderer data cannot grant model access.
    let models = crate::services::osir::gateway::models_for_account_key(
        &secret,
        &binding.account_origin,
        &binding.account_id,
        binding.key_id,
        binding.group_id,
        generation,
        &state.auth.request_budget(),
    )
    .await?;
    request.validate_catalog(&models)?;
    check_connected(&state.auth, generation)?;
    let auth = state.auth.clone();
    tokio::task::spawn_blocking(move || {
        use crate::error::AppError;
        let _operation = recovery::lock()?;
        let vault = managed::vault().map_err(|_| "连接凭据存储不可用")?;
        let journal =
            recovery::Journal::new(&app.path().app_data_dir().map_err(|_| "恢复日志不可用")?);
        let application = app.state::<crate::store::AppState>();
        recovery::recover_database(&journal, &application.db, &vault)?;
        journal.ensure_clear()?;
        let pending =
            std::cell::RefCell::new(None::<(KeyMetadata, crate::provider::ManagedBinding)>);
        let provider_id = request.provider_id.clone();
        let expected = request.expected_reference.clone();
        let saved = crate::services::ProviderService::replace_osir_key(
            application.inner(),
            app_type,
            &provider_id,
            &expected,
            |existing| {
                check_connected(&auth, generation).map_err(AppError::Message)?;
                let previous = existing
                    .meta
                    .as_ref()
                    .and_then(|m| m.osir_binding.as_ref())
                    .ok_or_else(|| AppError::InvalidInput("此连接不是账户托管连接".into()))?;
                if previous.binding.account_id != binding.account_id
                    || previous.binding.account_origin != binding.account_origin
                    || previous.binding.key_id == binding.key_id
                {
                    return Err(AppError::InvalidInput(
                        "账户或密钥已改变，请重新加载".into(),
                    ));
                }
                let prepared = vault
                    .prepare_store(&binding, &secret)
                    .map_err(|_| AppError::Message("连接凭据准备失败".into()))?;
                let trusted = TrustedKeyReplacement::new(request, &prepared)
                    .map_err(AppError::InvalidInput)?;
                journal
                    .record_replacement(&prepared, previous, recovery::Phase::Intent)
                    .map_err(AppError::Message)?;
                *pending.borrow_mut() = Some((prepared.clone(), previous.clone()));
                // The durable intent also covers a backend write-then-error.
                vault
                    .store_prepared(&prepared, &secret)
                    .map_err(|_| AppError::Message("连接凭据保存失败".into()))?;
                journal
                    .record_replacement(&prepared, previous, recovery::Phase::Stored)
                    .map_err(AppError::Message)?;
                check_connected(&auth, generation).map_err(AppError::Message)?;
                Ok(trusted)
            },
            |result| {
                if let Some((stored, previous)) = pending.borrow().as_ref() {
                    if result.is_ok() {
                        journal
                            .record_replacement(stored, previous, recovery::Phase::Committed)
                            .map_err(AppError::Message)?;
                    }
                    // Still under the tool switch lock. Unknown DB/live results
                    // retain both references; only confirmed absence is cleaned.
                    recovery::recover_database(&journal, &application.db, &vault)
                        .map_err(AppError::Message)?;
                    journal.ensure_clear().map_err(AppError::Message)?;
                }
                result
            },
        )
        .map_err(|_| "密钥更换未确认完成，请刷新供应商列表；未确认的凭据已保留".to_string())?;
        check_connected(&auth, generation)
            .map_err(|_| "账户已改变，请刷新供应商列表核对密钥更换结果".to_string())?;
        Ok(saved)
    })
    .await
    .map_err(|_| "密钥更换任务未完成，请刷新供应商列表后重试".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_freshness_matches_request_policy() {
        for (resource, seconds) in [
            (AccountResource::Profile, 120),
            (AccountResource::Keys, 300),
            (AccountResource::Groups, 300),
            (AccountResource::GroupRates, 300),
            (AccountResource::Subscriptions, 180),
            (AccountResource::Usage, 180),
            (AccountResource::UsageStats, 180),
            (AccountResource::UsageSnapshot, 60),
            (AccountResource::Announcements, 60),
        ] {
            assert_eq!(resource_ttl(resource), Duration::from_secs(seconds));
        }
    }
    #[test]
    fn stale_ipc_generation_does_not_clear_current_cache_binding() {
        let state = OsirAccountState::new("test.osir.dev").unwrap();
        let current = state
            .sync
            .start_session("https://osirclaw.com", "7")
            .unwrap();
        *state.binding.lock().unwrap() = Some((42, current.clone()));
        assert!(state.session(1).is_err());
        assert_eq!(state.binding.lock().unwrap().as_ref().unwrap().0, 42);
        assert!(state.sync.invalidate(&current, None).is_ok());
    }
}
