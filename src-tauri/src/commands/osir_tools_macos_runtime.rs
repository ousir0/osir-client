// Process/window evidence only: no argv, environment, AX content or window titles.
use objc2_06::{
    msg_send,
    rc::autoreleasepool,
    runtime::{AnyClass, AnyObject},
    sel,
};
use objc2_foundation_03::NSString;
use std::collections::HashSet;
use std::ffi::{c_void, CStr};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(options: u32, relative_to: u32) -> *mut AnyObject;
}
const ACTIVATION_DENIED: &str = "系统未同意将应用切换到前台，请在 Dock 中打开后重试";
const ACTIVATE_ALL_WINDOWS: usize = 1;

fn request_application_activation(
    cooperative_supported: bool,
    yield_to_target: impl FnOnce(),
    activate_cooperative: impl FnOnce() -> bool,
    activate_legacy: impl FnOnce() -> bool,
) -> Result<(), String> {
    let accepted = if cooperative_supported {
        yield_to_target();
        activate_cooperative()
    } else {
        activate_legacy()
    };
    if accepted {
        Ok(())
    } else {
        Err(ACTIVATION_DENIED.into())
    }
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *const c_void);
}

#[derive(Debug, Clone)]
struct AppEvidence {
    pid: i32,
    window: bool,
    onscreen: bool,
    active: bool,
}
fn desktop_identity(id: &str) -> Option<&'static str> {
    match id {
        "codex" => Some(BUNDLE_ID),
        "claude-desktop" => Some(CLAUDE_BUNDLE_ID),
        "workbuddy" => Some(WORKBUDDY_BUNDLE_ID),
        _ => None,
    }
}
fn desktop_label(id: &str) -> &'static str {
    match id {
        "codex" => "Codex Desktop",
        "claude-desktop" => "Claude Desktop",
        _ => "WorkBuddy",
    }
}
fn run_queued_main_action<T>(
    tx: tokio::sync::oneshot::Sender<Result<T, String>>,
    action: impl FnOnce() -> Result<T, String>,
) {
    // A timed-out caller drops its receiver while this work may still be
    // queued behind the main thread. Never begin that cancelled side effect.
    if !tx.is_closed() {
        let _ = tx.send(action());
    }
}
async fn on_main<T: Send + 'static>(
    app: &tauri::AppHandle,
    action: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        run_queued_main_action(tx, action);
    })
    .map_err(|_| "无法读取应用窗口状态，请重试".to_string())?;
    tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .map_err(|_| "应用窗口状态检查超时，请重试".to_string())?
        .map_err(|_| "无法读取应用窗口状态，请重试".to_string())?
}
unsafe fn class(name: &'static [u8]) -> Result<&'static AnyClass, String> {
    AnyClass::get(CStr::from_bytes_with_nul(name).expect("fixed Objective-C class"))
        .ok_or_else(|| "当前系统暂不能确认应用窗口状态".into())
}
unsafe fn object_field(object: *mut AnyObject, name: &str) -> *mut AnyObject {
    let key = NSString::from_str(name);
    msg_send![object, objectForKey: &*key]
}
unsafe fn number(object: *mut AnyObject, name: &str) -> Option<i64> {
    let value = object_field(object, name);
    if value.is_null() {
        None
    } else {
        Some(msg_send![value, longLongValue])
    }
}
/// Geometry does not read a screenshot or sensitive window title.
unsafe fn window_owners() -> Result<(HashSet<i32>, HashSet<i32>), String> {
    let windows = CGWindowListCopyWindowInfo(16, 0);
    if windows.is_null() {
        return Err("当前系统暂不能确认应用窗口状态".into());
    }
    let result = (|| {
        let count: usize = msg_send![windows, count];
        if count > 16384 {
            return Err("应用窗口状态过多，暂无法确认".into());
        }
        let mut all = HashSet::new();
        let mut onscreen = HashSet::new();
        for index in 0..count {
            let info: *mut AnyObject = msg_send![windows, objectAtIndex: index];
            if number(info, "kCGWindowLayer") != Some(0) {
                continue;
            }
            let Some(pid) = number(info, "kCGWindowOwnerPID")
                .and_then(|p| i32::try_from(p).ok())
                .filter(|p| *p > 1)
            else {
                continue;
            };
            let bounds = object_field(info, "kCGWindowBounds");
            if bounds.is_null()
                || number(bounds, "Width").unwrap_or(0) < 80
                || number(bounds, "Height").unwrap_or(0) < 80
            {
                continue;
            }
            all.insert(pid);
            if number(info, "kCGWindowIsOnscreen") == Some(1) {
                onscreen.insert(pid);
            }
        }
        Ok((all, onscreen))
    })();
    CFRelease(windows.cast());
    result
}
fn process_path(pid: i32) -> Option<PathBuf> {
    if pid <= 1 {
        return None;
    }
    let mut bytes = [0u8; 4096];
    if unsafe { libc::proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) } <= 0 {
        return None;
    }
    let end = bytes.iter().position(|byte| *byte == 0)?;
    let path = PathBuf::from(std::str::from_utf8(&bytes[..end]).ok()?);
    path.is_absolute().then_some(path)
}
fn process_is_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}
fn process_birth(pid: i32) -> Option<(u64, u64)> {
    if pid <= 1 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    if unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    } != size
    {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some((info.pbi_start_tvsec, info.pbi_start_tvusec))
}
fn process_matches(pid: i32, executable: &Path) -> bool {
    match (
        process_path(pid).and_then(|path| path.canonicalize().ok()),
        executable.canonicalize().ok(),
    ) {
        (Some(actual), Some(expected)) if executable.is_absolute() => actual == expected,
        _ => false,
    }
}
unsafe fn snapshot_now(
    id: &str,
    path: &Path,
    profile_pid: Option<i32>,
) -> Result<Vec<AppEvidence>, String> {
    application_snapshot_now(id, path, profile_pid, true)
}

