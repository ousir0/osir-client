//! Local tool inventory only. Registration belongs to the parent integration.
//! `codex` means Desktop, never the CLI. WorkBuddy model writes are handled
//! separately by the explicit native WorkBuddy configuration controller.
//! Inventory never launches tools or reads credentials. Runtime checks use only
//! native application identity, executable metadata and visible window geometry.

use futures::{
    future::{BoxFuture, Shared},
    stream, FutureExt, StreamExt,
};
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

const CLI_TIMEOUT: Duration = Duration::from_secs(4);
// Keep the catalog aligned with CC Switch's existing CLI lifecycle handlers.
// Codex is inventoried as Desktop here; its CLI remains in Codex's install tab.
const CLI_TOOLS: [&str; 11] = [
    "claude", "opencode", "gemini", "grok", "openclaw", "hermes", "pi", "mcode", "kimi", "agy",
    "omp",
];
const INVENTORY_CONCURRENCY: usize = 2;
const WORKBUDDY_DOWNLOAD_URL: &str = "https://www.workbuddy.ai/";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Installed,
    NotInstalled,
    Error,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Desktop,
    Cli,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolRuntimeStatus {
    Running,
    Stopped,
    Unavailable,
}

type InventoryProbe = Shared<BoxFuture<'static, Vec<OsirToolDetection>>>;

struct InventoryFlight {
    ticket: u64,
    generation: u64,
    probe: InventoryProbe,
}
struct InventoryState {
    cached: Option<(std::time::Instant, Vec<OsirToolDetection>)>,
    generation: u64,
    next_ticket: u64,
    flight: Option<InventoryFlight>,
}
struct InventoryCache(tokio::sync::Mutex<InventoryState>);
impl InventoryCache {
    const fn new() -> Self {
        Self(tokio::sync::Mutex::const_new(InventoryState {
            cached: None,
            generation: 0,
            next_ticket: 0,
            flight: None,
        }))
    }

    async fn invalidate(&self) {
        let mut state = self.0.lock().await;
        state.generation = state.generation.wrapping_add(1);
        state.cached = None;
        // Keep the in-flight probe shared until completion. Its old generation
        // cannot repopulate the cache or overlap a replacement batch.
    }

    async fn records(
        &self,
        probe: impl Fn() -> BoxFuture<'static, Vec<OsirToolDetection>>,
    ) -> Vec<OsirToolDetection> {
        loop {
            let (ticket, generation, pending) = {
                let mut state = self.0.lock().await;
                if let Some((at, records)) = &state.cached {
                    if at.elapsed() < Duration::from_secs(5) {
                        return records.clone();
                    }
                }
                if state.flight.is_none() {
                    state.next_ticket = state.next_ticket.wrapping_add(1);
                    state.flight = Some(InventoryFlight {
                        ticket: state.next_ticket,
                        generation: state.generation,
                        probe: probe().shared(),
                    });
                }
                let flight = state.flight.as_ref().expect("inventory flight created");
                (flight.ticket, flight.generation, flight.probe.clone())
            };
            // No cache lock is held while shells, version probes or metadata
            // subprocesses run. Other readers await the same shared future.
            let records = pending.await;
            let mut state = self.0.lock().await;
            if state.flight.as_ref().is_some_and(|f| f.ticket == ticket) {
                state.flight = None;
                if state.generation == generation {
                    state.cached = Some((std::time::Instant::now(), records.clone()));
                }
            }
            if state.generation == generation {
                return records;
            }
            // A launch invalidated this old result. Resolve one fresh batch,
            // coalesced with every other waiter, rather than caching stale data.
        }
    }
}

static INVENTORY_CACHE: InventoryCache = InventoryCache::new();

/// Stable renderer contract; optional values serialize as null, not omissions.
/// `installed` is installation evidence, independent of version probe success.
/// CLI paths stay null: the existing executor owns shell/default resolution and
/// does not return the resolved path. Never attribute its version to another copy.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OsirToolDetection {
    pub id: String,
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub status: ToolStatus,
    pub error: Option<String>,
    pub kind: ToolKind,
    #[serde(rename = "runtimeStatus")]
    pub runtime_status: ToolRuntimeStatus,
    #[serde(rename = "runtimeError")]
    pub runtime_error: Option<String>,
    #[serde(rename = "launchSupported")]
    pub launch_supported: bool,
    #[serde(rename = "closeSupported")]
    pub close_supported: bool,
    #[serde(rename = "restartSupported")]
    pub restart_supported: bool,
}

impl OsirToolDetection {
    fn new(id: &str, kind: ToolKind) -> Self {
        Self {
            id: id.into(),
            installed: false,
            version: None,
            path: None,
            status: ToolStatus::NotInstalled,
            error: None,
            kind,
            runtime_status: ToolRuntimeStatus::Unavailable,
            runtime_error: None,
            launch_supported: false,
            close_supported: false,
            restart_supported: false,
        }
    }

