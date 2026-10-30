//! Bounded, read-only adapter for the existing Desktop metadata contract.
//! Keep Codex candidates/identity aligned with services/osir/codex_desktop and
//! desktop_install; never call the install coordinator (leases/journals/writes).
//! The existing public detect() uses an unbounded plutil.output(), so this
//! inventory shares its types, identity and roots, with a bounded transport.

use super::{
    OsirToolClose, OsirToolDetection, OsirToolLaunch, ToolKind, ToolRuntimeStatus, ToolStatus,
    WorkBuddyLaunch,
};
use crate::services::osir::codex_desktop::DesktopInstallation;
use crate::services::osir::desktop_install::signature_verifier::BUNDLE_ID;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

// Observed in the installed Tencent WorkBuddy macOS bundle's Info.plist. This
// is identity matching, not a claim that metadata discovery verifies a signature.
const WORKBUDDY_BUNDLE_ID: &str = "com.tencent.workbuddy.mac";
// Read from the installed official Claude.app Info.plist, independently of CLI.
const CLAUDE_BUNDLE_ID: &str = "com.anthropic.claudefordesktop";
const METADATA_LIMIT: u64 = 64 * 1024;
const DESKTOP_BUDGET: Duration = Duration::from_secs(3);
// All Codex launch entries, including the installer panel, share this gate.
// Reject overlapping clicks instead of queuing multiple starts or restarts.
static CODEX_LAUNCH_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static CLAUDE_DESKTOP_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static WORKBUDDY_DESKTOP_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn codex_launch_guard() -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    CODEX_LAUNCH_GATE
        .try_lock()
        .map_err(|_| "Codex 正在打开或重启，请稍后重试".into())
}

fn desktop_action_guard(id: &str) -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    match id {
        "codex" => codex_launch_guard(),
        "claude-desktop" => CLAUDE_DESKTOP_GATE
            .try_lock()
            .map_err(|_| "此桌面应用正在启动、重启或关闭，请稍后重试".into()),
        "workbuddy" => WORKBUDDY_DESKTOP_GATE
            .try_lock()
            .map_err(|_| "此桌面应用正在启动、重启或关闭，请稍后重试".into()),
        _ => Err("此工具不支持从客户端关闭桌面应用".into()),
    }
}

include!("osir_tools_macos_runtime.rs");
include!("osir_tools_codex_profile.rs");
include!("osir_tools_desktop_lifecycle.rs");

#[derive(Debug)]
struct InspectionError {
    // A familiar directory name alone cannot establish product identity.
    installed: bool,
}
impl From<()> for InspectionError {
    fn from(_: ()) -> Self {
        Self { installed: false }
    }
}

fn candidates(id: &str) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    candidate_paths(id, &candidate_roots(id, home.as_deref()))
}

fn candidate_roots(id: &str, home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if id == "codex" {
        if let Some(home) = home {
            // Prefer the copy updated by OSIR over another system installation.
            roots.push(home.join("Applications/OSIR Codex"));
        }
    }
    roots.push(PathBuf::from("/Applications"));
    if let Some(home) = home {
        let applications = home.join("Applications");
        roots.push(applications);
    }
    roots
}

fn candidate_paths(id: &str, roots: &[PathBuf]) -> Vec<PathBuf> {
    let names: &[&str] = match id {
        "codex" => &["Codex.app", "ChatGPT.app"],
        "claude-desktop" => &["Claude.app"],
        "workbuddy" => &["WorkBuddy.app", "Tencent WorkBuddy.app"],
        _ => &[],
    };
    roots
        .iter()
        .flat_map(|root| names.iter().map(move |name| root.join(name)))
        .collect()
}

pub(super) async fn detect(id: &str) -> OsirToolDetection {
    detect_candidates(id, &candidates(id)).await
}

async fn detect_candidates(id: &str, paths: &[PathBuf]) -> OsirToolDetection {
    let mut record = OsirToolDetection::new(id, ToolKind::Desktop);
    let deadline = Instant::now() + DESKTOP_BUDGET;
    let mut failed = false;
    let mut present = false;
    for path in paths {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                failed = true;
                continue;
            }
            Ok(_) => {}
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return record.failed(present, "桌面应用检测超时，请重试");
        };
        match inspect_bundle(path, id, remaining.min(Duration::from_secs(1))).await {
            Ok(Some(installation)) => {
                record.installed = true;
                record.status = ToolStatus::Installed;
                record.version = installation.version;
                record.path = Some(installation.path);
                return record;
            }
            // A different application with a familiar name is not this product.
            Ok(None) => {}
            Err(error) => {
                failed = true;
                present |= error.installed;
            }
        }
    }
    if failed {
        record.failed(present, "应用目录存在但无法完成本地检查，请检查安装或重试")
    } else {
        record
    }
}

