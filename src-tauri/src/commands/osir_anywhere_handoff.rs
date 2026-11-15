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

#[derive(Debug)]
struct ApprovalRecovery {
    awaiting_approval: bool,
    running_instance_seen: bool,
}

impl ApprovalRecovery {
    fn new(awaiting_approval: bool) -> Self {
        Self {
            awaiting_approval,
            running_instance_seen: false,
        }
    }

    fn record_launch(&mut self, status: super::osir_tools::macos::HandoffLaunchStatus) {
        self.awaiting_approval |=
            status == super::osir_tools::macos::HandoffLaunchStatus::AwaitingSystemApproval;
    }

    fn should_retry_for_running_instance(&mut self, running: bool) -> bool {
        if !self.awaiting_approval {
            return false;
        }
        if !running {
            return false;
        }
        if self.running_instance_seen {
            return false;
        }
        self.running_instance_seen = true;
        true
    }
}

enum RecoveryStep<T> {
    Completed(T),
    PeerConnected,
    TransportFinished(Result<(), TransportError>),
}

// A native probe/open may take seconds. Keep the socket, its total deadline and
// account cancellation live while it runs, and stop recovery upon a trusted peer.
async fn await_recovery_step<T, S, O, C>(
    step: S,
    mut operation: Pin<&mut O>,
    mut peer_connected: tokio::sync::watch::Receiver<bool>,
    account_current: C,
) -> RecoveryStep<T>
where
    S: Future<Output = T>,
    O: Future<Output = Result<(), TransportError>>,
    C: Fn() -> bool,
{
    tokio::pin!(step);
    let mut cancellation_tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        if !account_current() {
            return RecoveryStep::TransportFinished(Err(TransportError::Cancelled));
        }
        if *peer_connected.borrow() {
            return RecoveryStep::PeerConnected;
        }
        tokio::select! {
            biased;
            _ = cancellation_tick.tick() => {},
            result = &mut operation => return RecoveryStep::TransportFinished(result),
            _ = peer_connected.changed() => {},
            result = &mut step => return RecoveryStep::Completed(result),
        }
    }
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
    let bundle_for_retry = bundle.clone();
    let flow_id = transport.flow_id().to_owned();
    let peer_connected = transport.peer_connected();
    let launch_status = super::osir_tools::macos::launch_handoff_bundle(&bundle, &flow_id)
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
    let retry_after = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut recovery_tick = tokio::time::interval(Duration::from_secs(1));
    recovery_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut retry_sent = false;
    let mut approval = ApprovalRecovery::new(
        launch_status == super::osir_tools::macos::HandoffLaunchStatus::AwaitingSystemApproval,
    );
    let result = loop {
        tokio::select! {
            biased;
            result = &mut operation => break result,
            _ = recovery_tick.tick(), if !*peer_connected.borrow() => {
                let current = auth.status();
                if !current.connected || current.generation != status.generation {
                    let _ = cancel.send(true);
                    continue;
                }
                if approval.awaiting_approval {
                    let running = match await_recovery_step(
                        super::osir_tools::macos::handoff_bundle_running(app, &bundle_for_retry),
                        operation.as_mut(), peer_connected.clone(),
                        || { let current = auth.status(); current.connected && current.generation == status.generation },
                    ).await {
                        RecoveryStep::Completed(result) => result.unwrap_or(false),
                        RecoveryStep::PeerConnected => continue,
                        RecoveryStep::TransportFinished(result) => break result,
                    };
                    if !approval.should_retry_for_running_instance(running) {
                        continue;
                    }
                } else {
                    if retry_sent || tokio::time::Instant::now() < retry_after {
                        continue;
                    }
                    retry_sent = true;
                }
                // Gatekeeper's "Open Anyway" can launch a plain app instance
                // after the original launch request was denied. Re-submit the
                // same short-lived flow once so the running instance receives
                // it through Electron's single-instance handoff.
                let retry_status = match await_recovery_step(
                    super::osir_tools::macos::launch_handoff_bundle(&bundle_for_retry, &flow_id),
                    operation.as_mut(), peer_connected.clone(),
                    || { let current = auth.status(); current.connected && current.generation == status.generation },
                ).await {
                    RecoveryStep::Completed(result) => result.inspect_err(|_| log::warn!("Anywhere handoff stage=app_launch_retry"))?,
                    RecoveryStep::PeerConnected => continue,
                    RecoveryStep::TransportFinished(result) => break result,
                };
                approval.record_launch(retry_status);
            }
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
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn fallback_policy_denial_allows_first_running_instance_replay() {
        use super::super::osir_tools::macos::HandoffLaunchStatus::{
            AwaitingSystemApproval, Opened,
        };
        let mut recovery = ApprovalRecovery::new(false);
        recovery.record_launch(Opened);
        assert!(!recovery.should_retry_for_running_instance(true));
        // The eight-second fallback is denied. No stopped observation is
        // required before the user permits/opens the target through macOS.
        recovery.record_launch(AwaitingSystemApproval);
        assert!(recovery.should_retry_for_running_instance(true));
        assert!(!recovery.should_retry_for_running_instance(true));
    }

    #[test]
    fn policy_replay_is_once_even_when_runtime_probe_flaps() {
        use super::super::osir_tools::macos::HandoffLaunchStatus::AwaitingSystemApproval;
        let mut recovery = ApprovalRecovery::new(true);
        assert!(!recovery.should_retry_for_running_instance(false));
        assert!(recovery.should_retry_for_running_instance(true));
        recovery.record_launch(AwaitingSystemApproval);
        assert!(!recovery.should_retry_for_running_instance(false));
        assert!(!recovery.should_retry_for_running_instance(true));
    }

    #[tokio::test(start_paused = true)]
    async fn recovery_keeps_transport_progressing_during_slow_native_action() {
        let (_connected, peer) = tokio::sync::watch::channel(false);
        let operation = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(())
        };
        tokio::pin!(operation);
        let late_launch = Cell::new(false);
        let result = await_recovery_step(
            async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                late_launch.set(true);
            },
            operation.as_mut(),
            peer,
            || true,
        )
        .await;
        assert!(matches!(result, RecoveryStep::TransportFinished(Ok(()))));
        assert!(!late_launch.get());
    }

    #[tokio::test(start_paused = true)]
    async fn recovery_cancels_slow_native_action_within_account_poll_budget() {
        let (_connected, peer) = tokio::sync::watch::channel(false);
        let operation = std::future::pending::<Result<(), TransportError>>();
        tokio::pin!(operation);
        let current = Cell::new(true);
        let late_launch = Cell::new(false);
        let started = tokio::time::Instant::now();
        let recovery = await_recovery_step(
            async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                late_launch.set(true);
            },
            operation.as_mut(),
            peer,
            || current.get(),
        );
        let change_account = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            current.set(false);
        };
        let (result, _) = tokio::join!(recovery, change_account);
        assert!(matches!(
            result,
            RecoveryStep::TransportFinished(Err(TransportError::Cancelled))
        ));
        assert!(started.elapsed() <= Duration::from_millis(250));
        assert!(!late_launch.get());
    }

    #[tokio::test(start_paused = true)]
    async fn recovery_does_not_poll_action_after_account_already_changed() {
        let (_connected, peer) = tokio::sync::watch::channel(false);
        let operation = std::future::pending::<Result<(), TransportError>>();
        tokio::pin!(operation);
        let result = await_recovery_step(
            async { panic!("cancelled launch must never start") },
            operation.as_mut(),
            peer,
            || false,
        )
        .await;
        assert!(matches!(
            result,
            RecoveryStep::TransportFinished(Err(TransportError::Cancelled))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn trusted_peer_stops_pending_replay_without_dropping_transport() {
        let (connected, peer) = tokio::sync::watch::channel(false);
        let operation = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            connected.send(true).unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(())
        };
        tokio::pin!(operation);
        let result = await_recovery_step(
            std::future::pending::<()>(),
            operation.as_mut(),
            peer,
            || true,
        )
        .await;
        assert!(matches!(result, RecoveryStep::PeerConnected));
        assert_eq!(operation.await, Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn recovery_cannot_extend_transport_deadline() {
        let (_connected, peer) = tokio::sync::watch::channel(false);
        let operation = async {
            tokio::time::sleep(Duration::from_secs(300)).await;
            Err(TransportError::Timeout)
        };
        tokio::pin!(operation);
        let started = tokio::time::Instant::now();
        let result = await_recovery_step(
            std::future::pending::<()>(),
            operation.as_mut(),
            peer,
            || true,
        )
        .await;
        assert!(matches!(
            result,
            RecoveryStep::TransportFinished(Err(TransportError::Timeout))
        ));
        assert_eq!(started.elapsed(), Duration::from_secs(300));
    }

    #[tokio::test]
    async fn recovery_returns_completed_native_result() {
        let (_connected, peer) = tokio::sync::watch::channel(false);
        let operation = std::future::pending::<Result<(), TransportError>>();
        tokio::pin!(operation);
        let result = await_recovery_step(async { 42 }, operation.as_mut(), peer, || true).await;
        assert!(matches!(result, RecoveryStep::Completed(42)));
    }

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
