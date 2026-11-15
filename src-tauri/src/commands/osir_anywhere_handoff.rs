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
    let tools = configured_tools(app).await;
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

async fn configured_tools(app: &tauri::AppHandle) -> Vec<String> {
    use crate::app_config::AppType;
    let state = app.state::<crate::store::AppState>();
    collect_configured_tools(
        async {
            let claude =
                crate::mode::controller::connection_mode(state.inner(), &AppType::Claude).await?;
            Ok(matches!(claude.mode, "native" | "api_key"))
        },
        async {
            Ok(
                crate::mode::controller::anywhere_codex_connection(state.inner())
                    .await?
                    .0
                    == Some(true),
            )
        },
        async {
            if crate::distribution::is_isolated()
                || crate::dsh_config::current_selection()
                    .map_err(|_| "DSH 模型配置无法读取")?
                    .is_none()
            {
                return Ok(false);
            }
            // Keep DSH's existing CLI/bridge checks. Its local failure must not
            // block the account handoff or other independently verified tools.
            let dsh = super::osir_tools::osir_launch_tool(
                app.clone(),
                app.state::<crate::store::AppState>(),
                "dsh".into(),
                "desktop".into(),
                None,
                Some(false),
            )
            .await?;
            Ok(matches!(
                dsh.runtime_status,
                super::osir_tools::ToolRuntimeStatus::Running
            ))
        },
    )
    .await
}

/// Tool types are optional hints, never account authorization. Unknown/failed
/// probes contribute no type; even an empty list can complete a secure handoff.
async fn collect_configured_tools<C, X, D>(claude: C, codex: X, dsh: D) -> Vec<String>
where
    C: Future<Output = Result<bool, String>>,
    X: Future<Output = Result<bool, String>>,
    D: Future<Output = Result<bool, String>>,
{
    let mut tools = Vec::new();
    for (tool, result) in [
        ("claude", claude.await),
        ("codex", codex.await),
        ("dsh", dsh.await),
    ] {
        match result {
            Ok(true) => tools.push(tool.into()),
            Ok(false) => {}
            // Raw configuration errors may contain paths or credentials.
            Err(_) => log::warn!("Anywhere handoff: {tool} readiness unavailable"),
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn configured_tools_isolate_each_failed_or_unavailable_probe() {
        async fn probe(status: u8, calls: &AtomicUsize) -> Result<bool, String> {
            calls.fetch_add(1, Ordering::Relaxed);
            match status {
                0 => Err("private configuration fixture".into()),
                1 => Ok(false),
                _ => Ok(true),
            }
        }
        // All 27 combinations cover individual/combined failures, unknown
        // connections, stopped DSH, no tools and stable transport allowlist order.
        for claude in 0..3 {
            for codex in 0..3 {
                for dsh in 0..3 {
                    let calls = AtomicUsize::new(0);
                    let tools = collect_configured_tools(
                        probe(claude, &calls),
                        probe(codex, &calls),
                        probe(dsh, &calls),
                    )
                    .await;
                    let expected: Vec<_> = [("claude", claude), ("codex", codex), ("dsh", dsh)]
                        .into_iter()
                        .filter_map(|(tool, status)| (status == 2).then_some(tool))
                        .collect();
                    assert_eq!(tools, expected);
                    assert_eq!(calls.load(Ordering::Relaxed), 3);
                }
            }
        }
    }
}
