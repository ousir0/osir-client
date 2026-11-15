//! Entry point reachable only from the explicit Client install/open action.
use crate::services::osir::{
    anywhere_handoff_transport::{HandoffTransport, PeerIdentity, PeerVerifier, TransportError},
    anywhere_handoff_trust::{self, Trust},
    auth::AuthManager,
};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};
use tauri::Manager;

static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Receiver {
    bundle: PathBuf,
    trust: Trust,
}
impl PeerVerifier for Receiver {
    fn verify<'a>(
        &'a self,
        peer: &'a PeerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            // Electron Main owns connect(); its helper only inspects the
            // inherited descriptor and never carries the credential payload.
            let executable = self.bundle.join("Contents/MacOS/OSIR Anywhere");
            if peer.executable_path != executable {
                log::warn!("Anywhere handoff stage=receiver_verify");
                return Err(TransportError::PeerUntrusted);
            }
            self.trust
                .verify_bundle(&self.bundle, "osir-anywhere")
                .await
                .inspect_err(|_| log::warn!("Anywhere handoff stage=receiver_verify"))
                .map_err(|_| TransportError::PeerUntrusted)
        })
    }
}

pub(super) async fn launch(
    app: &tauri::AppHandle,
) -> Result<super::osir_tools::OsirToolLaunch, String> {
    let _guard = GATE.try_lock().map_err(|_| "Anywhere 正在连接，请稍候")?;
    let auth: Arc<AuthManager> = app.state::<super::osir::OsirAccountState>().auth.clone();
    let status = auth.status();
    let account = status
        .profile
        .as_ref()
        .filter(|p| p.id > 0)
        .map(|p| p.id.to_string())
        .filter(|_| status.connected)
        .ok_or("请先登录 OSIR 客户端，再打开 Anywhere")?;
    let home = anywhere_handoff_trust::native_home()
        .inspect_err(|_| log::warn!("Anywhere handoff stage=transport"))?;
    let bundle = super::osir_tools::macos::handoff_bundle()
        .await
        .inspect_err(|_| log::warn!("Anywhere handoff stage=receiver_verify"))?;
    let trust = anywhere_handoff_trust::fetch()
        .await
        .inspect_err(|_| log::warn!("Anywhere handoff stage=trust_fetch"))?;
    trust
        .verify_bundle(&bundle, "osir-anywhere")
        .await
        .inspect_err(|_| log::warn!("Anywhere handoff stage=receiver_verify"))?;
    // Explicit handoff may prepare/start DSH, but readiness below stays read-only.
    let dsh_background = super::osir_tools::macos::ensure_dsh_background(app).await;
    let tools = configured_tools(app, dsh_background).await;
    if auth.status().generation != status.generation || !auth.status().connected {
        return Err("账号已改变，请重新打开 Anywhere".into());
    }
    let transport = HandoffTransport::bind(&home)
        .inspect_err(|_| log::warn!("Anywhere handoff stage=transport"))
        .map_err(|_| "无法创建本机安全连接，请重试")?;
    super::osir_tools::macos::launch_handoff_bundle(&bundle, transport.flow_id())
        .await
        .inspect_err(|_| log::warn!("Anywhere handoff stage=app_launch"))?;
    let receiver = Receiver { bundle, trust };
    let (cancel, rx) = tokio::sync::watch::channel(false);
    let authorization = auth.clone();
    let generation = status.generation;
    let operation = transport.serve(
        &receiver,
        |request| async move {
            authorization
                .authorize_anywhere_handoff(&request, generation, &account)
                .await
                .map_err(|_| TransportError::AuthorizationFailed)
        },
        tools,
        rx,
    );
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
    result
        .inspect_err(|_| log::warn!("Anywhere handoff stage=transport"))
        .map_err(|error| match error {
            TransportError::PeerUntrusted => "Anywhere 版本无法验证，请通过客户端修复安装",
            TransportError::Cancelled => "账号已改变，已取消本次 Anywhere 连接",
            TransportError::AuthorizationFailed => "Anywhere 账号连接未完成，请检查服务状态后重试",
            TransportError::Timeout => "Anywhere 连接超时，请重新打开",
            _ => "Anywhere 安全连接未完成，请重新打开",
        })?;
    Ok(super::osir_tools::OsirToolLaunch {
        isolated: crate::distribution::is_isolated(),
        already_running: false,
        runtime_status: super::osir_tools::ToolRuntimeStatus::Running,
        foreground: true,
        exit_code: None,
        restarted: false,
    })
}

async fn configured_tools(
    app: &tauri::AppHandle,
    dsh_background: Result<bool, String>,
) -> Vec<String> {
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
            if !dsh_background? {
                return Ok(false);
            }
            // Readiness must never install/configure a plugin, start DSH or
            // foreground an existing window. The explicit DSH action owns that.
            let status = super::osir_tools::handoff_dsh_runtime_status(app).await;
            dsh_readiness(status, super::osir_tools::handoff_anywhere_bridge_ready()).await
        },
    )
    .await
}

/// A running Desktop plus a matching installed bridge is only a bootstrap hint;
/// Anywhere still checks actual bridge reachability when starting the runtime.
async fn dsh_readiness<F>(
    status: super::osir_tools::ToolRuntimeStatus,
    bridge: F,
) -> Result<bool, String>
where
    F: Future<Output = Result<bool, String>>,
{
    match status {
        super::osir_tools::ToolRuntimeStatus::Running => bridge.await,
        super::osir_tools::ToolRuntimeStatus::Stopped => {
            log::info!("Anywhere handoff: dsh pending user startup");
            Ok(false)
        }
        super::osir_tools::ToolRuntimeStatus::Unavailable => Err("DSH 运行状态暂不可确认".into()),
    }
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
            Err(_) => log::warn!("Anywhere handoff stage=tool_probe"),
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn dsh_probe_reuses_a_ready_bridge_and_never_blocks_other_tools() {
        use super::super::osir_tools::ToolRuntimeStatus::{Running, Stopped, Unavailable};
        for (status, bridge, expected_calls, expected_dsh) in [
            (Stopped, Ok(true), 0, false),
            (Unavailable, Ok(true), 0, false),
            (Running, Ok(true), 1, true),
            (Running, Ok(false), 1, false),
            (Running, Err("private fixture error".to_owned()), 1, false),
        ] {
            let calls = AtomicUsize::new(0);
            let tools = collect_configured_tools(
                async { Ok(true) },
                async { Ok(true) },
                dsh_readiness(status, async {
                    calls.fetch_add(1, Ordering::Relaxed);
                    bridge
                }),
            )
            .await;
            let expected = if expected_dsh {
                vec!["claude", "codex", "dsh"]
            } else {
                vec!["claude", "codex"]
            };
            assert_eq!(tools, expected);
            assert_eq!(calls.load(Ordering::Relaxed), expected_calls);
        }
    }

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