    fn failed(mut self, installed: bool, error: &str) -> Self {
        self.installed = installed;
        self.status = if installed {
            ToolStatus::Error
        } else {
            ToolStatus::Unavailable
        };
        self.error = Some(error.into());
        self
    }
}

/// Local inventory only; update checks and installers remain explicit actions.
#[tauri::command]
pub async fn osir_detect_tools(app: tauri::AppHandle) -> Result<Vec<OsirToolDetection>, String> {
    let mut records = INVENTORY_CACHE.records(|| detect_inventory().boxed()).await;
    refresh_runtime(&app, &mut records).await;
    Ok(records)
}

/// Frequent desktop-only status refresh: no CLI, account, update or credential
/// work. Paths and identities are discovered natively, never IPC inputs.
#[tauri::command]
pub async fn osir_desktop_runtime(app: tauri::AppHandle) -> Result<Vec<OsirToolDetection>, String> {
    let (codex, claude, workbuddy) = tokio::join!(
        detect_desktop("codex"),
        detect_desktop("claude-desktop"),
        detect_desktop("workbuddy"),
    );
    let mut records = vec![codex, claude, workbuddy];
    refresh_runtime(&app, &mut records).await;
    Ok(records)
}

async fn detect_inventory() -> Vec<OsirToolDetection> {
    let probes: Vec<_> = CLI_TOOLS.into_iter().map(detect_cli).collect();
    let cli = stream::iter(probes)
        .buffered(INVENTORY_CONCURRENCY)
        .collect::<Vec<_>>();
    let (codex, claude_desktop, cli, workbuddy) = tokio::join!(
        detect_desktop("codex"),
        detect_desktop("claude-desktop"),
        cli,
        detect_desktop("workbuddy"),
    );
    let mut records = vec![codex, claude_desktop];
    records.extend(cli);
    records.push(workbuddy);
    records
}

async fn refresh_runtime(app: &tauri::AppHandle, records: &mut [OsirToolDetection]) {
    #[cfg(target_os = "macos")]
    macos::refresh_runtime(app, records).await;
    #[cfg(target_os = "windows")]
    windows::refresh_runtime(app, records).await;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        for record in records {
            record.runtime_error = Some("当前平台尚未接入运行状态检测".into());
        }
    }
}

async fn detect_cli(id: &'static str) -> OsirToolDetection {
    let result = tokio::task::spawn_blocking(move || {
        // Reuse the CC Switch runtime resolver and its shared deadline, process
        // group termination, Windows shim/WSL handling and Node PATH support.
        // get_tool_versions(check_updates=false) still has an unbounded fallback
        // in scan_cli_version, so it is deliberately not used here.
        let output = crate::commands::misc::run_detected_tool_command_with_timeout(
            id,
            &["--version"],
            Some(CLI_TIMEOUT),
            &[],
            &std::env::temp_dir(),
        );
        // Only needed if resolution/launch timed out or failed. This is a fixed
        // set of bin locations, never a recursive home/config/history scan.
        let present = output.is_err() && cli_file_present(id);
        cli_result(id, present, output)
    })
    .await;
    result.unwrap_or_else(|_| {
        OsirToolDetection::new(id, ToolKind::Cli).failed(false, "本机工具检测未完成，请重试")
    })
}

fn cli_result(
    id: &str,
    present: bool,
    result: Result<std::process::Output, String>,
) -> OsirToolDetection {
    let mut record = OsirToolDetection::new(id, ToolKind::Cli);
    match result {
        Ok(output) => {
            record.installed = true;
            if !output.status.success() {
                return record.failed(true, "已安装，但版本查询失败，请检查工具运行环境");
            }
            // Only return a strict version token, never arbitrary CLI stdout or
            // stderr (which may include credentials, environment or file data).
            record.version =
                parsed_version(&output.stdout).or_else(|| parsed_version(&output.stderr));
            if record.version.is_none() {
                return record.failed(true, "已安装，但未能读取版本号");
            }
            record.status = ToolStatus::Installed;
            record
        }
        Err(error) => {
            // Stable native executor diagnostics, not subprocess error text.
            let installed = present
                || error.starts_with(&format!("Failed to run {id}:"))
                || error == format!("{id} is installed but its default installation is ambiguous");
            if !installed && error == format!("{id} is not installed") {
                record
            } else {
                record.failed(
                    installed,
                    if installed {
                        "已找到工具，但本次版本查询未完成，请检查运行环境后重试"
                    } else {
                        "本机命令环境暂不可用，无法确认安装状态，请重试"
                    },
                )
            }
        }
    }
}

