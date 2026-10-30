// Desktop-only process ownership. No account/configuration writes, process
// arguments, environment or window titles are read here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopScope {
    Profile,
    Registered,
}

struct DesktopInstance {
    installation: DesktopInstallation,
    executable: PathBuf,
    pid: i32,
    birth: (u64, u64),
    scope: DesktopScope,
}

impl DesktopInstance {
    fn bundle(&self) -> &Path {
        Path::new(&self.installation.path)
    }
}

fn validate_desktop_instance_identity(
    expected_pid: i32,
    expected_birth: (u64, u64),
    actual_pid: i32,
    actual_birth: Option<(u64, u64)>,
    executable_matches: bool,
) -> Result<(), String> {
    if expected_pid > 1
        && expected_pid == actual_pid
        && actual_birth == Some(expected_birth)
        && executable_matches
    {
        Ok(())
    } else {
        Err("应用运行状态已变化，请重新打开".into())
    }
}

async fn registered_desktop_instance(
    app: &tauri::AppHandle,
    id: &str,
    profile_pid: Option<i32>,
) -> Result<Option<DesktopInstance>, String> {
    let deadline = Instant::now() + DESKTOP_BUDGET;
    let mut selected = None;
    let mut selected_rank = (false, false);
    let mut failed = false;
    let mut seen = HashSet::new();
    for candidate in candidates(id) {
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                failed = true;
                continue;
            }
            Ok(_) => (),
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("桌面应用检测超时，请重试")?;
        let (installation, executable) =
            match inspect_bundle_details(&candidate, id, remaining.min(Duration::from_secs(1)))
                .await
            {
                Ok(Some(details)) => details,
                Ok(None) => continue,
                Err(_) => {
                    failed = true;
                    continue;
                }
            };
        if !seen.insert(installation.path.clone()) {
            continue;
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("桌面应用检测超时，请重试")?;
        let found = tokio::time::timeout(
            remaining,
            desktop_process_snapshot(app, id, Path::new(&installation.path), profile_pid),
        )
        .await
        .map_err(|_| "桌面应用检测超时，请重试")??;
        let chosen = found
            .into_iter()
            .filter(|evidence| process_matches(evidence.pid, &executable))
            .max_by_key(|evidence| (evidence.active, evidence.window));
        if let Some(evidence) = chosen {
            let birth = process_birth(evidence.pid).ok_or("应用运行状态已变化，请重新打开")?;
            let rank = (evidence.active, evidence.window);
            if selected.is_none() || rank > selected_rank {
                selected_rank = rank;
                selected = Some(DesktopInstance {
                    installation,
                    executable,
                    pid: evidence.pid,
                    birth,
                    scope: if profile_pid.is_some() {
                        DesktopScope::Profile
                    } else {
                        DesktopScope::Registered
                    },
                });
            }
        }
    }
    if selected.is_none() && (profile_pid.is_some() || failed) {
        return Err("无法安全确认桌面应用的运行实例，请重新检测".into());
    }
    Ok(selected)
}

async fn current_desktop_instance(
    app: &tauri::AppHandle,
    id: &str,
) -> Result<Option<DesktopInstance>, String> {
    if id == "codex" && crate::distribution::is_isolated() {
        let profile = CodexDevProfile::new(
            crate::config::get_home_dir(),
            crate::codex_config::get_codex_config_dir(),
            std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from),
        )?;
        if let Some(running) = codex_profile_process(app, &profile).await? {
            let executable =
                checked_codex_executable(&running.bundle, Duration::from_secs(1)).await?;
            return Ok(Some(DesktopInstance {
                installation: running.installation,
                executable,
                pid: running.pid,
                birth: running.birth,
                scope: DesktopScope::Profile,
            }));
        }
        // Registered instances remain distinct from profile ownership.
    }
    let profile_pid = if id == "workbuddy" {
        WorkBuddyProfile::new(
            crate::config::get_home_dir(),
            crate::distribution::is_isolated(),
            std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from),
        )?
        .running_pid()
    } else {
        None
    };
    registered_desktop_instance(app, id, profile_pid).await
}

fn validate_desktop_action_target(id: &str, instance: &DesktopInstance) -> Result<(), String> {
    validate_restart_target(instance.pid)?;
    validate_desktop_instance_identity(
        instance.pid,
        instance.birth,
        instance.pid,
        process_birth(instance.pid),
        process_matches(instance.pid, &instance.executable),
    )?;
    if instance.scope == DesktopScope::Profile {
        let lock_pid = if id == "codex" {
            CodexDevProfile::new(
                crate::config::get_home_dir(),
                crate::codex_config::get_codex_config_dir(),
                std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from),
            )?
            .running_pid()
        } else {
            WorkBuddyProfile::new(
                crate::config::get_home_dir(),
                crate::distribution::is_isolated(),
                std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from),
            )?
            .running_pid()
        };
        if lock_pid.is_some_and(|pid| pid != instance.pid) {
            return Err("应用运行状态已变化，请重新打开".into());
        }
    }
    Ok(())
}