unsafe fn application_snapshot_now(
    id: &str,
    path: &Path,
    profile_pid: Option<i32>,
    include_windows: bool,
) -> Result<Vec<AppEvidence>, String> {
    let identity = desktop_identity(id).ok_or("不支持的桌面应用")?;
    // Runtime existence does not depend on a window. Keep the expensive
    // geometry query solely for real activation/foreground confirmation.
    let (windows, onscreen) = if include_windows {
        window_owners()?
    } else {
        (HashSet::new(), HashSet::new())
    };
    let applications = class(b"NSRunningApplication\0")?;
    let identifier = NSString::from_str(identity);
    let list: *mut AnyObject =
        msg_send![applications, runningApplicationsWithBundleIdentifier: &*identifier];
    if list.is_null() {
        return Err("无法读取应用运行状态".into());
    }
    let count: usize = msg_send![list, count];
    let mut found = Vec::new();
    for index in 0..count.min(128) {
        let candidate: *mut AnyObject = msg_send![list, objectAtIndex: index];
        let terminated: bool = msg_send![candidate, isTerminated];
        let pid: i32 = msg_send![candidate, processIdentifier];
        if terminated || profile_pid.is_some_and(|expected| expected != pid) {
            continue;
        }
        let Some(executable) = process_path(pid).and_then(|p| p.canonicalize().ok()) else {
            continue;
        };
        if !executable.starts_with(path.join("Contents/MacOS")) {
            continue;
        }
        found.push(AppEvidence {
            pid,
            window: windows.contains(&pid),
            onscreen: onscreen.contains(&pid),
            active: msg_send![candidate, isActive],
        });
    }
    Ok(found)
}
async fn desktop_snapshot(
    app: &tauri::AppHandle,
    id: &str,
    path: &Path,
    profile_pid: Option<i32>,
) -> Result<Vec<AppEvidence>, String> {
    let id = id.to_owned();
    let path = path.to_owned();
    on_main(app, move || {
        autoreleasepool(|_| unsafe { snapshot_now(&id, &path, profile_pid) })
    })
    .await
}

