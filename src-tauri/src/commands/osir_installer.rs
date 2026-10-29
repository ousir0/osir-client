//! Native Codex installer IPC. Fixed sources and native-owned paths only.
#[cfg(target_os = "macos")]
use crate::services::osir::desktop_install::{
    coordinator::{InstallCoordinator, InstallError, RestorePoint},
    release::Source,
};
use serde::Deserialize;
use serde_json::Value;
use tauri::State;

pub struct OsirInstallerState {
    #[cfg(target_os = "macos")]
    coordinator: Result<InstallCoordinator, InstallError>,
}
impl OsirInstallerState {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            coordinator: InstallCoordinator::new(),
        }
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn manager(&self) -> Result<&InstallCoordinator, String> {
        self.coordinator
            .as_ref()
            .map_err(|_| "安装器初始化不可用".into())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    Auto,
    Official,
    OsirMirror,
}
#[cfg(target_os = "macos")]
impl From<InstallSource> for Source {
    fn from(s: InstallSource) -> Self {
        match s {
            InstallSource::Auto => Self::Auto,
            InstallSource::Official => Self::Official,
            InstallSource::OsirMirror => Self::OsirMirror,
        }
    }
}
#[cfg(target_os = "macos")]
fn safe(error: InstallError) -> String {
    match error {
        InstallError::Busy => "安装器正忙，请等待当前操作完成",
        InstallError::Cancelled => "操作已取消",
        InstallError::Running => "请先退出 Codex，再执行安装或恢复",
        InstallError::Changed => "版本或安装状态已改变，请重新检查",
        InstallError::Storage => "无法安全访问安装目录或事务记录",
        InstallError::InvalidPackage => "安装包校验或解包失败，未提交安装",
        InstallError::Verification => "应用签名、身份或版本校验失败",
        InstallError::ManualRequired => "存在未完成或需人工处理的安装事务，请检查恢复状态",
        InstallError::PermissionDenied => "此安装目录不可更新，请选择用户目录安装",
        InstallError::Transport(
            crate::services::osir::desktop_install::download::DownloadError::RateLimited,
        ) => "版本检查过于频繁，请稍后重试或切换更新源",
        InstallError::Transport(
            crate::services::osir::desktop_install::download::DownloadError::Timeout,
        ) => "发布信息或下载超时，请重试或切换更新源",
        InstallError::Transport(
            crate::services::osir::desktop_install::download::DownloadError::Signature,
        ) => "安装包签名校验失败，未提交安装",
        InstallError::Transport(_) => "发布信息或下载暂不可用，请重试或切换更新源",
    }
    .into()
}
#[cfg(target_os = "macos")]
fn json<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "无法编码安装器状态".into())
}
const UNSUPPORTED: &str = "Codex Desktop 原生安装目前仅支持 macOS";

#[tauri::command]
pub async fn osir_codex_install_check(
    state: State<'_, OsirInstallerState>,
    source: InstallSource,
    target_path: Option<String>,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        return json(
            state
                .manager()?
                .for_target(target_path.as_deref())
                .map_err(safe)?
                .check(source.into())
                .await
                .map_err(safe)?,
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (state, source, target_path);
        Err(UNSUPPORTED.into())
    }
}
#[tauri::command]
pub async fn osir_codex_install_start(
    state: State<'_, OsirInstallerState>,
    source: InstallSource,
    expected_build: u64,
    target_path: Option<String>,
    expected_installed_build: Option<u64>,
    expected_source: Option<InstallSource>,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        if target_path.is_some() && expected_source.is_none() {
            return Err("版本或安装状态已改变，请重新检查".into());
        }
        return json(
            state
                .manager()?
                .for_target(target_path.as_deref())
                .map_err(safe)?
                .install_expected(
                    source.into(),
                    expected_build,
                    expected_source.map(|source| (expected_installed_build, source.into())),
                )
                .await
                .map_err(safe)?,
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (
            state,
            source,
            expected_build,
            target_path,
            expected_installed_build,
            expected_source,
        );
        Err(UNSUPPORTED.into())
    }
}
#[tauri::command]
pub fn osir_codex_install_status(state: State<'_, OsirInstallerState>) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        return json(state.manager()?.status().map_err(safe)?);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state;
        Err(UNSUPPORTED.into())
    }
}
#[tauri::command]
pub fn osir_codex_install_cancel(
    state: State<'_, OsirInstallerState>,
    operation_id: String,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        return json(state.manager()?.cancel(&operation_id).map_err(safe)?);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (state, operation_id);
        Err(UNSUPPORTED.into())
    }
}
#[tauri::command]
pub async fn osir_codex_install_restore(
    state: State<'_, OsirInstallerState>,
    expected: Value,
    target_path: Option<String>,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        let point: RestorePoint =
            serde_json::from_value(expected).map_err(|_| "恢复点参数无效".to_string())?;
        return json(
            state
                .manager()?
                .for_target(target_path.as_deref())
                .map_err(safe)?
                .restore(point)
                .await
                .map_err(safe)?,
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (state, expected, target_path);
        Err(UNSUPPORTED.into())
    }
}
#[tauri::command]
pub async fn osir_codex_install_recover(
    state: State<'_, OsirInstallerState>,
    target_path: Option<String>,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        return json(
            state
                .manager()?
                .for_target(target_path.as_deref())
                .map_err(safe)?
                .recover()
                .await
                .map_err(safe)?,
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (state, target_path);
        Err(UNSUPPORTED.into())
    }
}

/// Native-owned installation only: no renderer paths or launch arguments.
#[tauri::command]
pub async fn osir_codex_install_local(
    app: tauri::AppHandle,
    state: State<'_, OsirInstallerState>,
    action: String,
    target_path: Option<String>,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        use crate::services::osir::desktop_install::coordinator::LocalAction;
        if action == "launch" {
            let prepared = state
                .manager()?
                .for_target(target_path.as_deref())
                .map_err(safe)?
                .prepare_local_launch()
                .await
                .map_err(safe)?;
            super::osir_tools::launch_installed_codex(&app, prepared.bundle()).await?;
            return json(prepared.report());
        }
        let action = match action.as_str() {
            "inspect" => LocalAction::Inspect,
            "reveal" => LocalAction::Reveal,
            _ => return Err("安装器操作无效".into()),
        };
        return json(
            state
                .manager()?
                .for_target(target_path.as_deref())
                .map_err(safe)?
                .local(action)
                .await
                .map_err(safe)?,
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, state, action, target_path);
        Err(UNSUPPORTED.into())
    }
}
