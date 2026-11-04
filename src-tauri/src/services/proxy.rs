//! 代理服务业务逻辑层
//!
//! 提供代理服务器的启动、停止和配置管理

use crate::app_config::AppType;
use crate::config::{get_claude_settings_path, read_json_file};
use crate::database::Database;
use crate::provider::Provider;
use crate::proxy::server::{ProxyServer, RestartAdmissionLease};
use crate::proxy::switch_lock::SwitchLockManager;
use crate::proxy::types::*;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::live::project::claude::PROXY_TOKEN_PLACEHOLDER;

#[derive(Clone)]
pub struct ProxyService {
    db: Arc<Database>,
    server: Arc<RwLock<Option<ProxyServer>>>,
    /// AppHandle，用于传递给 ProxyServer 以支持故障转移时的 UI 更新
    app_handle: Arc<RwLock<Option<tauri::AppHandle>>>,
    switch_locks: SwitchLockManager,
    maintenance: Arc<RwLock<()>>,
}

/// Admission is restored before releasing maintenance, so a replacement cannot
/// start between the two. A lease also excludes starts when there is no server.
pub(crate) struct RestartLease {
    _admission: Option<RestartAdmissionLease>,
    _maintenance: OwnedRwLockWriteGuard<()>,
}

/// 重启失败时写回库的那份配置：设置页只动四个全局字段，旧接口是整份七个字段。
enum SavedProxyConfig {
    Global(GlobalProxyConfig),
    Legacy(ProxyConfig),
}

impl SavedProxyConfig {
    async fn write(&self, db: &Database) -> Result<(), String> {
        match self {
            Self::Global(config) => db.update_global_proxy_config(config.clone()).await,
            Self::Legacy(config) => db.update_proxy_config(config.clone()).await,
        }
        .map_err(|e| format!("恢复原代理配置失败: {e}"))
    }
}

/// 客户端连接代理用的地址。`host` 是设置里的客户端地址，没设时是监听地址，后者可能是
/// `0.0.0.0` / `::`（监听所有网卡），客户端连不上这个地址，改用本机回环；IPv6 加方括号。
pub(crate) fn proxy_origin(host: &str, listen_port: u16) -> String {
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    };
    if host.contains(':') && !host.starts_with('[') {
        format!("http://[{host}]:{listen_port}")
    } else {
        format!("http://{host}:{listen_port}")
    }
}

