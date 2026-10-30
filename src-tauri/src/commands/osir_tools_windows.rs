//! Windows desktop actions. Native process handles pin image and creation time;
//! no process arguments, credentials, session bodies or global name kills.
use super::{
    windows_support as support, OsirToolClose, OsirToolDetection, OsirToolLaunch, ToolKind,
    ToolRuntimeStatus, ToolStatus,
};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::time::{Duration, Instant};
use support::{discover, powershell, ps_literal, Installation};
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, WAIT_OBJECT_0},
    System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
        SetForegroundWindow, ShowWindow, SW_RESTORE, WM_CLOSE,
    },
};

static ACTION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
pub(crate) fn guard() -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    ACTION
        .try_lock()
        .map_err(|_| "桌面应用操作正在进行，请稍后重试".into())
}
fn production_only() -> Result<(), String> {
    if crate::distribution::is_isolated() {
        Err("当前隔离环境不能控制本机桌面应用，请在正式版中操作".into())
    } else {
        Ok(())
    }
}

struct Process {
    handle: usize,
    pid: u32,
    image: String,
    birth: u64,
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle as HANDLE);
        }
    }
}
impl Process {
    fn open(pid: u32, terminate: bool) -> Option<Self> {
        if pid <= 1 || pid == std::process::id() {
            return None;
        }
        let access = PROCESS_QUERY_LIMITED_INFORMATION
            | PROCESS_SYNCHRONIZE
            | if terminate { PROCESS_TERMINATE } else { 0 };
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let mut process = Self {
            handle: handle as usize,
            pid,
            image: String::new(),
            birth: 0,
        };
        let mut buffer = vec![0u16; 32768];
        let mut size = buffer.len() as u32;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) } == 0
            || unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
                == 0
        {
            return None;
        }
        process.image = OsString::from_wide(&buffer[..size as usize])
            .to_string_lossy()
            .into_owned();
        process.birth = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        Some(process)
    }
    fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle as HANDLE, 0) != WAIT_OBJECT_0 }
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn K32EnumProcesses(ids: *mut u32, size: u32, needed: *mut u32) -> i32;
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
}
fn processes(entries: &[Installation]) -> Result<Vec<Process>, String> {
    let mut current_session = 0;
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut current_session) } == 0 {
        return Err("无法确认当前 Windows 用户会话".into());
    }
    let mut ids = vec![0u32; 65536];
    let mut needed = 0;
    if unsafe { K32EnumProcesses(ids.as_mut_ptr(), (ids.len() * 4) as u32, &mut needed) } == 0
        || needed as usize >= ids.len() * 4
    {
        return Err("无法确认桌面应用运行状态，请重试".into());
    }
    Ok(ids
        .into_iter()
        .take(needed as usize / 4)
        .filter(|pid| {
            let mut session = 0;
            unsafe { ProcessIdToSessionId(*pid, &mut session) != 0 && session == current_session }
        })
        .filter_map(|pid| Process::open(pid, false))
        .filter(|p| {
            p.alive()
                && entries
                    .iter()
                    .any(|e| support::same_path(&p.image, &e.executable))
        })
        .collect())
}
struct Windows {
    pids: Vec<u32>,
    windows: Vec<(HWND, u32)>,
}
unsafe extern "system" fn collect_window(hwnd: HWND, value: LPARAM) -> i32 {
    let state = &mut *(value as *mut Windows);
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if state.pids.contains(&pid) && IsWindowVisible(hwnd) != 0 {
        state.windows.push((hwnd, pid));
    }
    1
}
fn windows(processes: &[Process]) -> Result<Vec<(HWND, u32)>, String> {
    let mut state = Windows {
        pids: processes.iter().map(|p| p.pid).collect(),
        windows: Vec::new(),
    };
    if unsafe { EnumWindows(Some(collect_window), &mut state as *mut _ as LPARAM) } == 0 {
        return Err("无法读取桌面应用窗口状态".into());
    }
    Ok(state.windows)
}
fn activate(processes: &[Process]) -> Result<(bool, bool), String> {
    let found = windows(processes)?;
    for (hwnd, _) in &found {
        unsafe {
            ShowWindow(*hwnd, SW_RESTORE);
            SetForegroundWindow(*hwnd);
        }
    }
    let front = unsafe { GetForegroundWindow() };
    Ok((!found.is_empty(), found.iter().any(|(h, _)| *h == front)))
}

