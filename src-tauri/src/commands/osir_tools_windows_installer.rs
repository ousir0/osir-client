//! Windows installation is owned by Microsoft Store. A handoff is never
//! reported as a completed installation. No local MSIX deployment or elevation.
use super::super::osir_tools::{windows, windows_support as support};
use super::InstallSource;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Duration;
use tauri_plugin_opener::OpenerExt;

#[derive(Default)]
pub(super) struct Installer {
    checked: Mutex<Option<Plan>>,
}
#[derive(Clone)]
struct Plan {
    build: u64,
    target: String,
    installed: Option<u64>,
    source: &'static str,
}
impl Installer {
    pub fn status(&self) -> Value {
        // Store is a separate application; OSIR cannot invent its progress.
        json!({"operationId":null,"phase":null})
    }
    pub fn cancel(&self) -> Value {
        json!(false)
    }
    pub async fn check(
        &self,
        source: InstallSource,
        target: Option<String>,
    ) -> Result<Value, String> {
        let source = source_name(source)?;
        let (target, installed) = selected(target.as_deref()).await?;
        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| "无法准备官方版本检查")?
            .head(support::package_url(std::env::consts::ARCH)?)
            .send()
            .await
            .map_err(|_| "官方版本信息暂不可用，请重试")?;
        if !response.status().is_success() {
            return Err("官方版本信息暂不可用，请稍后重试".into());
        }
        let text = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|h| h.to_str().ok())
                .map(str::to_owned)
                .ok_or("官方安装包信息不完整")
        };
        if text("x-ms-meta-package_identity")? != support::CODEX_PACKAGE {
            return Err("官方安装包身份不符".into());
        }
        let version = text("x-ms-meta-package_version")?;
        let build = support::version_number(&version)?;
        let size: u64 = text("content-length")?
            .parse()
            .map_err(|_| "官方安装包大小无效")?;
        if size == 0 || size > 2 * 1024 * 1024 * 1024 {
            return Err("官方安装包大小无效".into());
        }
        *self.checked.lock().map_err(|_| "安装状态不可用")? = Some(Plan {
            build,
            target: target.clone(),
            installed,
            source,
        });
        Ok(json!({
            "source":source,"actualSource":"official","installedBuild":installed,
            "highestDetectedBuild":installed,"comparisonComplete":true,
            "release":{"build":build,"version":version,"fullSize":size,"updateAvailable":build>installed.unwrap_or(0)},
            "targetPath":target,"restorePoints":[],"platform":"windows",
            "installMethod":"microsoft_store","supportsAppBackup":false
        }))
    }
    pub async fn local(
        &self,
        app: &tauri::AppHandle,
        action: &str,
        target: Option<&str>,
    ) -> Result<Value, String> {
        let (target, installed) = selected(target).await?;
        match action {
            "inspect" => (),
            "launch" => {
                windows::launch_desktop(app, "codex", false).await?;
            }
            "reveal" => {
                let entries = support::discover("codex").await?;
                let entry = entries.first().ok_or("请先安装 Codex Desktop")?;
                tauri_plugin_opener::reveal_item_in_dir(&entry.executable)
                    .map_err(|_| "无法显示应用安装位置")?;
            }
            _ => return Err("安装器操作无效".into()),
        }
        Ok(
            json!({"targetPath":target,"installedBuild":installed,"restorePoints":[],"recoveryRequired":false,
            "updateBlockedReason":null,"platform":"windows","installMethod":"microsoft_store","supportsAppBackup":false}),
        )
    }
    pub async fn install(
        &self,
        app: &tauri::AppHandle,
        source: InstallSource,
        expected_build: u64,
        target: Option<String>,
        expected_installed: Option<u64>,
        expected_source: Option<InstallSource>,
    ) -> Result<Value, String> {
        if crate::distribution::is_isolated() {
            return Err("隔离开发环境不能安装本机 Windows 应用，请在正式版中操作".into());
        }
        let _guard = windows::guard()?;
        let source = source_name(source)?;
        if !matches!(expected_source, Some(InstallSource::Official)) {
            return Err("更新源已变化，请重新检查".into());
        }
        let plan = self
            .checked
            .lock()
            .map_err(|_| "安装状态不可用")?
            .clone()
            .ok_or("请先检查官方版本")?;
        let (current_target, current_build) = selected(target.as_deref()).await?;
        if plan.source != source
            || plan.build != expected_build
            || plan.installed != current_build
            || expected_installed != current_build
            || !support::same_path(&plan.target, &current_target)
            || expected_build <= current_build.unwrap_or(0)
        {
            return Err("版本或安装状态已变化，请重新检查".into());
        }
        app.opener()
            .open_url(support::STORE_URL, None::<String>)
            .map_err(|_| "无法打开 Microsoft Store 页面，请检查系统默认浏览器后重试")?;
        *self.checked.lock().map_err(|_| "安装状态不可用")? = None;
        Ok(
            json!({"step":"system_install_requested","build":null,"installed":false,
            "message":"已打开 Microsoft Store，请在系统中完成安装或更新，然后重新检测。"}),
        )
    }
}
fn source_name(source: InstallSource) -> Result<&'static str, String> {
    match source {
        InstallSource::Auto => Ok("auto"),
        InstallSource::Official => Ok("official"),
        InstallSource::OsirMirror => {
            Err("Windows 请使用官方更新源，安装由 Microsoft Store 接管".into())
        }
    }
}
async fn selected(target: Option<&str>) -> Result<(String, Option<u64>), String> {
    let entries = support::discover("codex").await?;
    if entries.len() > 1 {
        return Err("检测到多个 Codex 注册版本，请先在 Windows 应用设置中确认安装".into());
    }
    let entry = entries.first();
    let actual = entry
        .map(|e| e.root.clone())
        .unwrap_or_else(|| "Windows 当前用户（Microsoft Store）".into());
    if target.is_some_and(|t| !support::same_path(t, &actual)) {
        return Err("Windows 应用安装位置由系统管理，请重新检测安装状态".into());
    }
    Ok((
        actual,
        entry
            .map(|e| support::version_number(&e.version))
            .transpose()?,
    ))
}
