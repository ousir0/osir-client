//! Native-only, single-flight installer. Opening an installer is never installation evidence.
#[path = "osir_anywhere_install_completion.rs"]
mod completion;
#[path = "osir_anywhere_install_io.rs"]
mod io;
#[path = "osir_anywhere_install_manifest.rs"]
mod manifest;
#[path = "osir_anywhere_install_revision.rs"]
mod revision;
#[cfg(target_os = "macos")]
#[path = "osir_anywhere_install_shutdown.rs"]
pub(crate) mod shutdown;
#[path = "osir_anywhere_install_signature.rs"]
mod signature;
use serde::Serialize;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnywhereInstallPhase {
    Idle,
    Downloading,
    Ready,
    Opening,
    WaitingInstallation,
    Cancelled,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnywhereInstallStatus {
    pub phase: AnywhereInstallPhase,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
    pub install_id: u64,
    pub target_version: Option<String>,
    pub target_revision: Option<u32>,
    pub installation_complete: bool,
}
impl Default for AnywhereInstallStatus {
    fn default() -> Self {
        Self {
            phase: AnywhereInstallPhase::Idle,
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
            install_id: 0,
            target_version: None,
            target_revision: None,
            installation_complete: false,
        }
    }
}

#[derive(Serialize)]
pub struct AnywhereReleaseStatus {
    pub status: &'static str,
    pub version: Option<String>,
    pub revision: u32,
    pub platform: Option<String>,
    pub arch: Option<String>,
    pub size: Option<u64>,
    pub error: Option<String>,
    #[serde(rename = "systemSignature", skip_serializing_if = "Option::is_none")]
    pub system_signature: Option<&'static str>,
}

// The path/version are supplied by native installation detection, never IPC,
// an environment override, a server value or a cached release-feed revision.
pub(super) fn local_revision(path: &str, version: &str) -> Result<u32, String> {
    revision::installed(std::path::Path::new(path), version)
}

#[derive(Default)]
struct Inner {
    status: AnywhereInstallStatus,
    active: bool,
    cancel: Option<tokio::sync::watch::Sender<bool>>,
    completion: Option<completion::Target>,
}
#[derive(Clone, Default)]
pub struct OsirAnywhereInstaller {
    inner: Arc<Mutex<Inner>>,
    // Retain only this process's private cache. Never trust renderer paths or
    // previous runs' files; rehash and recheck system signatures on every use.
    cache: Arc<tokio::sync::Mutex<Option<tempfile::TempDir>>>,
}
impl OsirAnywhereInstaller {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    fn status(&self) -> AnywhereInstallStatus {
        self.lock().status.clone()
    }
    fn begin(
        &self,
        expected_install_id: u64,
    ) -> Result<(AnywhereInstallStatus, tokio::sync::watch::Receiver<bool>), String> {
        let mut inner = self.lock();
        if inner.active || inner.status.install_id != expected_install_id {
            return Err("安装状态已改变，请重新检测后重试".into());
        }
        let install_id = inner.status.install_id.checked_add(1).ok_or("请重启客户端后重试")?;
        let (send, receive) = tokio::sync::watch::channel(false);
        inner.cancel = Some(send);
        inner.active = true;
        inner.completion = None;
        inner.status = AnywhereInstallStatus {
            phase: AnywhereInstallPhase::Downloading,
            install_id,
            ..Default::default()
        };
        Ok((inner.status.clone(), receive))
    }
    fn progress(&self, phase: AnywhereInstallPhase, downloaded: u64, total: Option<u64>) {
        let mut inner = self.lock();
        if inner.status.phase != AnywhereInstallPhase::Cancelled {
            inner.status.phase = phase;
            inner.status.downloaded_bytes = downloaded;
            inner.status.total_bytes = total;
            inner.status.error = None;
        }
    }
    fn finish(&self, result: Result<(), String>) {
        let mut inner = self.lock();
        if inner.status.phase != AnywhereInstallPhase::Cancelled {
            if let Err(error) = result {
                inner.status.phase = AnywhereInstallPhase::Error;
                inner.status.error = Some(error);
            }
        }
        inner.active = false;
        inner.cancel = None;
    }
    fn cancel(&self) -> AnywhereInstallStatus {
        let mut inner = self.lock();
        // Once handed to the OS, cancellation cannot safely undo an install or
        // kill someone else's installer. Return the actual state unchanged.
        if inner.active
            && matches!(
                inner.status.phase,
                AnywhereInstallPhase::Downloading | AnywhereInstallPhase::Ready
            )
        {
            if let Some(cancel) = &inner.cancel {
                let _ = cancel.send(true);
            }
            inner.status.phase = AnywhereInstallPhase::Cancelled;
            inner.status.error = None;
        }
        inner.status.clone()
    }
    fn begin_opening(&self) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.status.phase == AnywhereInstallPhase::Cancelled {
            return Err("下载已取消".into());
        }
        inner.status.phase = AnywhereInstallPhase::Opening;
        Ok(())
    }
    async fn poll_status(&self, app: tauri::AppHandle) -> AnywhereInstallStatus {
        let target = {
            let inner = self.lock();
            if inner.status.phase != AnywhereInstallPhase::WaitingInstallation
                || inner.status.installation_complete
            {
                return inner.status.clone();
            }
            inner.completion.clone()
        };
        if let Some(target) = target {
            let local = super::osir_tools::osir_anywhere_detect(app).await;
            if local.status == "installed" && local.installed {
                if let (Some(path), Some(version), Some(revision)) =
                    (local.path, local.version, local.revision)
                {
                    if let Ok(current) = completion::marker(std::path::Path::new(&path)) {
                        // Read the packed target between stable file markers;
                        // never pair old metadata with a partially replaced file.
                        if local_revision(&path, &version) == Ok(revision)
                            && completion::marker(std::path::Path::new(&path)).as_ref()
                                == Ok(&current)
                            && target.confirmed(&version, revision, &current)
                        {
                            self.confirm_installation(target.install_id);
                        }
                    }
                }
            }
        }
        self.status()
    }
    fn confirm_installation(&self, install_id: u64) {
        let mut inner = self.lock();
        if inner.status.install_id == install_id
            && inner.status.phase == AnywhereInstallPhase::WaitingInstallation
        {
            inner.status.installation_complete = true;
        }
    }
    async fn run(
        &self,
        app: tauri::AppHandle,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), String> {
        if crate::distribution::is_isolated() {
            return Err("隔离开发环境不能安装本机应用，请在正式版中操作".into());
        }
        let (platform, arch) = io::target()?;
        let release = tokio::select! {
            _ = cancel.wait_for(|cancelled| *cancelled) => return Err("下载已取消".into()),
            release = manifest::fetch() => release?.ok_or("正式安装包尚未开放")?,
        };
        let artifact = release.select(platform, arch, &app.package_info().version.to_string())?;
        {
            let mut inner = self.lock();
            inner.status.target_version = Some(release.version.clone());
            inner.status.target_revision = Some(release.revision);
        }
        tokio::select! {
            _ = cancel.wait_for(|cancelled| *cancelled) => return Err("下载已取消".into()),
            compatible = release.check_server() => compatible?,
        }
        let local = tokio::select! {
            _ = cancel.wait_for(|cancelled| *cancelled) => return Err("下载已取消".into()),
            local = super::osir_tools::osir_anywhere_detect(app.clone()) => local,
        };
        if local.status == "error" {
            return Err(local.error.unwrap_or("请先修复已有 Anywhere 安装".into()));
        }
        if local.installed {
            let installed = local
                .version
                .as_deref()
                .ok_or("无法确认已有应用版本，请先修复安装")?;
            if local.path.is_none() {
                return Err("无法确认已有应用安装路径".into());
            }
            if revision::needs_install(
                &release.version,
                release.revision,
                installed,
                local.revision.ok_or("无法确认已有应用修订，请先修复安装")?,
            )? {
                return Err("本机 Anywhere 比当前安装包更新，已阻止降级安装".into());
            }
            // Mac uses the targeted graceful-quit lifecycle immediately before
            // opening the verified installer. Windows retains its existing gate.
            #[cfg(not(target_os = "macos"))]
            if local.runtime_status != super::osir_tools::ToolRuntimeStatus::Stopped {
                return Err("请先退出 Anywhere，再安装更新；正在进行的对话可能中断".into());
            }
        }
        self.progress(AnywhereInstallPhase::Downloading, 0, Some(artifact.size));
        let mut cache = self.cache.lock().await;
        if cache.is_none() {
            *cache = Some(io::private_cache().await?);
        }
        let root = cache.as_ref().ok_or("无法创建安装缓存")?.path();
        let path = io::download(self, root, &artifact, &mut cancel).await?;
        io::mark_download_origin(&path, &artifact).await?;
        let _pinned_package = io::pin_file(&path)?;
        self.progress(
            AnywhereInstallPhase::Ready,
            artifact.size,
            Some(artifact.size),
        );
        // Do not drop a live DMG verification future on cancellation: it owns a
        // read-only mount and must detach it before the single-flight ends.
        // Cancellation still updates status immediately; no installer is opened.
        io::verify_system_package(&path, &artifact, &release.version).await?;
        // A second identity/size/hash check immediately before opening catches
        // changed cached bytes; only native-owned paths can reach this point.
        io::verify_file(&path, &artifact, &mut cancel).await?;
        self.begin_opening()?;
        #[cfg(target_os = "macos")]
        let _shutdown_gate = super::osir_tools::macos::prepare_anywhere_install(&app).await?;
        // Snapshot immediately before the OS handoff. The same build already
        // present on disk is not proof a subsequent drag-copy succeeded.
        let before = super::osir_tools::osir_anywhere_detect(app.clone()).await;
        if before.status == "error" {
            return Err("无法确认安装前的 Anywhere 状态，已停止安装".into());
        }
        let marker = if before.installed {
            Some(completion::marker(std::path::Path::new(
                before.path.as_deref().ok_or("无法确认已有安装路径")?,
            ))?)
        } else {
            None
        };
        {
            let mut inner = self.lock();
            inner.completion = Some(completion::Target {
                install_id: inner.status.install_id,
                version: release.version.clone(),
                revision: release.revision,
                before: marker,
            });
        }
        // Recheck pinned bytes after graceful shutdown as well; shutdown must
        // not create a new unchecked interval before opening a cached package.
        io::verify_file(&path, &artifact, &mut cancel).await?;
        io::open_installer(&path).await?;
        self.progress(
            AnywhereInstallPhase::WaitingInstallation,
            artifact.size,
            Some(artifact.size),
        );
        // UI polls install_status through waiting_installation. Native target
        // metadata plus file replacement, never OS handoff, confirms completion.
        Ok(())
    }
}