async fn inspect_bundle(
    path: &Path,
    id: &str,
    timeout: Duration,
) -> Result<Option<DesktopInstallation>, InspectionError> {
    inspect_bundle_details(path, id, timeout)
        .await
        .map(|found| found.map(|(installation, _)| installation))
}

async fn inspect_bundle_details(
    path: &Path,
    id: &str,
    timeout: Duration,
) -> Result<Option<(DesktopInstallation, PathBuf)>, InspectionError> {
    let bundle = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if !bundle.is_dir() || bundle.file_type().is_symlink() {
        return Err(().into());
    }
    let canonical = path.canonicalize().map_err(|_| ())?;
    let plist = path.join("Contents/Info.plist");
    let metadata = std::fs::symlink_metadata(&plist).map_err(|_| ())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > METADATA_LIMIT {
        return Err(().into());
    }
    if !plist
        .canonicalize()
        .map_err(|_| ())?
        .starts_with(&canonical)
    {
        return Err(().into());
    }
    let output = system_output(
        "/usr/bin/plutil",
        &["-convert", "json", "-o", "-", "--"],
        &plist,
        timeout,
    )
    .await?;
    if !output.status.success() || output.stdout.len() > 8 * METADATA_LIMIT as usize {
        return Err(().into());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|_| ())?;
    let Some(installation) = installation_from_metadata(&canonical, id, &metadata)? else {
        return Ok(None);
    };
    let executable = validate_executable(&canonical, &metadata)
        .map_err(|_| InspectionError { installed: true })?;
    Ok(Some((installation, executable)))
}

fn installation_from_metadata(
    path: &Path,
    id: &str,
    metadata: &Value,
) -> Result<Option<DesktopInstallation>, InspectionError> {
    let expected_id = desktop_identity(id).ok_or(())?;
    let bundle_id = metadata
        .get("CFBundleIdentifier")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or(())?;
    if bundle_id != expected_id {
        return Ok(None);
    }
    validate_executable(path, metadata).map_err(|_| InspectionError { installed: true })?;
    let path = path
        .to_str()
        .filter(|p| !p.chars().any(char::is_control))
        .ok_or(InspectionError { installed: true })?;
    Ok(Some(DesktopInstallation {
        path: path.into(),
        version: version_field(metadata, "CFBundleShortVersionString"),
        build: version_field(metadata, "CFBundleVersion"),
    }))
}

fn validate_executable(path: &Path, metadata: &Value) -> Result<PathBuf, ()> {
    // WorkBuddy currently ships CFBundleExecutable=Electron. Accept a single
    // metadata basename, never a shell line or an arbitrary path.
    let name = metadata
        .get("CFBundleExecutable")
        .and_then(Value::as_str)
        .ok_or(())?;
    if !safe_executable_name(name) {
        return Err(());
    }
    let real = path
        .join("Contents/MacOS")
        .join(name)
        .canonicalize()
        .map_err(|_| ())?;
    let executable_metadata = std::fs::metadata(&real).map_err(|_| ())?;
    if !real.starts_with(path)
        || !executable_metadata.is_file()
        || executable_metadata.permissions().mode() & 0o111 == 0
    {
        return Err(());
    }
    Ok(real)
}