async fn desktop_process_snapshot(
    app: &tauri::AppHandle,
    id: &str,
    path: &Path,
    profile_pid: Option<i32>,
) -> Result<Vec<AppEvidence>, String> {
    let id = id.to_owned();
    let path = path.to_owned();
    on_main(app, move || {
        autoreleasepool(|_| unsafe { application_snapshot_now(&id, &path, profile_pid, false) })
    })
    .await
}
async fn activate_pid(
    app: &tauri::AppHandle,
    id: &str,
    path: &Path,
    pid: i32,
) -> Result<(), String> {
    let id = id.to_owned();
    let path = path.to_owned();
    let birth = process_birth(pid).ok_or("应用运行状态已变化，请重新打开")?;
    on_main(app, move || {
        autoreleasepool(|_| unsafe {
            if process_birth(pid) != Some(birth)
                || !snapshot_now(&id, &path, Some(pid))?
                .iter()
                .any(|p| p.pid == pid)
            {
                return Err("应用运行状态已变化，请重新打开".into());
            }
            let applications = class(b"NSRunningApplication\0")?;
            let application: *mut AnyObject =
                msg_send![applications, runningApplicationWithProcessIdentifier: pid];
            if application.is_null() {
                return Err("应用已退出，请重新打开".into());
            }
            let app_class = class(b"NSApplication\0")?;
            let source: *mut AnyObject = msg_send![app_class, sharedApplication];
            let source_running: *mut AnyObject = msg_send![applications, currentApplication];
            if source.is_null() || source_running.is_null() || process_birth(pid) != Some(birth) {
                return Err("应用运行状态已变化，请重新打开".into());
            }
            // macOS 14 introduced cooperative activation; IgnoringOtherApps
            // has no effect there. Probe both selectors before sending either
            // so earlier supported macOS versions keep the legacy request.
            let can_yield: bool = msg_send![source, respondsToSelector: sel!(yieldActivationToApplication:)];
            let can_cooperate: bool = msg_send![application, respondsToSelector: sel!(activateFromApplication:options:)];
            let _: bool = msg_send![application, unhide];
            request_application_activation(
                can_yield && can_cooperate,
                || { let _: () = msg_send![source, yieldActivationToApplication: application]; },
                || msg_send![application, activateFromApplication: source_running, options: ACTIVATE_ALL_WINDOWS],
                || msg_send![application, activateWithOptions: ACTIVATE_ALL_WINDOWS],
            )
        })
    })
    .await
}
async fn confirm_desktop_launch(
    app: &tauri::AppHandle,
    id: &str,
    path: &Path,
    profile_pid: Option<i32>,
    isolated: bool,
    already_running: bool,
) -> Result<OsirToolLaunch, String> {
    let deadline = Instant::now() + Duration::from_secs(7);
    let mut activated = HashSet::new();
    loop {
        let applications = desktop_snapshot(app, id, path, profile_pid).await?;
        for application in &applications {
            if application.window && application.onscreen && application.active {
                return Ok(OsirToolLaunch {
                    isolated,
                    already_running,
                    runtime_status: ToolRuntimeStatus::Running,
                    foreground: true,
                    exit_code: None,
                    restarted: false,
                });
            }
            if activated.insert(application.pid) {
                activate_pid(app, id, path, application.pid).await?;
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} 启动请求已发送，但尚未确认前台窗口；请检查应用后重试",
                desktop_label(id)
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn validate_restart_target(pid: i32) -> Result<(), String> {
    if pid <= 1 || pid == std::process::id() as i32 {
        Err("无法安全确认桌面应用的运行实例，请重新检测".into())
    } else {
        Ok(())
    }
}

async fn terminate_known(
    app: &tauri::AppHandle,
    id: &str,
    path: &Path,
    executable: &Path,
    targets: &[(i32, (u64, u64))],
    force: bool,
) -> Result<(), String> {
    for (pid, _) in targets {
        validate_restart_target(*pid)?;
    }
    for (pid, birth) in targets {
        let pid = *pid;
        let birth = *birth;
        let id = id.to_owned();
        let path = path.to_path_buf();
        let executable = executable.to_path_buf();
        on_main(app, move || {
            autoreleasepool(|_| unsafe {
                validate_desktop_instance_identity(
                    pid,
                    birth,
                    pid,
                    process_birth(pid),
                    process_matches(pid, &executable),
                )?;
                if !application_snapshot_now(&id, &path, Some(pid), false)?
                    .iter()
                    .any(|found| found.pid == pid)
                {
                    return Err("应用运行状态已变化，请重新打开".into());
                }
                let applications = class(b"NSRunningApplication\0")?;
                let application: *mut AnyObject =
                    msg_send![applications, runningApplicationWithProcessIdentifier: pid];
                if application.is_null() {
                    return Err("应用已退出，请重新打开".into());
                }
                validate_desktop_instance_identity(
                    pid,
                    birth,
                    pid,
                    process_birth(pid),
                    process_matches(pid, &executable),
                )?;
                let accepted: bool = if force {
                    msg_send![application, forceTerminate]
                } else {
                    msg_send![application, terminate]
                };
                if !accepted {
                    return Err(if force {
                        "系统未能强制退出应用，请重新检测"
                    } else {
                        "应用未同意退出，请保存工作后重试"
                    }
                    .into());
                }
                Ok(())
            })
        })
        .await?;
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        // NSRunningApplication may report termination before Electron has
        // released its singleton resources. Wait for the exact PIDs to exit;
        // a disappearing window alone must not trigger another open -n.
        if !targets
            .iter()
            .any(|(pid, birth)| process_birth(*pid) == Some(*birth) && process_is_alive(*pid))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(if force {
                "应用仍未退出，请重新检测"
            } else {
                "应用尚未退出，已取消重新打开；请保存工作后重试"
            }
            .into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn native_macho(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && matches!(
            magic,
            [0xcf, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xce]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
}
fn cli_process_status(executable: &Path) -> ToolRuntimeStatus {
    // Interpreter processes cannot be attributed safely without reading argv.
    let Ok(executable) = executable.canonicalize() else {
        return ToolRuntimeStatus::Unavailable;
    };
    if !native_macho(&executable) {
        return ToolRuntimeStatus::Unavailable;
    }
    let mut pids = vec![0i32; 16384];
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if count <= 0 || count as usize >= pids.len() {
        return ToolRuntimeStatus::Unavailable;
    }
    let mut unknown = false;
    for pid in pids.into_iter().take(count as usize).filter(|pid| *pid > 1) {
        if !process_matches(pid, &executable) {
            continue;
        }
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        if unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        } != size
        {
            unknown = true;
            continue;
        }
        let info = unsafe { info.assume_init() };
        // Foreground TTY session for the resolved executable. This deliberately
        // avoids reading argv or environment, so an app-server/version probe
        // using the same executable cannot be distinguished here.
        if foreground_cli_session(info.pbi_status, info.e_tdev, info.pbi_pgid, info.e_tpgid) {
            return ToolRuntimeStatus::Running;
        }
    }
    if unknown {
        ToolRuntimeStatus::Unavailable
    } else {
        ToolRuntimeStatus::Stopped
    }
}
fn foreground_cli_session(status: u32, device: u32, group: u32, foreground_group: u32) -> bool {
    status != 5 && device != u32::MAX && foreground_group != 0 && group == foreground_group
}

fn launched_cli_child_running(launcher_pid: i32) -> bool {
    if launcher_pid <= 1 {
        return false;
    }
    let mut pids = vec![0i32; 16384];
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if count <= 0 || count as usize >= pids.len() {
        return false;
    }
    pids.into_iter()
        .take(count as usize)
        .filter(|pid| *pid > 1)
        .any(|pid| {
            let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
            if unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    size,
                )
            } != size
            {
                return false;
            }
            let info = unsafe { info.assume_init() };
            // Only a foreground child of our private per-request launcher counts.
            // This attributes script/Node tools without inspecting any argv, env,
            // Terminal contents or unrelated sessions running the same executable.
            info.pbi_ppid == launcher_pid as u32
                && foreground_cli_session(info.pbi_status, info.e_tdev, info.pbi_pgid, info.e_tpgid)
        })
}
async fn terminal_foreground(app: &tauri::AppHandle) -> bool {
    on_main(app, || {
        autoreleasepool(|_| unsafe {
            let workspace = class(b"NSWorkspace\0")?;
            let shared: *mut AnyObject = msg_send![workspace, sharedWorkspace];
            let front: *mut AnyObject = msg_send![shared, frontmostApplication];
            if front.is_null() {
                return Ok(false);
            }
            let bundle: *mut AnyObject = msg_send![front, bundleIdentifier];
            if bundle.is_null() {
                return Ok(false);
            }
            let utf8: *const std::ffi::c_char = msg_send![bundle, UTF8String];
            if utf8.is_null() {
                return Ok(false);
            }
            let identity = CStr::from_ptr(utf8).to_string_lossy();
            if !matches!(
                identity.as_ref(),
                "com.apple.Terminal"
                    | "com.googlecode.iterm2"
                    | "dev.warp.Warp-Stable"
                    | "org.alacritty"
                    | "net.kovidgoyal.kitty"
                    | "com.mitchellh.ghostty"
                    | "com.github.wez.wezterm"
            ) {
                return Ok(false);
            }
            let pid: i32 = msg_send![front, processIdentifier];
            Ok(window_owners()?.1.contains(&pid))
        })
    })
    .await
    .unwrap_or(false)
}
pub(super) async fn launch_cli(
    app: &tauri::AppHandle,
    state: &crate::store::AppState,
    id: &str,
    directory: &Path,
) -> Result<OsirToolLaunch, String> {
    let receipt = crate::commands::misc::launch_current_osir_cli(state, id, directory).await?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut status = ToolRuntimeStatus::Unavailable;
    let mut exit_code = None;
    let mut foreground;
    loop {
        foreground = terminal_foreground(app).await;
        match receipt.state() {
            crate::commands::misc::CliLaunchState::Exited { code, .. } => {
                exit_code = Some(code);
                status = ToolRuntimeStatus::Stopped;
                break;
            }
            crate::commands::misc::CliLaunchState::Started { pid } => {
                status = if launched_cli_child_running(pid) {
                    ToolRuntimeStatus::Running
                } else {
                    ToolRuntimeStatus::Unavailable
                };
            }
            crate::commands::misc::CliLaunchState::Pending => {}
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(OsirToolLaunch {
        isolated: crate::distribution::is_isolated(),
        // We cannot safely associate an arbitrary existing TTY with a project.
        // This action opens a new terminal; never claim it resumed that session.
        already_running: false,
        runtime_status: status,
        foreground,
        exit_code,
        restarted: false,
    })
}
pub(super) async fn refresh_runtime(app: &tauri::AppHandle, records: &mut [OsirToolDetection]) {
    for record in records {
        record.runtime_status = ToolRuntimeStatus::Unavailable;
        record.runtime_error = None;
        record.close_supported = false;
        record.restart_supported = false;
        record.launch_supported = record.installed
            && super::valid_launch_target(
                &record.id,
                if record.kind == ToolKind::Desktop {
                    "desktop"
                } else {
                    "cli"
                },
            );
        if record.kind != ToolKind::Desktop {
            continue;
        }
        match current_desktop_instance(app, &record.id).await {
            Ok(Some(instance)) => {
                record.installed = true;
                record.status = ToolStatus::Installed;
                record.error = None;
                record.version = instance.installation.version;
                record.path = Some(instance.installation.path);
                record.runtime_status = ToolRuntimeStatus::Running;
                record.launch_supported = true;
                record.close_supported = true;
                record.restart_supported = true;
            }
            Ok(None) => {
                record.runtime_status = ToolRuntimeStatus::Stopped;
                // Restarting a stopped app is the same as opening it once.
                record.restart_supported = record.launch_supported;
            }
            Err(error) => record.runtime_error = Some(error),
        }
    }
}