fn desktop_session_url(session_id: &str) -> Result<String, String> {
    crate::session_manager::resume::arguments("codex", session_id)?;
    Ok(format!("codex://threads/{session_id}"))
}

pub(super) async fn request_desktop_session(
    app: &tauri::AppHandle,
    session_id: &str,
) -> Result<(), String> {
    let url = desktop_session_url(session_id)?;
    if crate::distribution::is_isolated() {
        return Err(
            "开发环境不能将本机历史投递到隔离桌面实例，请在原 Codex Desktop 中继续该会话".into(),
        );
    }
    let _guard = desktop_action_guard("codex")?;
    let path = checked_launch_path("codex").await?;
    launch_checked_desktop(app, "codex", &path, false).await?;
    let instance = current_desktop_instance(app, "codex")
        .await?
        .ok_or("未能确认 Codex Desktop 运行实例，请重试")?;
    let mut pids = HashSet::new();
    for candidate in candidates("codex") {
        let Ok(Some((installation, executable))) =
            inspect_bundle_details(&candidate, "codex", Duration::from_secs(1)).await
        else {
            continue;
        };
        for found in
            desktop_process_snapshot(app, "codex", Path::new(&installation.path), None).await?
        {
            if process_matches(found.pid, &executable) {
                pids.insert(found.pid);
            }
        }
    }
    if pids.len() != 1 || !pids.contains(&instance.pid) {
        return Err(
            "检测到多个 Codex Desktop 实例，无法准确投递会话；请在目标客户端中打开该会话".into(),
        );
    }
    let output = system_output(
        "/usr/bin/plutil",
        &["-convert", "json", "-o", "-", "--"],
        &instance.bundle().join("Contents/Info.plist"),
        Duration::from_secs(1),
    )
    .await
    .map_err(|_| "无法确认此 Codex Desktop 的会话协议")?;
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "无法确认此 Codex Desktop 的会话协议")?;
    let protocol = metadata
        .get("CFBundleURLTypes")
        .and_then(Value::as_array)
        .is_some_and(|types| {
            types
                .iter()
                .filter_map(|entry| entry.get("CFBundleURLSchemes").and_then(Value::as_array))
                .flatten()
                .any(|scheme| scheme.as_str() == Some("codex"))
        });
    if !protocol {
        return Err("此 Codex Desktop 未声明会话打开协议，请在客户端中打开该会话".into());
    }
    validate_desktop_instance_identity(
        instance.pid,
        instance.birth,
        instance.pid,
        process_birth(instance.pid),
        process_matches(instance.pid, &instance.executable),
    )?;
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("/usr/bin/open")
            .arg("-a")
            .arg(instance.bundle())
            .arg("--")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .map_err(|_| "Codex Desktop 会话请求超时，请在客户端中确认")?
    .map_err(|_| "无法向 Codex Desktop 发送会话请求")?;
    if !status.success() {
        return Err("Codex Desktop 未接受会话打开请求，请在客户端中打开该会话".into());
    }
    Ok(())
}

async fn activate_desktop_instance(
    app: &tauri::AppHandle,
    id: &str,
    instance: &DesktopInstance,
) -> Result<OsirToolLaunch, String> {
    validate_desktop_instance_identity(
        instance.pid,
        instance.birth,
        instance.pid,
        process_birth(instance.pid),
        process_matches(instance.pid, &instance.executable),
    )?;
    confirm_desktop_launch(
        app,
        id,
        instance.bundle(),
        Some(instance.pid),
        instance.scope == DesktopScope::Profile && crate::distribution::is_isolated(),
        true,
    )
    .await
}

pub(super) async fn close_desktop(
    app: &tauri::AppHandle,
    id: &str,
    force: bool,
) -> Result<OsirToolClose, String> {
    let _guard = desktop_action_guard(id)?;
    let Some(instance) = current_desktop_instance(app, id).await? else {
        return Ok(OsirToolClose {
            runtime_status: ToolRuntimeStatus::Stopped,
            closed: false,
            already_stopped: true,
        });
    };
    validate_desktop_action_target(id, &instance)?;
    if force {
        force_terminate_instance(app, id, &instance).await?;
    } else {
        terminate_known(
            app,
            id,
            instance.bundle(),
            &instance.executable,
            &[(instance.pid, instance.birth)],
            false,
        )
        .await?;
    }
    // Never turn a denied or still-running request into a successful close.
    if process_birth(instance.pid) == Some(instance.birth) && process_is_alive(instance.pid) {
        return Err("应用尚未退出，请保存工作后重试".into());
    }
    Ok(OsirToolClose {
        runtime_status: ToolRuntimeStatus::Stopped,
        closed: true,
        already_stopped: false,
    })
}

/// The UI confirms session interruption before requesting this action. Only
/// the selected native instance is terminated, including when it hosts a chat.
async fn force_terminate_instance(
    app: &tauri::AppHandle,
    id: &str,
    instance: &DesktopInstance,
) -> Result<(), String> {
    validate_desktop_action_target(id, instance)?;
    terminate_known(
        app,
        id,
        instance.bundle(),
        &instance.executable,
        &[(instance.pid, instance.birth)],
        true,
    )
    .await
}