impl ProxyService {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            server: Arc::new(RwLock::new(None)),
            app_handle: Arc::new(RwLock::new(None)),
            switch_locks: SwitchLockManager::new(),
            maintenance: Arc::new(RwLock::new(())),
        }
    }

    /// 设置 AppHandle（在应用初始化时调用）
    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        futures::executor::block_on(async {
            *self.app_handle.write().await = Some(handle);
        });
    }

    pub(crate) async fn lock_switch_for_app(
        &self,
        app_type: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        self.switch_locks.lock_for_app(app_type).await
    }

    /// 启动代理服务器。成功、失败都让托盘比对一次（图标圆点、问题区、「退出」的后果跟着变）。
    pub async fn start(&self) -> Result<ProxyServerInfo, String> {
        let result = self.start_server().await;
        self.notify_tray().await;
        result
    }

    /// 停止代理服务器。同 [`Self::start`]，结束后让托盘比对一次。
    pub async fn stop(&self) -> Result<(), String> {
        let result = self.stop_server().await;
        self.notify_tray().await;
        result
    }

    /// Hold the pause through installation and cleanup. Dropping the lease on
    /// failure or cancellation reopens this server's admission synchronously.
    pub(crate) async fn prepare_restart(&self) -> Result<RestartLease, String> {
        let maintenance = self
            .maintenance
            .clone()
            .try_write_owned()
            .map_err(|_| "代理设置正在更改或已在准备重启，请稍后重试。".to_string())?;
        let server = self.server.read().await;
        let admission =
            match server.as_ref() {
                Some(server) => Some(server.prepare_restart().await.map_err(|_| {
                    "当前请求尚未结束，已取消重启。请等待任务完成后重试。".to_string()
                })?),
                None => None,
            };
        Ok(RestartLease {
            _admission: admission,
            _maintenance: maintenance,
        })
    }

    fn try_operation(&self) -> Result<OwnedRwLockReadGuard<()>, String> {
        self.maintenance
            .clone()
            .try_read_owned()
            .map_err(|_| "正在准备重启，请稍后重试。".to_string())
    }

    /// 托盘在后台线程比对、变了才重建，这里不等它。
    async fn notify_tray(&self) {
        if let Some(handle) = self.app_handle.read().await.as_ref() {
            crate::tray::schedule_tray_status_check(handle);
        }
    }

    async fn start_server(&self) -> Result<ProxyServerInfo, String> {
        let _operation = self.try_operation()?;
        // Serialize check/create/commit; two starts must not produce two generations.
        let mut slot = self.server.write().await;
        if let Some(server) = slot.as_ref() {
            let status = server.get_status().await;
            if !status.running || !server.is_accepting().await {
                return Err("代理正在停止，请等待停止完成后重试。".into());
            }
            return Ok(ProxyServerInfo {
                address: status.address,
                port: status.port,
                started_at: chrono::Utc::now().to_rfc3339(),
            });
        }
        // 1. 启动时自动设置 proxy_enabled = true
        let mut global_config = self
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| format!("获取全局代理配置失败: {e}"))?;

        if !global_config.proxy_enabled {
            global_config.proxy_enabled = true;
            self.db
                .update_global_proxy_config(global_config.clone())
                .await
                .map_err(|e| format!("更新代理总开关失败: {e}"))?;
        }

        // 2. 获取配置
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;

        // 4. 创建并启动服务器
        let app_handle = self.app_handle.read().await.clone();
        let server = ProxyServer::new(config.clone(), self.db.clone(), app_handle);
        let info = server
            .start()
            .await
            .map_err(|e| format!("启动代理服务器失败: {e}"))?;
        if let Err(e) = self
            .persist_ephemeral_listen_port_if_needed(&config, info.port)
            .await
        {
            let _ = server.stop().await;
            return Err(e);
        }

        // 5. 保存服务器实例
        *slot = Some(server);

        log::info!("代理服务器已启动: {}:{}", info.address, info.port);
        Ok(info)
    }

    async fn persist_ephemeral_listen_port_if_needed(
        &self,
        config: &ProxyConfig,
        actual_port: u16,
    ) -> Result<(), String> {
        if config.listen_port != 0 {
            return Ok(());
        }

        // 端口是全局字段，不能通过旧接口回写各应用独立的重试和超时配置。
        let mut resolved_config = self
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| format!("获取全局代理配置失败: {e}"))?;
        resolved_config.listen_port = actual_port;
        self.db
            .update_global_proxy_config(resolved_config)
            .await
            .map_err(|e| format!("保存动态代理端口失败: {e}"))
    }

    /// 各应用是否处于代理模式（读设备本地的模式状态，不读会随云同步的
    /// `proxy_config.enabled`）。
    pub async fn get_takeover_status(&self) -> Result<ProxyTakeoverStatus, String> {
        let [claude, codex, gemini, grokbuild, claude_desktop] =
            crate::mode::current::proxy_flags([
                AppType::Claude,
                AppType::Codex,
                AppType::Gemini,
                AppType::GrokBuild,
                AppType::ClaudeDesktop,
            ]);
        Ok(ProxyTakeoverStatus {
            claude,
            codex,
            gemini,
            grokbuild,
            claude_desktop,
            // OpenCode and OpenClaw don't support proxy features
            opencode: false,
            openclaw: false,
        })
    }

    /// 把代理的上游指向 `provider`（状态面板里的「使用中」）。
    pub(crate) async fn set_active_target(&self, app_type: &AppType, provider: &Provider) {
        if let Some(server) = self.server.read().await.as_ref() {
            server
                .set_active_target(app_type.as_str(), &provider.id, &provider.name)
                .await;
        }
    }

    pub(crate) async fn emit(&self, event: &str, payload: Value) {
        if let Some(handle) = self.app_handle.read().await.as_ref() {
            if let Err(error) = handle.emit(event, payload) {
                log::warn!("发送事件 {event} 失败: {error}");
            }
        }
    }

    async fn stop_server(&self) -> Result<(), String> {
        let mut slot = self.server.write().await;
        if let Some(server) = slot.as_ref() {
            server
                .stop()
                .await
                .map_err(|e| format!("停止代理服务器失败: {e}"))?;
            // Keep ownership on timeout: callers must still be able to retry
            // or observe a server whose in-flight connections are draining.
            *slot = None;

            // 停止时设置 proxy_enabled = false
            let mut global_config = self
                .db
                .get_global_proxy_config()
                .await
                .map_err(|e| format!("获取全局代理配置失败: {e}"))?;

            if global_config.proxy_enabled {
                global_config.proxy_enabled = false;
                if let Err(e) = self.db.update_global_proxy_config(global_config).await {
                    log::warn!("更新代理总开关失败: {e}");
                }
            }

            log::info!("代理服务器已停止");
            Ok(())
        } else {
            Err("代理服务器未运行".to_string())
        }
    }

    /// 构造写入 Live 的代理地址（处理 0.0.0.0 / IPv6 等特殊情况）
    pub(crate) async fn build_proxy_urls(&self) -> Result<(String, String), String> {
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;

        let mut listen_port = config.listen_port;
        if let Some(server) = self.server.read().await.as_ref() {
            let status = server.get_status().await;
            if status.running {
                listen_port = status.port;
            }
        }
        if listen_port == 0 {
            return Err("代理监听端口为 0，但代理服务器尚未运行，无法生成接管地址".to_string());
        }

        let host = crate::settings::get_proxy_client_host().unwrap_or(config.listen_address);
        let proxy_url = proxy_origin(&host, listen_port);
        let proxy_codex_base_url = format!("{proxy_url}/v1");
        Ok((proxy_url, proxy_codex_base_url))
    }

    /// 客户端文件里有没有接管占位符 `PROXY_MANAGED`（旧版接管的遗留物，或新版接上代理
    /// 时写的契约）。Codex 的官方路由不带占位符，按指向本地代理的地址认。
    pub(crate) fn live_has_proxy_placeholder(&self, app_type: &AppType) -> bool {
        match app_type {
            AppType::Claude => match self.read_claude_live() {
                Ok(config) => Self::is_claude_live_taken_over(&config),
                Err(_) => false,
            },
            AppType::Codex => {
                match self.read_codex_live() {
                    Ok(config) => Self::is_codex_live_taken_over(&config)
                        || config
                            .get("config")
                            .and_then(|v| v.as_str())
                            .is_some_and(|text| {
                                crate::services::provider::codex_direct::routes_official_to_proxy(
                                    &self.db, text,
                                )
                            }),
                    Err(_) => false,
                }
            }
            AppType::Gemini => match self.read_gemini_live() {
                Ok(config) => Self::is_gemini_live_taken_over(&config),
                Err(_) => false,
            },
            AppType::GrokBuild => match self.read_grok_live() {
                Ok(config) => Self::is_grok_live_taken_over(&config),
                Err(_) => false,
            },
            _ => false,
        }
    }

    /// 一份配置（客户端文件或供应商行）里有没有接管占位符：行里带着它的是旧版接管期间
    /// 被导入的残留，不能照写回 live，否则客户端会一直指着已经不在的本地代理。
    pub(crate) fn config_has_proxy_placeholder(app_type: &AppType, config: &Value) -> bool {
        match app_type {
            AppType::Claude => Self::is_claude_live_taken_over(config),
            AppType::Codex => Self::is_codex_live_taken_over(config),
            AppType::Gemini => Self::is_gemini_live_taken_over(config),
            AppType::GrokBuild => Self::is_grok_live_taken_over(config),
            _ => false,
        }
    }

    /// 是否有应用处于代理模式
    pub async fn is_takeover_active(&self) -> Result<bool, String> {
        let status = self.get_takeover_status().await?;
        Ok(status.claude
            || status.codex
            || status.gemini
            || status.grokbuild
            || status.claude_desktop)
    }

    fn is_claude_live_taken_over(config: &Value) -> bool {
        let env = match config.get("env").and_then(|v| v.as_object()) {
            Some(env) => env,
            None => return false,
        };

        for key in [
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
            "OPENAI_API_KEY",
        ] {
            if env.get(key).and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER) {
                return true;
            }
        }

        false
    }

    fn codex_live_has_proxy_placeholder(config: &Value) -> bool {
        if config
            .get("auth")
            .and_then(|v| v.as_object())
            .and_then(|auth| auth.get("OPENAI_API_KEY"))
            .and_then(|v| v.as_str())
            == Some(PROXY_TOKEN_PLACEHOLDER)
        {
            return true;
        }

        config
            .get("config")
            .and_then(|v| v.as_str())
            .and_then(crate::codex_config::extract_codex_experimental_bearer_token)
            .as_deref()
            == Some(PROXY_TOKEN_PLACEHOLDER)
    }

    fn is_codex_live_taken_over(config: &Value) -> bool {
        Self::codex_live_has_proxy_placeholder(config)
            || config
                .get("config")
                .and_then(|v| v.as_str())
                .is_some_and(crate::codex_config::codex_config_has_official_proxy_route)
    }

    fn is_gemini_live_taken_over(config: &Value) -> bool {
        let env = match config.get("env").and_then(|v| v.as_object()) {
            Some(env) => env,
            None => return false,
        };
        env.get("GEMINI_API_KEY").and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER)
    }

    fn is_grok_live_taken_over(config: &Value) -> bool {
        config
            .get("config")
            .and_then(Value::as_str)
            .is_some_and(|config_toml| {
                crate::grok_config::has_proxy_placeholder(config_toml, PROXY_TOKEN_PLACEHOLDER)
            })
    }

    // ==================== Live 配置读写辅助方法 ====================

    fn read_claude_live(&self) -> Result<Value, String> {
        let path = get_claude_settings_path();
        if !path.exists() {
            return Err("Claude 配置文件不存在".to_string());
        }

        let mut value: Value =
            read_json_file(&path).map_err(|e| format!("读取 Claude 配置失败: {e}"))?;

        if value.is_null() {
            value = json!({});
        }

        if !value.is_object() {
            let kind = match &value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            return Err(format!(
                "Claude 配置文件格式错误：根节点必须是 JSON 对象（当前为 {kind}），路径: {}",
                path.display()
            ));
        }

        Ok(value)
    }

    fn read_codex_live(&self) -> Result<Value, String> {
        crate::codex_config::read_codex_live_settings()
            .map_err(|e| format!("读取 Codex Live 配置失败: {e}"))
    }

    fn read_gemini_live(&self) -> Result<Value, String> {
        use crate::gemini_config::{env_to_json, get_gemini_env_path, read_gemini_env};

        let env_path = get_gemini_env_path();
        if !env_path.exists() {
            return Err("Gemini .env 文件不存在".to_string());
        }

        let env_map = read_gemini_env().map_err(|e| format!("读取 Gemini env 失败: {e}"))?;
        Ok(env_to_json(&env_map))
    }

    fn read_grok_live(&self) -> Result<Value, String> {
        crate::grok_config::read_grok_live_settings()
            .map_err(|e| format!("读取 Grok Build 配置失败: {e}"))
    }

    // ==================== 原有方法 ====================

    /// 获取服务器状态
    pub async fn get_status(&self) -> Result<ProxyStatus, String> {
        if let Some(server) = self.server.read().await.as_ref() {
            let mut status = server.get_status().await;
            // Public running means the listener is still available, not merely
            // that an owned task is draining after a stop timeout.
            status.running &= server.is_accepting().await;
            Ok(status)
        } else {
            // 服务器未运行时返回默认状态
            Ok(ProxyStatus {
                running: false,
                ..Default::default()
            })
        }
    }

    /// 获取代理配置
    pub async fn get_config(&self) -> Result<ProxyConfig, String> {
        self.db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))
    }

    /// 更新代理配置（旧接口：七个字段整份写进四行）。返回代理是否因地址或端口变了而重启：
    /// 重启后调用方要按新地址重写接上代理的客户端（`mode::controller::resync_route`）。
    pub async fn update_config(&self, config: &ProxyConfig) -> Result<bool, String> {
        let _operation = self.try_operation()?;
        // 记录旧配置用于判定是否需要重启
        let previous = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;

        // 保存到数据库（保持 live_takeover_active 状态不变）
        let mut new_config = config.clone();
        new_config.live_takeover_active = previous.live_takeover_active;

        self.db
            .update_proxy_config(new_config.clone())
            .await
            .map_err(|e| format!("保存代理配置失败: {e}"))?;

        // 判断是否需要重启（地址或端口变更）
        let require_restart = new_config.listen_address != previous.listen_address
            || new_config.listen_port != previous.listen_port;
        self.apply_saved_config(require_restart, SavedProxyConfig::Legacy(previous))
            .await
    }

    /// 设置页「保存并重启服务」：只写四个全局字段。各应用自己的重试次数和超时不碰——
    /// [`Self::update_config`] 走的旧 DAO 读的是 claude 行、写的是四行，会把 claude 的
    /// 应用级字段广播给其他三家。返回值同 [`Self::update_config`]。
    pub async fn update_global_config(&self, config: &GlobalProxyConfig) -> Result<bool, String> {
        let _operation = self.try_operation()?;
        let previous = self
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| format!("获取全局代理配置失败: {e}"))?;

        self.db
            .update_global_proxy_config(config.clone())
            .await
            .map_err(|e| format!("保存全局代理配置失败: {e}"))?;

        let require_restart = config.listen_address != previous.listen_address
            || config.listen_port != previous.listen_port;
        self.apply_saved_config(require_restart, SavedProxyConfig::Global(previous))
            .await
    }

    /// 让运行中的服务用上刚写进库的配置：地址或端口变了就重启（返回 true），否则实时应用。
    /// 服务没在跑时什么都不做。新地址绑不上就把 `previous` 写回库、按旧地址重新拉起，让
    /// 「保存失败」就是什么都没变：服务停着不管的话，下一次保存会因为没在跑而假成功，
    /// 客户端也还指着旧地址。
    async fn apply_saved_config(
        &self,
        require_restart: bool,
        previous: SavedProxyConfig,
    ) -> Result<bool, String> {
        let _operation = self.try_operation()?;
        let mut server_guard = self.server.write().await;
        if server_guard.is_none() {
            return Ok(false);
        }
        let new_config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;

        if !require_restart {
            if let Some(server) = server_guard.as_ref() {
                server.apply_runtime_config(&new_config).await;
                log::info!("代理配置已实时应用，无需重启代理服务器");
            }
            return Ok(false);
        }

        // Pause before closing the listener. A busy server must keep serving
        // when a settings-triggered restart cannot drain within the budget.
        let _admission = if let Some(server) = server_guard.as_ref() {
            match server.prepare_restart().await {
                Ok(lease) => Some(lease),
                Err(_) => {
                    previous.write(&self.db).await?;
                    return Err("当前请求尚未结束，已保留原代理设置。请等待任务完成后重试。".into());
                }
            }
        } else {
            None
        };
        if let Some(server) = server_guard.as_ref() {
            if let Err(error) = server.stop().await {
                previous.write(&self.db).await?;
                return Err(format!("重启前停止代理服务器失败: {error}"));
            }
        }
        *server_guard = None;

        let error = match self.start_with(&new_config).await {
            Ok(server) => {
                *server_guard = Some(server);
                log::info!("代理配置已更新，服务器已自动重启应用最新配置");
                return Ok(true);
            }
            Err(error) => format!("重启代理服务器失败: {error}"),
        };

        log::warn!("{error}，恢复原配置并重新启动");
        match self.restore_and_start(previous).await {
            Ok(server) => {
                *server_guard = Some(server);
                Err(format!("{error}；已按原配置重新启动"))
            }
            Err(restore_error) => Err(format!("{error}；恢复原配置也失败: {restore_error}")),
        }
    }

    /// 按 `config` 起一个新服务，动态端口回写进库。
    async fn start_with(&self, config: &ProxyConfig) -> Result<ProxyServer, String> {
        let _operation = self.try_operation()?;
        let app_handle = self.app_handle.read().await.clone();
        let server = ProxyServer::new(config.clone(), self.db.clone(), app_handle);
        let info = server.start().await.map_err(|e| e.to_string())?;
        if let Err(e) = self
            .persist_ephemeral_listen_port_if_needed(config, info.port)
            .await
        {
            let _ = server.stop().await;
            return Err(e);
        }
        Ok(server)
    }

    async fn restore_and_start(&self, previous: SavedProxyConfig) -> Result<ProxyServer, String> {
        let _operation = self.try_operation()?;
        previous.write(&self.db).await?;
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("获取代理配置失败: {e}"))?;
        self.start_with(&config).await
    }

    /// 检查服务器是否正在运行
    pub async fn is_running(&self) -> bool {
        match self.server.read().await.as_ref() {
            Some(server) => server.is_accepting().await && server.get_status().await.running,
            None => false,
        }
    }

    /// 同 [`Self::is_running`]，但不等锁：托盘菜单这种同步路径用。正在启动 / 停止、拿不到锁时
    /// 为 `None`（按「不知道」处理，不报问题）。
    pub fn running_now(&self) -> Option<bool> {
        let slot = self.server.try_read().ok()?;
        match slot.as_ref() {
            Some(server) => server.accepting_now(),
            None => Some(false),
        }
    }

    /// 热更新熔断器配置
    ///
    /// 如果代理服务器正在运行，将新配置应用到所有已创建的熔断器实例
    pub async fn update_circuit_breaker_configs(
        &self,
        config: crate::proxy::CircuitBreakerConfig,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server.update_circuit_breaker_configs(config).await;
            log::info!("已热更新运行中的熔断器配置");
        } else {
            log::debug!("代理服务器未运行，熔断器配置将在下次启动时生效");
        }
        Ok(())
    }

    /// 热更新指定应用的熔断器配置
    pub async fn update_circuit_breaker_config_for_app(
        &self,
        app_type: &str,
        config: crate::proxy::CircuitBreakerConfig,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server
                .update_circuit_breaker_config_for_app(app_type, config)
                .await;
            log::info!("已热更新 {app_type} 运行中的熔断器配置");
        } else {
            log::debug!("{app_type} 熔断器配置将在下次代理启动时生效");
        }
        Ok(())
    }

    /// 重置指定 Provider 的熔断器
    ///
    /// 如果代理服务器正在运行，立即重置内存中的熔断器状态
    pub async fn reset_provider_circuit_breaker(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server
                .reset_provider_circuit_breaker(provider_id, app_type)
                .await;
            log::info!("已重置 Provider {provider_id} (app: {app_type}) 的熔断器");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn settings_restart_timeout_keeps_original_listener_and_config() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let db = Arc::new(Database::memory().unwrap());
        let service = ProxyService::new(db.clone());
        let mut global = db.get_global_proxy_config().await.unwrap();
        global.listen_address = "127.0.0.1".into();
        global.listen_port = 0;
        db.update_global_proxy_config(global).await.unwrap();
        let port = service.start().await.unwrap().port;
        let original = db.get_global_proxy_config().await.unwrap();
        let mut pending = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        pending.write_all(b"POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut interim = [0; b"HTTP/1.1 100 Continue\r\n\r\n".len()];
        tokio::time::timeout(Duration::from_secs(2), pending.read_exact(&mut interim))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        let mut changed = original.clone();
        changed.listen_port = 0;
        let error = tokio::time::timeout(
            Duration::from_secs(33),
            service.update_global_config(&changed),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.contains("已保留原代理设置"));
        assert!(service.is_running().await);
        assert_eq!(
            serde_json::to_value(db.get_global_proxy_config().await.unwrap()).unwrap(),
            serde_json::to_value(original).unwrap()
        );
        // The original request survives, and admission reopens on the same port.
        pending.write_all(b"{").await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), pending.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        // Invalid JSON reaches the existing parser after the pending body is
        // supplied. Its current protocol status is Internal (500).
        assert!(response.starts_with(b"HTTP/1.1 500"));
        let mut next = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        next.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        response.clear();
        tokio::time::timeout(Duration::from_secs(2), next.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 "));
        assert!(!response.starts_with(b"HTTP/1.1 503"));
        service.stop().await.unwrap();
    }

    #[tokio::test]
    async fn maintenance_without_server_rejects_concurrent_start_and_config_writes() {
        let db = Arc::new(Database::memory().unwrap());
        let service = ProxyService::new(db.clone());
        let original = db.get_global_proxy_config().await.unwrap();
        let mut global = original.clone();
        global.listen_port = 0;
        let mut config = db.get_proxy_config().await.unwrap();
        let original_config = serde_json::to_value(&config).unwrap();
        config.listen_port = 0;
        let lease = service.prepare_restart().await.unwrap();
        let starting = {
            let service = service.clone();
            tokio::spawn(async move { service.start().await })
        };
        let updating = {
            let service = service.clone();
            tokio::spawn(async move { service.update_config(&config).await })
        };
        let updating_global = {
            let service = service.clone();
            tokio::spawn(async move { service.update_global_config(&global).await })
        };
        assert!(starting.await.unwrap().is_err());
        assert!(updating.await.unwrap().is_err());
        assert!(updating_global.await.unwrap().is_err());
        assert!(service.server.read().await.is_none());
        assert_eq!(
            serde_json::to_value(db.get_global_proxy_config().await.unwrap()).unwrap(),
            serde_json::to_value(original).unwrap()
        );
        assert_eq!(
            serde_json::to_value(db.get_proxy_config().await.unwrap()).unwrap(),
            original_config
        );
        assert!(service.prepare_restart().await.is_err());
        drop(lease);
        assert!(service.try_operation().is_ok());
    }

    #[tokio::test]
    async fn maintenance_lease_survives_cleanup_stop_and_excludes_new_generation() {
        let db = Arc::new(Database::memory().unwrap());
        let service = ProxyService::new(db.clone());
        let mut global = db.get_global_proxy_config().await.unwrap();
        global.listen_address = "127.0.0.1".into();
        global.listen_port = 0;
        db.update_global_proxy_config(global).await.unwrap();
        service.start().await.unwrap();
        let lease = service.prepare_restart().await.unwrap();
        // Cleanup must run under exclusive maintenance without reacquiring a read guard.
        tokio::time::timeout(std::time::Duration::from_secs(2), service.stop())
            .await
            .unwrap()
            .unwrap();
        let starting = {
            let service = service.clone();
            tokio::spawn(async move { service.start().await })
        };
        assert!(starting.await.unwrap().is_err());
        assert!(service.server.read().await.is_none());
        let config = db.get_proxy_config().await.unwrap();
        assert!(service.start_with(&config).await.is_err());
        assert!(service
            .restore_and_start(SavedProxyConfig::Legacy(config))
            .await
            .is_err());
        drop(lease);
        service.start().await.unwrap();
        service.stop().await.unwrap();
    }

    #[tokio::test]
    async fn maintenance_cannot_overtake_start_commit_and_cancellation_releases_it() {
        let db = Arc::new(Database::memory().unwrap());
        let service = ProxyService::new(db.clone());
        let mut global = db.get_global_proxy_config().await.unwrap();
        global.listen_address = "127.0.0.1".into();
        global.listen_port = 0;
        db.update_global_proxy_config(global).await.unwrap();
        let slot = service.server.write().await;
        let starting = {
            let service = service.clone();
            tokio::spawn(async move { service.start().await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while service.maintenance.try_write().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(service.prepare_restart().await.is_err());
        drop(slot);
        starting.await.unwrap().unwrap();

        // Cancellation while prepare waits for the server lock must drop the
        // service maintenance write guard, even before an admission lease exists.
        let slot = service.server.write().await;
        let preparing = {
            let service = service.clone();
            tokio::spawn(async move { service.prepare_restart().await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while service.maintenance.try_read().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        preparing.abort();
        assert!(matches!(preparing.await, Err(error) if error.is_cancelled()));
        assert!(service.try_operation().is_ok());
        drop(slot);
        service.stop().await.unwrap();
    }

    /// Exercise the real service stop path and its five-second production budget.
    /// Expect/Continue acknowledges that the request body is being polled before
    /// stop begins, without relying on the forwarding-only active counter.
    #[tokio::test]
    async fn stop_timeout_preserves_server_and_allows_service_stop_retry() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let db = Arc::new(Database::memory().unwrap());
        let service = ProxyService::new(db.clone());
        let mut global = db.get_global_proxy_config().await.unwrap();
        global.listen_address = "127.0.0.1".into();
        global.listen_port = 0;
        db.update_global_proxy_config(global).await.unwrap();
        let port = service.start().await.unwrap().port;
        let mut pending = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        pending.write_all(b"POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nExpect: 100-continue\r\n\r\n").await.unwrap();
        let mut interim = [0; b"HTTP/1.1 100 Continue\r\n\r\n".len()];
        tokio::time::timeout(Duration::from_secs(2), pending.read_exact(&mut interim))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");

        let error = tokio::time::timeout(Duration::from_secs(7), service.stop())
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.contains("停止超时"), "{error}");
        assert!(
            service.server.read().await.is_some(),
            "timeout must retain the actual server"
        );
        assert!(
            !service.is_running().await,
            "closed listener must not report available"
        );
        assert_eq!(service.running_now(), Some(false));
        assert!(!service.get_status().await.unwrap().running);
        assert!(
            service.start().await.is_err(),
            "a draining server is not a successful start"
        );
        assert!(
            service
                .server
                .read()
                .await
                .as_ref()
                .unwrap()
                .get_status()
                .await
                .running,
            "still draining this request"
        );
        assert!(db.get_global_proxy_config().await.unwrap().proxy_enabled);
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err());
        let mut probe = [0; 1];
        assert!(
            matches!(pending.try_read(&mut probe), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );

        // Invalid synthetic JSON ends at local parsing, before credentials or upstream IO.
        pending.write_all(b"{").await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), pending.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(
            response.starts_with(b"HTTP/1.1 "),
            "the original request must finish normally"
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while service
                .server
                .read()
                .await
                .as_ref()
                .unwrap()
                .get_status()
                .await
                .running
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            service.server.read().await.is_some(),
            "completed server awaits the service retry"
        );

        service.stop().await.unwrap();
        assert!(service.server.read().await.is_none());
        assert!(!service.is_running().await);
        assert!(!service.get_status().await.unwrap().running);
        assert!(!db.get_global_proxy_config().await.unwrap().proxy_enabled);
    }

    async fn seed_distinct_app_proxy_configs(db: &Database) -> Vec<Value> {
        let mut configs = Vec::new();
        for (app, retries) in [("claude", 6), ("codex", 0), ("gemini", 2), ("grokbuild", 3)] {
            let mut config = db.get_proxy_config_for_app(app).await.unwrap();
            config.enabled = retries % 2 == 0;
            config.auto_failover_enabled = retries % 2 != 0;
            config.max_retries = retries;
            config.streaming_first_byte_timeout = 30 + retries;
            config.streaming_idle_timeout = 90 + retries;
            config.non_streaming_timeout = 300 + retries;
            config.circuit_failure_threshold = 5 + retries;
            configs.push(serde_json::to_value(&config).unwrap());
            db.update_proxy_config_for_app(config).await.unwrap();
        }
        configs
    }

    async fn assert_app_proxy_configs_unchanged(db: &Database, configs: &[Value]) {
        for expected in configs {
            let app = expected["appType"].as_str().unwrap();
            let actual = db.get_proxy_config_for_app(app).await.unwrap();
            assert_eq!(serde_json::to_value(actual).unwrap(), *expected, "{app}");
        }
    }

    #[tokio::test]
    async fn ephemeral_port_preserves_app_proxy_configs() {
        let db = Arc::new(Database::memory().unwrap());
        let configs = seed_distinct_app_proxy_configs(&db).await;
        let service = ProxyService::new(db.clone());
        let mut config = db.get_proxy_config().await.unwrap();
        config.listen_port = 0;
        let mut expected_global = db.get_global_proxy_config().await.unwrap();
        expected_global.listen_port = 23456;

        service
            .persist_ephemeral_listen_port_if_needed(&config, 23456)
            .await
            .unwrap();

        assert_app_proxy_configs_unchanged(&db, &configs).await;
        assert_eq!(
            serde_json::to_value(db.get_global_proxy_config().await.unwrap()).unwrap(),
            serde_json::to_value(expected_global).unwrap()
        );

        config.listen_port = 23456;
        service
            .persist_ephemeral_listen_port_if_needed(&config, 34567)
            .await
            .unwrap();
        assert_eq!(
            db.get_global_proxy_config().await.unwrap().listen_port,
            23456
        );
        assert_app_proxy_configs_unchanged(&db, &configs).await;
    }

    #[tokio::test]
    async fn update_global_config_preserves_app_proxy_configs() {
        let db = Arc::new(Database::memory().unwrap());
        let configs = seed_distinct_app_proxy_configs(&db).await;
        let service = ProxyService::new(db.clone());
        let mut global = db.get_global_proxy_config().await.unwrap();
        global.enable_logging = !global.enable_logging;
        global.listen_port = 23456;

        let restarted = service.update_global_config(&global).await.unwrap();

        assert!(!restarted, "服务没在跑，不该报重启");
        assert_app_proxy_configs_unchanged(&db, &configs).await;
        assert_eq!(
            serde_json::to_value(db.get_global_proxy_config().await.unwrap()).unwrap(),
            serde_json::to_value(global).unwrap()
        );
    }

    /// 新端口绑不上时「保存失败」要等于什么都没变：库里还是旧端口、服务还在旧端口上跑。
    /// 服务停着不管的话，下一次保存会因为「没在跑」直接假成功。
    #[tokio::test]
    async fn failed_restart_restores_previous_config_and_server() {
        let db = Arc::new(Database::memory().unwrap());
        let service = ProxyService::new(db.clone());
        let mut global = db.get_global_proxy_config().await.unwrap();
        global.listen_address = "127.0.0.1".to_string();
        global.listen_port = 0;
        db.update_global_proxy_config(global).await.unwrap();
        let running_port = service.start().await.unwrap().port;
        assert_ne!(running_port, 0);

        // 占住另一个端口，让重启时绑定失败
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken_port = blocker.local_addr().unwrap().port();
        let mut attempt = db.get_global_proxy_config().await.unwrap();
        attempt.listen_port = taken_port;

        let error = service
            .update_global_config(&attempt)
            .await
            .expect_err("binding a taken port must fail");
        assert!(error.contains("重启代理服务器失败"), "{error}");

        assert_eq!(
            db.get_global_proxy_config().await.unwrap().listen_port,
            running_port,
            "库里应回到原端口"
        );
        let status = service.get_status().await.unwrap();
        assert!(status.running, "服务应按原配置重新拉起");
        assert_eq!(status.port, running_port);

        // 再保存一次不再假成功：服务在跑，配置没变就实时应用
        let again = db.get_global_proxy_config().await.unwrap();
        assert!(!service.update_global_config(&again).await.unwrap());
        assert!(service.is_running().await);
        service.stop().await.unwrap();
        drop(blocker);
    }
}