pub(super) async fn detect(id: &str) -> OsirToolDetection {
    let mut result = OsirToolDetection::new(id, ToolKind::Desktop);
    match discover(id).await {
        Ok(entries) => {
            if let Some(entry) = entries.first() {
                result.installed = true;
                result.status = ToolStatus::Installed;
                result.version = Some(entry.version.clone());
                result.path = Some(entry.root.clone());
                result.launch_supported = !crate::distribution::is_isolated();
            }
        }
        Err(error) => result = result.failed(false, &error),
    }
    result
}
pub(super) async fn refresh_runtime(_: &tauri::AppHandle, records: &mut [OsirToolDetection]) {
    for record in records.iter_mut().filter(|r| r.kind == ToolKind::Desktop) {
        let result = async {
            let entries = discover(&record.id).await?;
            processes(&entries).map(|p| !p.is_empty())
        }
        .await;
        match result {
            Ok(running) => {
                record.runtime_status = if running {
                    ToolRuntimeStatus::Running
                } else {
                    ToolRuntimeStatus::Stopped
                };
                record.close_supported = record.installed && !crate::distribution::is_isolated();
                record.restart_supported = record.close_supported;
            }
            Err(error) => {
                record.runtime_status = ToolRuntimeStatus::Unavailable;
                record.runtime_error = Some(error);
                record.close_supported = false;
                record.restart_supported = false;
            }
        }
    }
}