fn version_field(metadata: &Value, key: &str) -> Option<String> {
    let value = match metadata.get(key)? {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!value.is_empty()
        && value.len() <= 64
        && value.chars().any(|c| c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_')))
    .then_some(value)
}

fn safe_executable_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    !name.is_empty()
        && !name.chars().any(char::is_control)
        && !name.contains(['/', '\\'])
        && matches!(parts.next(), Some(Component::Normal(_)))
        && parts.next().is_none()
}

async fn system_output(
    program: &str,
    args: &[&str],
    target: &Path,
    timeout: Duration,
) -> Result<std::process::Output, ()> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .arg(target)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Only fixed Apple utilities run here. On expiry, dropping output() kills
    // the child; no shell or arbitrary bundle executable runs during detection.
    tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

async fn checked_launch_path(id: &str) -> Result<PathBuf, String> {
    let label = desktop_label(id);
    let discovered = detect(id).await;
    let path = discovered
        .path
        .map(PathBuf::from)
        .filter(|_| discovered.installed)
        .ok_or_else(|| format!("未找到可安全启动的 {label}，请先安装或重新检查"))?;
    // Re-discover from native-owned candidates, reject stale/renderer paths and
    // revalidate bundle identity and executable immediately before opening.
    let allowed = candidates(id)
        .into_iter()
        .find(|candidate| candidate.canonicalize().ok().as_ref() == Some(&path));
    let checked = match allowed {
        Some(candidate) => inspect_bundle(&candidate, id, Duration::from_secs(1)).await,
        None => Err(().into()),
    };
    if !matches!(checked, Ok(Some(ref app)) if Path::new(&app.path) == path) {
        return Err(format!("{label} 安装状态已改变，请重新检查"));
    }
    Ok(path)
}

pub(super) async fn launch_desktop(
    app: &tauri::AppHandle,
    id: &str,
    restart: bool,
) -> Result<OsirToolLaunch, String> {
    let _guard = desktop_action_guard(id)?;
    let path = checked_launch_path(id).await?;
    launch_checked_desktop(app, id, &path, restart).await
}

/// The installer holds its transaction/filesystem leases and supplies its
/// native-owned, signature-verified bundle. No renderer path reaches here.
pub(super) async fn launch_managed_codex(
    app: &tauri::AppHandle,
    bundle: &Path,
) -> Result<OsirToolLaunch, String> {
    let _guard = codex_launch_guard()?;
    let inspected = inspect_bundle(bundle, "codex", Duration::from_secs(1))
        .await
        .map_err(|_| "Codex 安装状态已改变，请重新检查")?
        .ok_or("Codex 安装状态已改变，请重新检查")?;
    launch_checked_desktop(app, "codex", Path::new(&inspected.path), false).await
}

async fn launch_checked_desktop(
    app: &tauri::AppHandle,
    id: &str,
    path: &Path,
    restart: bool,
) -> Result<OsirToolLaunch, String> {
    let existing = current_desktop_instance(app, id).await?;
    if let Some(instance) = existing.as_ref() {
        if !restart {
            return activate_desktop_instance(app, id, instance).await;
        }
        validate_desktop_action_target(id, instance)?;
    }
    if id == "codex"
        && crate::distribution::is_isolated()
        && existing
            .as_ref()
            .is_none_or(|instance| instance.scope == DesktopScope::Profile)
    {
        let profile = CodexDevProfile::new(
            crate::config::get_home_dir(),
            crate::codex_config::get_codex_config_dir(),
            std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from),
        )?;
        let running = codex_profile_process(app, &profile).await?;
        let bundle = codex_profile_bundle_choice(
            path,
            running.as_ref().map(|running| running.bundle.as_path()),
        );
        let expected = running.as_ref().map(|running| (running.pid, running.birth));
        return launch_codex_dev(app, &bundle, restart, expected).await;
    }
    let path = existing.as_ref().map_or(path, DesktopInstance::bundle);
    if let Some(instance) = existing.as_ref() {
        force_terminate_instance(app, id, instance).await?;
    }
    // A reopen event also restores minimized/closed windows. No -n: reuse
    // the validated copy instead of creating another account/profile instance.
    let output = system_output("/usr/bin/open", &["-a"], path, Duration::from_secs(5))
        .await
        .map_err(|_| format!("{} 启动未完成，请重试", desktop_label(id)))?;
    if !output.status.success() {
        return Err(format!("无法启动 {}，请检查安装后重试", desktop_label(id)));
    }
    let mut result = confirm_desktop_launch(app, id, path, None, false, false).await?;
    result.restarted = restart && existing.is_some();
    Ok(result)
}

// WorkBuddy's supported config/userData overrides must travel with the process.
// Launch Services does not inherit our isolated HOME. Its single-instance lock
// also uses userData, so setting only models.json's parent would be insufficient.
#[derive(Debug)]
struct WorkBuddyProfile {
    home: PathBuf,
    config_dir: PathBuf,
    user_data_dir: PathBuf,
}

impl WorkBuddyProfile {
    fn new(home: PathBuf, isolated: bool, sandbox: Option<PathBuf>) -> Result<Self, String> {
        let error = "WorkBuddy 配置目录不可用，请重新启动 OSIRAPI 客户端";
        if !home.is_absolute()
            || home.to_string_lossy().chars().any(char::is_control)
            || !home.is_dir()
            || (isolated && sandbox.as_ref() != Some(&home))
        {
            return Err(error.into());
        }
        let profile = Self {
            config_dir: home.join(".workbuddy"),
            user_data_dir: home.join(".workbuddy/app"),
            home,
        };
        for directory in [&profile.config_dir, &profile.user_data_dir] {
            match std::fs::symlink_metadata(directory) {
                Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                    return Err(error.into());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(error.into()),
            }
        }
        Ok(profile)
    }

