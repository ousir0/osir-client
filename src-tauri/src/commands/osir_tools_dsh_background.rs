// Client handoff uses the existing Desktop/bridge, never a second runtime.
// macOS open(1): -g avoids foreground activation; -j requests hidden launch.
const DSH_BACKGROUND_OPEN_ARGS: &[&str] = &["-g", "-j", "-a"];

async fn start_dsh_background<P, L, LF>(
    already_running: bool,
    prepare: P,
    launch: L,
) -> Result<(), String>
where
    P: std::future::Future<Output = Result<(), String>>,
    L: FnOnce(&'static [&'static str]) -> LF,
    LF: std::future::Future<Output = Result<(), String>>,
{
    if already_running {
        return Ok(());
    }
    prepare.await?;
    launch(DSH_BACKGROUND_OPEN_ARGS).await
}

/// Separate from the read-only readiness probe and the user's foreground DSH
/// action. A live instance is never opened, activated, hidden or reconfigured.
pub(in crate::commands) async fn ensure_dsh_background(
    app: &tauri::AppHandle,
) -> Result<bool, String> {
    if crate::distribution::is_isolated()
        || crate::dsh_config::current_selection()
            .map_err(|_| "DSH 模型配置无法读取")?
            .is_none()
    {
        return Ok(false);
    }
    let _guard = desktop_action_guard("dsh")?;
    let running = current_desktop_instance(app, "dsh").await?.is_some();
    start_dsh_background(
        running,
        async {
            let bundle = checked_launch_path("osir-anywhere").await?;
            let dsh_bundle = checked_launch_path("dsh").await?;
            let cli =
                crate::dsh_config::desktop_cli(&dsh_bundle).map_err(|error| error.to_string())?;
            crate::dsh_config::prepare_desktop_bridge(
                &bundle.join("Contents/Resources"),
                &cli,
                false,
            )
            .await
            .map_err(|error| error.to_string())
        },
        |args| async move {
            // Installation can take time; do not reopen an instance the user
            // started meanwhile. No -n, restart, URL or foreground confirmation.
            if current_desktop_instance(app, "dsh").await?.is_some() {
                return Ok(());
            }
            let path = checked_launch_path("dsh").await?;
            let output = system_output("/usr/bin/open", args, &path, Duration::from_secs(5))
                .await
                .map_err(|_| "DSH 后台启动未完成，请重试")?;
            if !output.status.success() {
                return Err("DSH 后台启动未完成，请重试".into());
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(instance) = current_desktop_instance(app, "dsh").await? {
                    if instance.bundle() != path.as_path() {
                        return Err("DSH 运行状态已改变，请重新检测".into());
                    }
                    validate_desktop_action_target("dsh", &instance)?;
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err("DSH 后台启动尚未就绪，请稍后重试".into());
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        },
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
#[path = "osir_tools_dsh_background_tests.rs"]
mod dsh_background_tests;