#[tauri::command]
pub async fn osir_anywhere_release(app: tauri::AppHandle) -> AnywhereReleaseStatus {
    let target = io::target();
    let mut result = AnywhereReleaseStatus {
        status: "unsupported",
        version: None,
        revision: 0,
        platform: None,
        arch: None,
        size: None,
        error: None,
        system_signature: None,
    };
    let (platform, arch) = match target {
        Ok(target) => target,
        Err(error) => {
            result.error = Some(error);
            return result;
        }
    };
    result.platform = Some(platform.into());
    result.arch = Some(arch.into());
    match manifest::fetch().await {
        Ok(None) => result.status = "not_configured",
        Err(error) => {
            result.status = "error";
            result.error = Some(error);
        }
        Ok(Some(release)) => {
            result.version = Some(release.version.clone());
            result.revision = release.revision;
            match release.select(platform, arch, &app.package_info().version.to_string()) {
                Ok(artifact) => match release.check_server().await {
                    Ok(()) => {
                        result.status = "ready";
                        result.system_signature = artifact
                            .signature_policy()
                            .ok()
                            .map(|policy| policy.as_str());
                        result.arch = Some(artifact.arch);
                        result.size = Some(artifact.size);
                    }
                    Err(error) => {
                        result.status = "error";
                        result.error = Some(error);
                    }
                },
                Err(error) => {
                    result.status = "unsupported";
                    result.error = Some(error);
                }
            }
        }
    }
    result
}

#[tauri::command]
pub async fn osir_anywhere_install_status(
    app: tauri::AppHandle,
    state: tauri::State<'_, OsirAnywhereInstaller>,
) -> Result<AnywhereInstallStatus, String> {
    Ok(state.poll_status(app).await)
}

#[tauri::command]
pub fn osir_anywhere_cancel_install(
    state: tauri::State<'_, OsirAnywhereInstaller>,
) -> AnywhereInstallStatus {
    state.cancel()
}

#[tauri::command]
pub fn osir_anywhere_install(
    app: tauri::AppHandle,
    state: tauri::State<'_, OsirAnywhereInstaller>,
    expected_install_id: u64,
) -> Result<AnywhereInstallStatus, String> {
    let (initial, receive) = state.begin(expected_install_id)?;
    let installer = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        // Ensure even an unexpected worker panic releases the single-flight gate.
        use futures::FutureExt;
        let result = std::panic::AssertUnwindSafe(installer.run(app, receive))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err("安装任务中断，请重试".into()));
        installer.finish(result);
    });
    Ok(initial)
}

#[cfg(test)]
#[path = "osir_anywhere_install_tests.rs"]
mod tests;