async fn close_checked(entries: &[Installation], force: bool) -> Result<bool, String> {
    let targets = processes(entries)?;
    unique_instance(&targets)?;
    if targets.is_empty() {
        return Ok(false);
    }
    // Keep original query handles open while reopening with terminate rights;
    // compare creation time and executable again to reject PID reuse.
    for target in &targets {
        if !target.alive() {
            continue;
        }
        if force {
            let Some(pinned) = Process::open(target.pid, true) else {
                if !target.alive() {
                    continue;
                }
                return Err("Windows 不允许关闭此应用，请在应用中退出".into());
            };
            if pinned.birth != target.birth || !support::same_path(&pinned.image, &target.image) {
                return Err("应用运行状态已改变，请重新检测".into());
            }
            if pinned.alive()
                && unsafe { TerminateProcess(pinned.handle as HANDLE, 1) } == 0
                && pinned.alive()
            {
                return Err("Windows 不允许关闭此应用，请在应用中退出".into());
            }
        }
    }
    if !force {
        for (hwnd, pid) in windows(&targets)? {
            if let Some(target) = targets.iter().find(|p| p.pid == pid) {
                let mut actual_pid = 0;
                unsafe {
                    GetWindowThreadProcessId(hwnd, &mut actual_pid);
                }
                if actual_pid == target.pid
                    && target.alive()
                    && unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } == 0
                {
                    return Err("应用未接受关闭请求，请在应用中退出".into());
                }
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if targets.iter().all(|p| !p.alive()) && processes(entries)?.is_empty() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Err("应用尚未退出，请保存工作后重试".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
pub(super) async fn close_desktop(
    _: &tauri::AppHandle,
    id: &str,
    force: bool,
) -> Result<OsirToolClose, String> {
    production_only()?;
    let _guard = guard()?;
    let entries = discover(id).await?;
    let closed = close_checked(&entries, force).await?;
    Ok(OsirToolClose {
        runtime_status: ToolRuntimeStatus::Stopped,
        closed,
        already_stopped: !closed,
    })
}
async fn start(entry: &Installation) -> Result<(), String> {
    if let (Some(family), Some(app)) = (&entry.family, &entry.app_id) {
        powershell(
            &format!(
                "Start-Process -FilePath {} -ErrorAction Stop | Out-Null; 'true'",
                ps_literal(&format!("shell:AppsFolder\\{family}!{app}"))
            ),
            Duration::from_secs(5),
        )
        .await?;
    } else {
        tokio::process::Command::new(&entry.executable)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| "无法打开桌面应用，请修复安装后重试")?;
    }
    Ok(())
}
pub(crate) async fn launch_desktop(
    _: &tauri::AppHandle,
    id: &str,
    restart: bool,
) -> Result<OsirToolLaunch, String> {
    production_only()?;
    let _guard = guard()?;
    let entries = discover(id).await?;
    let running = processes(&entries)?;
    unique_instance(&running)?;
    let already_running = !running.is_empty();
    let chosen = if already_running {
        entries.iter().find(|e| {
            running
                .iter()
                .any(|p| support::same_path(&p.image, &e.executable))
        })
    } else {
        entries.first()
    }
    .ok_or("尚未找到桌面应用，请先安装")?
    .clone();
    if restart {
        close_checked(&entries, true).await?;
    }
    if !already_running || restart {
        start(&chosen).await?;
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let live = processes(std::slice::from_ref(&chosen))?;
        let (visible, foreground) = activate(&live)?;
        if visible {
            return Ok(OsirToolLaunch {
                isolated: false,
                already_running,
                runtime_status: ToolRuntimeStatus::Running,
                foreground,
                exit_code: None,
                restarted: restart && already_running,
            });
        }
        if Instant::now() >= deadline {
            return Err("未能确认桌面应用窗口已打开，请检查应用提示后重试".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
pub(super) async fn request_desktop_session(_: &tauri::AppHandle, id: &str) -> Result<(), String> {
    production_only()?;
    let default_home = dirs::home_dir()
        .ok_or("无法确认 Codex Desktop 的本地历史目录")?
        .join(".codex");
    if !support::same_path(
        &crate::codex_config::get_codex_config_dir().to_string_lossy(),
        &default_home.to_string_lossy(),
    ) {
        return Err("Codex 使用自定义历史目录，请在原 Desktop 中打开该会话".into());
    }
    let url = support::session_url(id)?;
    let _guard = guard()?;
    let entries = discover("codex").await?;
    if entries.len() != 1 || !entries[0].protocol {
        return Err("无法确认唯一 Codex Desktop 会话入口，请在原客户端打开该会话".into());
    }
    // A targeted package-family launch avoids a hijacked/default codex handler.
    let script = format!(
        r#"
[Windows.System.Launcher,Windows.System,ContentType=WindowsRuntime] | Out-Null
[Windows.System.LauncherOptions,Windows.System,ContentType=WindowsRuntime] | Out-Null
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$options = [Windows.System.LauncherOptions]::new()
$options.TargetApplicationPackageFamilyName = {}
$op = [Windows.System.Launcher]::LaunchUriAsync([Uri]{}, $options)
$asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {{ $_.Name -eq 'AsTask' -and $_.IsGenericMethodDefinition -and $_.GetGenericArguments().Count -eq 1 -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' }} | Select-Object -First 1
$task = $asTask.MakeGenericMethod([bool]).Invoke($null, @($op))
if (-not $task.Wait(5000) -or -not $task.Result) {{ throw 'launch rejected' }}
'true'
"#,
        ps_literal(
            entries[0]
                .family
                .as_deref()
                .ok_or("无法确认 Codex Desktop 会话入口")?
        ),
        ps_literal(&url)
    );
    powershell(&script, Duration::from_secs(7)).await?;
    Ok(())
}

pub(crate) async fn desktop_detection(
) -> Result<crate::services::osir::codex_desktop::DesktopDetection, String> {
    use crate::services::osir::codex_desktop::{
        DesktopDetection, DesktopInstallation, DetectionStatus,
    };
    let entries = discover("codex").await?;
    Ok(DesktopDetection {
        status: if entries.is_empty() {
            DetectionStatus::NotFound
        } else {
            DetectionStatus::Detected
        },
        comparison_complete: true,
        installations: entries
            .into_iter()
            .map(|e| DesktopInstallation {
                path: e.root,
                build: Some(e.version.clone()),
                version: Some(e.version),
            })
            .collect(),
        message: "已检查当前 Windows 用户的 Codex Desktop 注册安装".into(),
        recovery_targets: Vec::new(),
    })
}

pub(crate) async fn codex_running() -> Result<bool, String> {
    let entries = discover("codex").await?;
    Ok(!processes(&entries)?.is_empty())
}

fn unique_instance(processes: &[Process]) -> Result<(), String> {
    let images: std::collections::HashSet<_> = processes
        .iter()
        .map(|p| support::path_key(&p.image))
        .collect();
    if images.len() > 1 {
        Err("检测到多个桌面安装同时运行，请在目标应用中操作后重新检测".into())
    } else {
        Ok(())
    }
}
