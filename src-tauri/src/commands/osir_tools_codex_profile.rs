// Codex's installed Electron app supports the same independent launch contract:
// CODEX_HOME + CODEX_ELECTRON_USER_DATA_PATH + --user-data-dir. The isolated
// profile is never inferred from an arbitrary existing Codex window.
#[derive(Debug)]
struct CodexDevProfile {
    home: PathBuf,
    codex_home: PathBuf,
    user_data_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexProfileKey {
    bundle: PathBuf,
    user_data_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexOwnedState {
    Running,
    Exited,
    Changed,
}

struct CodexPendingSnapshot {
    key: CodexProfileKey,
    pid: i32,
    state: CodexOwnedState,
}

fn codex_pending_pid(
    key: &CodexProfileKey,
    pending: Option<&CodexPendingSnapshot>,
) -> Result<Option<i32>, String> {
    match pending.filter(|pending| &pending.key == key) {
        None => Ok(None),
        Some(pending) => match pending.state {
            CodexOwnedState::Running => Ok(Some(pending.pid)),
            CodexOwnedState::Exited => Ok(None),
            CodexOwnedState::Changed => {
                Err("Codex 开发窗口的启动状态暂不能确认，请检查该窗口，关闭后重试".into())
            }
        },
    }
}

#[derive(Clone)]
struct CodexOwnedLaunch {
    key: CodexProfileKey,
    pid: i32,
    birth: Option<(u64, u64)>,
    executable: PathBuf,
    started: Instant,
    exited: tokio::sync::watch::Receiver<bool>,
}

impl CodexOwnedLaunch {
    fn snapshot(&self) -> CodexPendingSnapshot {
        let state = if *self.exited.borrow() || !process_is_alive(self.pid) {
            CodexOwnedState::Exited
        } else if self.birth.is_some()
            && process_birth(self.pid) == self.birth
            && process_matches(self.pid, &self.executable)
        {
            CodexOwnedState::Running
        } else {
            CodexOwnedState::Changed
        };
        CodexPendingSnapshot {
            key: self.key.clone(),
            pid: self.pid,
            state,
        }
    }

    fn waiting_message(&self) -> String {
        if *self.exited.borrow() || !process_is_alive(self.pid) {
            "Codex 开发窗口已退出，请检查安装后重试".into()
        } else if self.started.elapsed() >= Duration::from_secs(30) {
            "Codex 开发窗口仍未完成启动，请检查该窗口；如需重试，请先关闭该开发窗口".into()
        } else {
            "Codex 开发窗口正在启动，请稍后再点运行中检查；不会重复打开窗口".into()
        }
    }
}

// A successful spawn owns an exact Child/PID even before Electron creates its
// singleton lock. Keep that receipt across confirmation timeouts and retries.
static CODEX_OWNED_LAUNCHES: std::sync::Mutex<Vec<CodexOwnedLaunch>> =
    std::sync::Mutex::new(Vec::new());

fn owned_codex_launch(key: &CodexProfileKey) -> Result<Option<CodexOwnedLaunch>, String> {
    let mut launches = CODEX_OWNED_LAUNCHES
        .lock()
        .map_err(|_| "Codex 启动状态暂不可用，请重新打开 OSIRAPI 客户端")?;
    // Retain the requested key until the caller has observed its exit, so a
    // stale lock cannot silently adopt that owned PID after it gets recycled.
    launches.retain(|launch| &launch.key == key || !*launch.exited.borrow());
    Ok(launches.iter().find(|launch| &launch.key == key).cloned())
}

fn forget_codex_launch(key: &CodexProfileKey) -> Result<(), String> {
    CODEX_OWNED_LAUNCHES
        .lock()
        .map_err(|_| "Codex 启动状态暂不可用，请重新打开 OSIRAPI 客户端")?
        .retain(|launch| &launch.key != key);
    Ok(())
}

fn codex_profile_bundle_choice(preferred: &Path, running: Option<&Path>) -> PathBuf {
    running.unwrap_or(preferred).to_path_buf()
}

struct CodexProfileProcess {
    bundle: PathBuf,
    pid: i32,
    birth: (u64, u64),
    installation: DesktopInstallation,
}

async fn checked_codex_executable(bundle: &Path, timeout: Duration) -> Result<PathBuf, String> {
    let plist = bundle.join("Contents/Info.plist");
    let metadata =
        std::fs::symlink_metadata(&plist).map_err(|_| "Codex 安装状态已改变，请重新检查")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > METADATA_LIMIT
        || !plist
            .canonicalize()
            .is_ok_and(|path| path.starts_with(bundle))
    {
        return Err("Codex 安装状态已改变，请重新检查".into());
    }
    let output = system_output(
        "/usr/bin/plutil",
        &["-convert", "json", "-o", "-", "--"],
        &plist,
        timeout,
    )
    .await
    .map_err(|_| "Codex 安装状态已改变，请重新检查")?;
    if !output.status.success() || output.stdout.len() > 8 * METADATA_LIMIT as usize {
        return Err("Codex 安装状态已改变，请重新检查".into());
    }
    let metadata: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "Codex 安装状态已改变，请重新检查")?;
    if !matches!(
        installation_from_metadata(bundle, "codex", &metadata),
        Ok(Some(_))
    ) {
        return Err("Codex 安装状态已改变，请重新检查".into());
    }
    validate_executable(bundle, &metadata).map_err(|_| "Codex 安装状态已改变，请重新检查".into())
}

/// A profile can outlive a change in the preferred installed bundle. Resolve
/// its exact lock/owned PID only against native candidates; never infer another
/// Codex window's profile from identity alone or fall back to that window.
async fn codex_profile_process(
    app: &tauri::AppHandle,
    profile: &CodexDevProfile,
) -> Result<Option<CodexProfileProcess>, String> {
    let profile_path = profile.user_data_dir.canonicalize().ok();
    let owned = CODEX_OWNED_LAUNCHES
        .lock()
        .map_err(|_| "Codex 启动状态暂不可用，请重新打开 OSIRAPI 客户端")?
        .iter()
        .find(|launch| {
            profile_path.as_ref() == Some(&launch.key.user_data_dir) && !*launch.exited.borrow()
        })
        .cloned();
    let owned_pid = match owned.as_ref().map(CodexOwnedLaunch::snapshot) {
        Some(snapshot) => codex_pending_pid(&snapshot.key, Some(&snapshot))?,
        None => None,
    };
    let lock_pid = profile.running_pid();
    if owned_pid.is_some() && lock_pid.is_some() && owned_pid != lock_pid {
        return Err("Codex 开发配置被其他进程占用，请检查后重试".into());
    }
    let Some(pid) = lock_pid.or(owned_pid) else {
        return Ok(None);
    };
    let error = "Codex 开发配置被其他进程占用，请检查后重试";
    let birth = process_birth(pid).ok_or(error)?;
    let executable = process_path(pid)
        .and_then(|path| path.canonicalize().ok())
        .ok_or(error)?;
    let deadline = Instant::now() + DESKTOP_BUDGET;
    let mut seen = HashSet::new();
    for candidate in candidates("codex") {
        // Reject symlink bundles consistently with normal native discovery.
        let Ok(metadata) = std::fs::symlink_metadata(&candidate) else {
            continue;
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let Ok(bundle) = candidate.canonicalize() else {
            continue;
        };
        if !seen.insert(bundle.clone()) || !executable.starts_with(bundle.join("Contents/MacOS")) {
            continue;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err("Codex 开发窗口检测超时，请重试".into());
        };
        let expected =
            checked_codex_executable(&bundle, remaining.min(Duration::from_secs(1))).await?;
        if expected != executable || process_birth(pid) != Some(birth) {
            return Err(error.into());
        }
        // An owned Child may not be registered with LaunchServices yet; keep
        // its precise receipt so a slow startup still cannot spawn a duplicate.
        if owned_pid == Some(pid) || {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("Codex 开发窗口检测超时，请重试")?;
            tokio::time::timeout(
                remaining,
                desktop_process_snapshot(app, "codex", &bundle, Some(pid)),
            )
            .await
            .map_err(|_| "Codex 开发窗口检测超时，请重试")??
            .iter()
            .any(|found| found.pid == pid)
        } {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err("Codex 开发窗口检测超时，请重试".into());
            };
            let installation =
                inspect_bundle(&candidate, "codex", remaining.min(Duration::from_secs(1)))
                    .await
                    .map_err(|_| error)?
                    .ok_or(error)?;
            if process_birth(pid) != Some(birth) {
                return Err(error.into());
            }
            return Ok(Some(CodexProfileProcess {
                bundle,
                pid,
                birth,
                installation,
            }));
        }
        return Err(error.into());
    }
    Err(error.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexProfileAction {
    Start,
    Activate(i32),
    Restart(i32),
}

fn codex_profile_action(
    pid: Option<i32>,
    matches_bundle: bool,
    restart: bool,
) -> Result<CodexProfileAction, String> {
    let Some(pid) = pid else {
        return Ok(CodexProfileAction::Start);
    };
    if !matches_bundle {
        return Err("Codex 开发配置被其他进程占用，请检查后重试".into());
    }
    // A profile lock is accepted only after exact bundle/PID ownership checks.
    // The caller revalidates process birth and executable before termination.
    validate_restart_target(pid)?;
    Ok(if restart {
        CodexProfileAction::Restart(pid)
    } else {
        CodexProfileAction::Activate(pid)
    })
}

impl CodexDevProfile {
    fn new(home: PathBuf, codex_home: PathBuf, sandbox: Option<PathBuf>) -> Result<Self, String> {
        let error = "Codex 开发配置目录不可用，请重新启动 OSIRAPI 客户端";
        if !home.is_absolute()
            || !home.is_dir()
            || sandbox.as_ref() != Some(&home)
            || !codex_home.is_absolute()
            || !codex_home.starts_with(&home)
            || [&home, &codex_home]
                .iter()
                .any(|p| p.to_str().is_none_or(|p| p.chars().any(char::is_control)))
        {
            return Err(error.into());
        }
        let user_data_dir = codex_home.join("osir-desktop-profile");
        let canonical_home = home.canonicalize().map_err(|_| error)?;
        for path in [&codex_home, &user_data_dir] {
            let mut existing = path.as_path();
            while !existing.exists() {
                existing = existing.parent().ok_or(error)?;
            }
            if !existing
                .canonicalize()
                .map_err(|_| error)?
                .starts_with(&canonical_home)
            {
                return Err(error.into());
            }
            if let Ok(meta) = std::fs::symlink_metadata(path) {
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    return Err(error.into());
                }
            }
        }
        Ok(Self {
            home,
            codex_home,
            user_data_dir,
        })
    }

    fn command(&self, executable: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(executable);
        command
            .arg(format!("--user-data-dir={}", self.user_data_dir.display()))
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LC_ALL", "en_US.UTF-8")
            .env("HOME", &self.home)
            .env("CODEX_HOME", &self.codex_home)
            .env("CODEX_ELECTRON_USER_DATA_PATH", &self.user_data_dir)
            .current_dir(&self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        command
    }

    fn running_pid(&self) -> Option<i32> {
        let lock = std::fs::read_link(self.user_data_dir.join("SingletonLock")).ok()?;
        let pid = lock
            .to_str()?
            .rsplit_once('-')?
            .1
            .parse::<i32>()
            .ok()
            .filter(|pid| *pid > 1)?;
        process_is_alive(pid).then_some(pid)
    }
}

async fn launch_codex_dev(
    app: &tauri::AppHandle,
    bundle: &Path,
    restart: bool,
    expected: Option<(i32, (u64, u64))>,
) -> Result<OsirToolLaunch, String> {
    let profile = CodexDevProfile::new(
        crate::config::get_home_dir(),
        crate::codex_config::get_codex_config_dir(),
        std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from),
    )?;
    let executable = checked_codex_executable(bundle, Duration::from_secs(1)).await?;
    std::fs::create_dir_all(&profile.user_data_dir)
        .map_err(|_| "无法创建 Codex 开发配置目录，请检查权限")?;
    std::fs::set_permissions(
        &profile.user_data_dir,
        std::fs::Permissions::from_mode(0o700),
    )
    .map_err(|_| "无法创建 Codex 开发配置目录，请检查权限")?;
    let key = CodexProfileKey {
        bundle: bundle.to_path_buf(),
        user_data_dir: profile
            .user_data_dir
            .canonicalize()
            .map_err(|_| "Codex 开发配置目录不可用，请重新启动 OSIRAPI 客户端")?,
    };
    let owned = owned_codex_launch(&key)?;
    let pending = owned.as_ref().map(CodexOwnedLaunch::snapshot);
    let owned_pid = codex_pending_pid(&key, pending.as_ref())?;
    let lock_pid = profile.running_pid();
    if pending.as_ref().is_some_and(|pending| {
        pending.state == CodexOwnedState::Exited && lock_pid == Some(pending.pid)
    }) {
        return Err("上次 Codex 开发进程已退出，但配置仍被其他进程占用，请检查后重试".into());
    }
    if matches!(
        pending.as_ref().map(|pending| pending.state),
        Some(CodexOwnedState::Exited)
    ) {
        forget_codex_launch(&key)?;
    }
    if owned_pid.is_some() && lock_pid.is_some() && owned_pid != lock_pid {
        return Err("Codex 开发配置被其他进程占用，请检查后重试".into());
    }
    let before = lock_pid.or(owned_pid);
    if expected.is_some_and(|(pid, birth)| before != Some(pid) || process_birth(pid) != Some(birth))
    {
        return Err("Codex 开发窗口状态已变化，请重新检查".into());
    }
    let matches_bundle = if let Some(pid) = before {
        if owned_pid == Some(pid) {
            true // Exact Child/PID, executable and process birth were verified.
        } else {
            desktop_snapshot(app, "codex", bundle, Some(pid))
                .await?
                .iter()
                .any(|found| found.pid == pid)
        }
    } else {
        false
    };
    let action = codex_profile_action(before, matches_bundle, restart)?;
    match action {
        CodexProfileAction::Activate(pid) => {
            // The exact owned process also covers slow startup before a
            // SingletonLock exists. A retry must never spawn a second app.
            return confirm_desktop_launch(app, "codex", bundle, Some(pid), true, true)
                .await
                .map_err(|error| {
                    owned
                        .as_ref()
                        .map_or(error, CodexOwnedLaunch::waiting_message)
                });
        }
        CodexProfileAction::Restart(pid) => {
            let current = profile.running_pid();
            if current.is_some_and(|current| current != pid)
                || (owned_pid != Some(pid) && current != Some(pid))
                || expected.is_some_and(|(_, birth)| process_birth(pid) != Some(birth))
            {
                return Err("Codex 开发窗口状态已变化，请重新检查".into());
            }
            let birth = expected
                .filter(|(expected_pid, _)| *expected_pid == pid)
                .map(|(_, birth)| birth)
                .ok_or("Codex 开发窗口状态已变化，请重新检查")?;
            terminate_known(app, "codex", bundle, &executable, &[(pid, birth)], true).await?;
            forget_codex_launch(&key)?;
        }
        CodexProfileAction::Start => (),
    }
    // An external launch may have acquired this profile while the previous
    // instance was closing. Never open another instance over that process.
    if profile.running_pid().is_some() {
        return Err("Codex 开发窗口状态已变化，请重新检查".into());
    }
    let mut child = profile
        .command(&executable)
        .spawn()
        .map_err(|_| "无法启动 Codex 开发窗口，请检查安装后重试")?;
    let pid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or("无法确认 Codex 开发窗口进程，请检查后重试")?;
    let birth = process_birth(pid);
    let (exit_tx, exited) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let _ = child.wait().await;
        let _ = exit_tx.send(true);
    });
    let launch = CodexOwnedLaunch {
        key,
        pid,
        birth,
        executable,
        started: Instant::now(),
        exited,
    };
    CODEX_OWNED_LAUNCHES
        .lock()
        .map_err(|_| "Codex 启动状态暂不可用，请检查刚打开的开发窗口")?
        .push(launch.clone());
    if codex_pending_pid(&launch.key, Some(&launch.snapshot()))?.is_none() {
        return Err(launch.waiting_message());
    }
    let mut result = confirm_desktop_launch(app, "codex", bundle, Some(pid), true, false)
        .await
        .map_err(|_| launch.waiting_message())?;
    result.restarted = matches!(action, CodexProfileAction::Restart(_));
    Ok(result)
}