fn parsed_version(bytes: &[u8]) -> Option<String> {
    static VERSION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let text = std::str::from_utf8(bytes.get(..bytes.len().min(4096))?).ok()?;
    let re = VERSION.get_or_init(|| {
        regex::Regex::new(r"(?:^|[^0-9A-Za-z])v?([0-9]{1,12}\.[0-9]{1,12}\.[0-9]{1,12}(?:-[0-9A-Za-z.-]{1,48})?(?:\+[0-9A-Za-z.-]{1,48})?)(?:$|[^0-9A-Za-z.+-])")
            .expect("fixed version expression")
    });
    Some(re.captures(text)?.get(1)?.as_str().into())
}

fn cli_file_present(id: &str) -> bool {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|p| p.is_absolute())
                .take(128)
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        for bin in [
            ".local/bin",
            ".npm-global/bin",
            ".volta/bin",
            ".bun/bin",
            "n/bin",
        ] {
            dirs.push(home.join(bin));
        }
        if id == "opencode" {
            dirs.push(home.join(".opencode/bin"));
        }
        // A single bounded level of known Node manager install directories.
        // This preserves installation evidence if a located nvm/mise CLI hangs.
        for manager in [
            ".nvm/versions/node",
            ".local/share/mise/installs/node",
            ".local/state/fnm_multishells",
        ] {
            if let Ok(entries) = std::fs::read_dir(home.join(manager)) {
                dirs.extend(
                    entries
                        .take(64)
                        .filter_map(Result::ok)
                        .map(|entry| entry.path().join("bin")),
                );
            }
        }
    }
    #[cfg(unix)]
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    #[cfg(windows)]
    if let Some(appdata) = dirs::data_dir() {
        dirs.push(appdata.join("npm"));
    }
    cli_file_in_dirs(id, &dirs)
}

fn cli_file_in_dirs(id: &str, dirs: &[PathBuf]) -> bool {
    dirs.iter().filter(|dir| dir.is_absolute()).any(|dir| {
        #[cfg(windows)]
        {
            // Never count Windows Store protocol/App Execution Alias stubs.
            if dir
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
                .contains("microsoft/windowsapps")
            {
                return false;
            }
            ["exe", "cmd", "bat"]
                .iter()
                .any(|ext| dir.join(format!("{id}.{ext}")).is_file())
        }
        #[cfg(not(windows))]
        {
            dir.join(id).is_file()
        }
    })
}

#[cfg(target_os = "macos")]
#[path = "osir_tools_macos.rs"]
mod macos;

#[cfg(target_os = "windows")]
#[path = "osir_tools_windows.rs"]
pub(crate) mod windows;
#[cfg(any(target_os = "windows", test))]
#[path = "osir_tools_windows_support.rs"]
pub(crate) mod windows_support;

async fn detect_desktop(id: &str) -> OsirToolDetection {
    #[cfg(target_os = "macos")]
    {
        macos::detect(id).await
    }
    #[cfg(target_os = "windows")]
    {
        windows::detect(id).await
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        OsirToolDetection::new(id, ToolKind::Desktop)
            .failed(false, "当前平台尚未接入此桌面应用的本地检测")
    }
}

/// Explicit user action only. Native discovery supplies the path; no URL, path
/// or arguments are accepted from the renderer. No app is launched by detection.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkBuddyLaunch {
    pub isolated: bool,
    pub already_running: bool,
    pub runtime_status: ToolRuntimeStatus,
    pub foreground: bool,
    pub exit_code: Option<i32>,
    pub restarted: bool,
}

pub type OsirToolLaunch = WorkBuddyLaunch;

pub(crate) async fn request_desktop_session(
    app: &tauri::AppHandle,
    session_id: &str,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::request_desktop_session(app, session_id).await
    }
    #[cfg(target_os = "windows")]
    {
        windows::request_desktop_session(app, session_id).await
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app, session_id);
        Err("当前平台尚未支持准确打开桌面会话，请在原工具中继续".into())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsirToolClose {
    pub runtime_status: ToolRuntimeStatus,
    pub closed: bool,
    pub already_stopped: bool,
}

#[tauri::command]
pub async fn osir_close_tool(
    app: tauri::AppHandle,
    tool_id: String,
    force: Option<bool>,
) -> Result<OsirToolClose, String> {
    if !valid_launch_target(&tool_id, "desktop") {
        return Err("此工具不支持从客户端关闭桌面应用".into());
    }
    #[cfg(target_os = "macos")]
    let result = macos::close_desktop(&app, &tool_id, force.unwrap_or(false)).await;
    #[cfg(target_os = "windows")]
    let result = windows::close_desktop(&app, &tool_id, force.unwrap_or(false)).await;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = {
        let _ = (app, force);
        Err("当前平台尚未接入桌面应用关闭，请从系统应用中退出".into())
    };
    INVENTORY_CACHE.invalidate().await;
    result
}

