use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumeResult {
    pub status: &'static str,
    pub target: &'static str,
    pub restored: bool,
    pub message: &'static str,
}

/// Only provider/session/source selectors cross IPC. Source ownership, ID,
/// project directory and every executable argument are resolved natively.
#[tauri::command]
pub async fn resume_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::store::AppState>,
    provider_id: String,
    session_id: String,
    source_path: String,
    target: String,
) -> Result<SessionResumeResult, String> {
    if !matches!(target.as_str(), "cli" | "desktop") {
        return Err("请选择会话原来的工具类型".into());
    }
    let cli = target == "cli";
    let plan = tauri::async_runtime::spawn_blocking(move || {
        crate::session_manager::resume::prepare(&provider_id, &session_id, &source_path, cli)
    })
    .await
    .map_err(|_| "会话续聊检查未完成，请稍后重试")??;
    if cli {
        let receipt = crate::commands::misc::resume_current_osir_cli(&state, &plan).await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match receipt.state() {
                crate::commands::misc::CliLaunchState::Exited { code, .. } if code != 0 => {
                    return Err("续聊命令已退出，请在终端中检查工具提示后重试".into())
                }
                crate::commands::misc::CliLaunchState::Started { .. }
                | crate::commands::misc::CliLaunchState::Exited { .. } => break,
                _ if tokio::time::Instant::now() >= deadline => break,
                _ => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        return Ok(SessionResumeResult {
            status: "request_sent",
            target: "cli",
            restored: false,
            message: "已在新终端发送该会话的续聊命令，请在工具中确认会话已恢复",
        });
    }
    if plan.provider_id != "codex" || !plan.desktop_origin {
        return Err(
            "此记录属于命令行工具，不能映射为桌面客户端会话；请在原命令行工具中续聊".into(),
        );
    }
    crate::commands::osir_tools::request_desktop_session(&app, &plan.session_id).await?;
    Ok(SessionResumeResult {
        status: "request_sent",
        target: "desktop",
        restored: false,
        message:
            "已向 Codex Desktop 发送该会话的打开请求，客户端未提供恢复确认，请核对已打开的会话",
    })
}