    fn command(&self, executable: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(executable);
        command
            .env_clear()
            .env(
                "PATH",
                "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            )
            .env("LC_ALL", "en_US.UTF-8")
            .env("HOME", &self.home)
            .env("WORKBUDDY_CONFIG_DIR", &self.config_dir)
            .env("CODEBUDDY_CONFIG_DIR", &self.config_dir)
            .env("WORKBUDDY_USER_DATA_DIR", &self.user_data_dir)
            .env("WORKBUDDY_FORCE_NO_INSTANCE_NUMBER", "1")
            .current_dir(&self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn running_pid(&self) -> Option<i32> {
        // Electron's macOS singleton link is hostname-pid. Probe that PID with
        // signal zero only; never terminate or inspect another app's arguments.
        let Ok(lock) = std::fs::read_link(self.user_data_dir.join("SingletonLock")) else {
            return None;
        };
        let Some(pid) = lock
            .to_str()
            .and_then(|value| value.rsplit_once('-'))
            .and_then(|(_, pid)| pid.parse::<i32>().ok())
            .filter(|pid| *pid > 1)
        else {
            return None;
        };
        unsafe {
            (libc::kill(pid, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
            .then_some(pid)
        }
    }
}

pub(super) async fn launch_workbuddy(
    app: &tauri::AppHandle,
    restart: bool,
) -> Result<WorkBuddyLaunch, String> {
    let _guard = desktop_action_guard("workbuddy")?;
    let existing = current_desktop_instance(app, "workbuddy").await?;
    if let Some(instance) = existing.as_ref() {
        if !restart {
            return activate_desktop_instance(app, "workbuddy", instance).await;
        }
        validate_desktop_action_target("workbuddy", instance)?;
        force_terminate_instance(app, "workbuddy", instance).await?;
    }
    let path = match existing.as_ref() {
        Some(instance) => instance.bundle().to_path_buf(),
        None => checked_launch_path("workbuddy").await?,
    };
    let isolated = crate::distribution::is_isolated();
    let profile = WorkBuddyProfile::new(
        crate::config::get_home_dir(),
        isolated,
        std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from),
    )?;
    // Read the executable basename from the same validated bundle, never a
    // renderer path, executable name, launch argument, or profile override.
    let output = system_output(
        "/usr/bin/plutil",
        &["-convert", "json", "-o", "-", "--"],
        &path.join("Contents/Info.plist"),
        Duration::from_secs(1),
    )
    .await
    .map_err(|_| "WorkBuddy 安装状态已改变，请重新检查")?;
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "WorkBuddy 安装状态已改变，请重新检查")?;
    if !matches!(
        installation_from_metadata(&path, "workbuddy", &metadata),
        Ok(Some(_))
    ) {
        return Err("WorkBuddy 安装状态已改变，请重新检查".into());
    }
    let executable = validate_executable(&path, &metadata)
        .map_err(|_| "WorkBuddy 安装状态已改变，请重新检查")?;
    std::fs::create_dir_all(&profile.user_data_dir)
        .map_err(|_| "无法创建 WorkBuddy 配置目录，请检查权限")?;
    let existing_pid = profile.running_pid();
    let restarted = restart && existing.is_some();
    if restart && existing_pid.is_some() {
        // The selected instance has already exited. Do not terminate another
        // process which acquired the profile while that instance was closing.
        return Err("应用运行状态已变化，请重新打开".into());
    }
    let already_running = if let Some(pid) = existing_pid {
        if !process_matches(pid, &executable) {
            return Err("WorkBuddy 配置被其他进程占用，请检查后重试".into());
        }
        true
    } else {
        false
    };
    // Electron's same-profile second-instance event restores its window. It
    // must use this exact profile, never a Launch Services formal-home reopen.
    let mut child = profile
        .command(&executable)
        .spawn()
        .map_err(|_| "无法启动 WorkBuddy，请检查安装后重试")?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    if child
        .try_wait()
        .map_err(|_| "WorkBuddy 启动未完成，请重试")?
        .is_some_and(|status| !status.success())
    {
        return Err("无法启动 WorkBuddy，请检查安装后重试".into());
    }
    // Reap a process launched here when it eventually closes, without owning
    // its lifetime or killing a user's running WorkBuddy instance.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Some(pid) = profile
            .running_pid()
            .filter(|pid| process_matches(*pid, &executable))
        {
            break pid;
        }
        if Instant::now() >= deadline {
            return Err("WorkBuddy 启动请求已发送，但尚未确认当前配置窗口，请稍后重试".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let mut result = confirm_desktop_launch(
        app,
        "workbuddy",
        &path,
        Some(pid),
        isolated,
        already_running,
    )
    .await?;
    result.restarted = restarted;
    Ok(result)
}

#[cfg(test)]
#[path = "osir_tools_macos_tests.rs"]
mod tests;