#[cfg(target_os = "macos")]
pub(super) async fn launch_installed_codex(
    app: &tauri::AppHandle,
    bundle: &std::path::Path,
) -> Result<OsirToolLaunch, String> {
    let result = macos::launch_managed_codex(app, bundle).await;
    INVENTORY_CACHE.invalidate().await;
    result
}

fn valid_launch_target(id: &str, target: &str) -> bool {
    (target == "cli" && (id == "codex" || CLI_TOOLS.contains(&id)))
        || (target == "desktop" && matches!(id, "codex" | "claude-desktop" | "workbuddy"))
}

fn launch_directory(target: &str, directory: Option<String>) -> Result<Option<PathBuf>, String> {
    if target == "desktop" {
        return if directory.is_none() {
            Ok(None)
        } else {
            Err("桌面应用启动不接受项目目录".into())
        };
    }
    let raw = directory.ok_or("请先选择一个项目文件夹")?;
    let path = std::path::Path::new(&raw);
    if !path.is_absolute() || raw.chars().any(char::is_control) {
        return Err("请选择有效的项目文件夹".into());
    }
    let path = path
        .canonicalize()
        .map_err(|_| "项目文件夹已不可用，请重新选择")?;
    if !path.is_dir() {
        return Err("请选择项目文件夹，而不是文件".into());
    }
    Ok(Some(path))
}

/// Fixed tool identities only. Installation, configuration and provider writes
/// remain separate operations; opening a terminal is not proof a CLI is running.
#[tauri::command]
pub async fn osir_launch_tool(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::store::AppState>,
    tool_id: String,
    target: String,
    directory: Option<String>,
    restart: Option<bool>,
) -> Result<OsirToolLaunch, String> {
    if !valid_launch_target(&tool_id, &target) {
        return Err("此工具暂不支持所选启动方式".into());
    }
    let directory = launch_directory(&target, directory)?;
    let restart = restart.unwrap_or(false);
    if restart && target != "desktop" {
        return Err("命令行工具请在所选项目中新开终端".into());
    }
    #[cfg(target_os = "macos")]
    let result = if target == "desktop" {
        if tool_id == "workbuddy" {
            macos::launch_workbuddy(&app, restart).await
        } else {
            macos::launch_desktop(&app, &tool_id, restart).await
        }
    } else {
        macos::launch_cli(
            &app,
            state.inner(),
            &tool_id,
            directory.as_deref().expect("CLI directory validated"),
        )
        .await
    };
    #[cfg(target_os = "windows")]
    let result = {
        let _ = (state, directory);
        if target == "desktop" {
            windows::launch_desktop(&app, &tool_id, restart).await
        } else {
            Err("Windows 命令行启动请使用终端入口".into())
        }
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = {
        let _ = (app, state, directory, restart);
        Err("当前平台尚未接入此启动方式，请从系统打开工具".into())
    };
    INVENTORY_CACHE.invalidate().await;
    result
}

#[tauri::command]
pub async fn osir_launch_workbuddy(
    app: tauri::AppHandle,
    restart: Option<bool>,
) -> Result<WorkBuddyLaunch, String> {
    #[cfg(target_os = "macos")]
    {
        let result = macos::launch_workbuddy(&app, restart.unwrap_or(false)).await;
        INVENTORY_CACHE.invalidate().await;
        result
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, restart);
        Err("当前平台尚未接入 WorkBuddy 启动，请从系统应用列表打开".into())
    }
}

/// Desktop only: no Codex CLI fallback, provider change, or configuration write.
#[tauri::command]
pub async fn osir_launch_codex_desktop(app: tauri::AppHandle) -> Result<OsirToolLaunch, String> {
    #[cfg(target_os = "macos")]
    {
        let result = macos::launch_desktop(&app, "codex", false).await;
        INVENTORY_CACHE.invalidate().await;
        result
    }
    #[cfg(target_os = "windows")]
    {
        let result = windows::launch_desktop(&app, "codex", false).await;
        INVENTORY_CACHE.invalidate().await;
        result
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        Err("当前平台尚未接入 Codex Desktop 启动，请从系统应用列表打开".into())
    }
}

/// Fixed homepage verified against Tencent's official macOS installation guide.
/// Opens the browser only; it neither downloads a package nor installs an app.
#[tauri::command]
pub fn osir_open_workbuddy_download(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(WORKBUDDY_DOWNLOAD_URL, None::<String>)
        .map_err(|_| "无法打开 WorkBuddy 官网，请稍后重试".into())
}

#[cfg(test)]
#[path = "osir_tools_tests.rs"]
mod tests;
