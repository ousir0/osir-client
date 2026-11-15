//! Entry point reachable only from the explicit Client install/open action.
use crate::services::osir::{auth::AuthManager, anywhere_handoff_transport::{HandoffTransport, PeerIdentity, PeerVerifier, TransportError}, anywhere_handoff_trust::{self, Trust}};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};
use tauri::Manager;

static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Receiver { bundle: PathBuf, trust: Trust }
impl PeerVerifier for Receiver {
    fn verify<'a>(&'a self, peer: &'a PeerIdentity) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            // Electron Main owns connect(); its helper only inspects the
            // inherited descriptor and never carries the credential payload.
            let executable = self.bundle.join("Contents/MacOS/OSIR Anywhere");
            if peer.executable_path != executable {
                return Err(TransportError::PeerUntrusted);
            }
            self.trust.verify_bundle(&self.bundle, "osir-anywhere").await.map_err(|_| TransportError::PeerUntrusted)
        })
    }
}

pub(super) async fn launch(app: &tauri::AppHandle) -> Result<super::osir_tools::OsirToolLaunch, String> {
    let _guard = GATE.try_lock().map_err(|_| "Anywhere 正在连接，请稍候")?;
    let auth: Arc<AuthManager> = app.state::<super::osir::OsirAccountState>().auth.clone();
    let status = auth.status();
    let account = status.profile.as_ref().filter(|p| p.id > 0).map(|p| p.id.to_string())
        .filter(|_| status.connected).ok_or("请先登录 OSIR 客户端，再打开 Anywhere")?;
    let home = anywhere_handoff_trust::native_home()?;
    let bundle = super::osir_tools::macos::handoff_bundle().await?;
    let trust = anywhere_handoff_trust::fetch().await?;
    trust.verify_bundle(&bundle, "osir-anywhere").await?;
    let tools = configured_tools(app).await?;
    if auth.status().generation != status.generation || !auth.status().connected { return Err("账号已改变，请重新打开 Anywhere".into()); }
    let transport = HandoffTransport::bind(&home).map_err(|_| "无法创建本机安全连接，请重试")?;
    super::osir_tools::macos::launch_handoff_bundle(&bundle, transport.flow_id()).await?;
    let receiver = Receiver { bundle, trust };
    let (cancel, rx) = tokio::sync::watch::channel(false);
    let authorization = auth.clone();
    let generation = status.generation;
    let operation = transport.serve(&receiver, |request| async move {
        authorization.authorize_anywhere_handoff(&request, generation, &account).await.map_err(|_| TransportError::AuthorizationFailed)
    }, tools, rx);
    tokio::pin!(operation);
    let result = loop {
        tokio::select! {
            result = &mut operation => break result,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                let current = auth.status();
                if !current.connected || current.generation != status.generation { let _ = cancel.send(true); }
            }
        }
    };
    // A successful open followed by a handoff timeout/stopped process can be a
    // crash or transport failure. It supplies no system policy-denial evidence.
    result.map_err(|error| match error {
        TransportError::PeerUntrusted => "Anywhere 版本无法验证，请通过客户端修复安装",
        TransportError::Cancelled => "账号已改变，已取消本次 Anywhere 连接",
        TransportError::AuthorizationFailed => "Anywhere 账号连接未完成，请检查服务状态后重试",
        TransportError::Timeout => "Anywhere 连接超时，请重新打开",
        _ => "Anywhere 安全连接未完成，请重新打开",
    })?;
    Ok(super::osir_tools::OsirToolLaunch {
        isolated: crate::distribution::is_isolated(), already_running: false,
        runtime_status: super::osir_tools::ToolRuntimeStatus::Running,
        foreground: true, exit_code: None, restarted: false,
    })
}

async fn configured_tools(app: &tauri::AppHandle) -> Result<Vec<String>, String> {
    use crate::app_config::AppType;
    let state = app.state::<crate::store::AppState>();
    let mut tools = Vec::new();
    let claude = crate::mode::controller::connection_mode(state.inner(), &AppType::Claude).await?;
    if matches!(claude.mode, "native" | "api_key") { tools.push("claude".into()); }
    if crate::mode::controller::anywhere_codex_connection(state.inner()).await?.0 == Some(true) {
        tools.push("codex".into());
    }
    // Reuse the native DSH launch path: it validates the official Desktop CLI,
    // installs the bridge bundled with this verified Anywhere, and preserves
    // the model selection. No provider/credential is copied into cloud config.
    if !crate::distribution::is_isolated() && crate::dsh_config::current_selection().map_err(|_| "DSH 模型配置无法读取")?.is_some() {
        let dsh = super::osir_tools::osir_launch_tool(app.clone(), state, "dsh".into(), "desktop".into(), None, Some(false)).await?;
        if !matches!(dsh.runtime_status, super::osir_tools::ToolRuntimeStatus::Running) { return Err("DSH 尚未启动，请检查 DSH 安装后重试".into()); }
        tools.push("dsh".into());
    }
    Ok(tools)
}
