//! Native installation service. Only an explicit install/restore/recover mutates
//! applications. Target is private ~/Applications/OSIR Codex/Codex.app.
use super::{
    download::{Cancellation, DownloadError, ReleaseTransport, VerifiedFile},
    fs_backend::FsBackend,
    release::{Architecture, ReleaseMetadata, Source},
    signature_verifier::SystemVerifier,
    Journal, Step, Verifier,
};
use crate::services::osir::desktop_install as tx;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum InstallError {
    Busy,
    Cancelled,
    Running,
    Changed,
    Storage,
    InvalidPackage,
    Verification,
    ManualRequired,
    PermissionDenied,
    Transport(DownloadError),
}
pub type Result<T> = std::result::Result<T, InstallError>;
impl From<tx::Error> for InstallError {
    fn from(e: tx::Error) -> Self {
        match e {
            tx::Error::Verification => Self::Verification,
            tx::Error::ManualRequired => Self::ManualRequired,
            _ => Self::Storage,
        }
    }
}
impl From<DownloadError> for InstallError {
    fn from(e: DownloadError) -> Self {
        if e == DownloadError::Cancelled {
            Self::Cancelled
        } else {
            Self::Transport(e)
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationStatus {
    pub operation_id: Option<String>,
    pub phase: Option<&'static str>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestorePoint {
    pub receipt_id: String,
    pub target_build: u64,
    pub previous_build: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
    pub source: Source,
    pub actual_source: Source,
    pub installed_build: Option<u64>,
    pub highest_detected_build: Option<u64>,
    pub comparison_complete: bool,
    pub release: ReleaseMetadata,
    pub target_path: String,
    pub restore_points: Vec<RestorePoint>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub receipt_id: String,
    pub build: Option<u64>,
    pub step: Step,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalReport {
    pub installed_build: Option<u64>,
    pub target_path: String,
    pub restore_points: Vec<RestorePoint>,
    pub recovery_required: bool,
    pub update_blocked_reason: Option<&'static str>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetReceipt {
    schema: u32,
    path: String,
}
#[derive(Clone, Copy)]
pub enum LocalAction {
    Inspect,
    Reveal,
}
struct Active {
    id: String,
    token: Cancellation,
    committing: bool,
    phase: &'static str,
}
struct Inner {
    base_root: PathBuf,
    root: PathBuf,
    target: PathBuf,
    transport: ReleaseTransport,
    active: Arc<Mutex<Option<Active>>>,
}
#[derive(Clone)]
pub struct InstallCoordinator(Arc<Inner>);
struct Lease {
    inner: Arc<Inner>,
    id: String,
    token: Cancellation,
}

/// Keeps both installation leases alive while the shared native launcher
/// validates and activates this exact bundle. Dropping it permits updates again.
pub(crate) struct PreparedLocalLaunch {
    _filesystem: FsBackend,
    _lease: Lease,
    bundle: PathBuf,
    report: LocalReport,
}
impl PreparedLocalLaunch {
    pub(crate) fn bundle(&self) -> &Path {
        &self.bundle
    }
    pub(crate) fn report(&self) -> &LocalReport {
        &self.report
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut active) = self.inner.active.lock() {
            if active.as_ref().is_some_and(|a| a.id == self.id) {
                *active = None;
            }
        }
    }
}
impl Lease {
    fn phase(&self, phase: &'static str) {
        if let Ok(mut active) = self.inner.active.lock() {
            if let Some(a) = active.as_mut().filter(|a| a.id == self.id) {
                a.phase = phase;
            }
        }
    }
    fn commit(&self) -> Result<()> {
        let mut active = self
            .inner
            .active
            .lock()
            .map_err(|_| InstallError::Storage)?;
        let a = active
            .as_mut()
            .filter(|a| a.id == self.id)
            .ok_or(InstallError::Changed)?;
        a.token.check()?;
        a.committing = true;
        a.phase = "applying";
        Ok(())
    }
}
impl InstallCoordinator {
    /// Resolves home in native code. No IPC-controlled path or installation URL.
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or(InstallError::Storage)?
            .canonicalize()
            .map_err(|_| InstallError::Storage)?;
        let root = home.join("Applications").join("OSIR Codex");
        Ok(Self(Arc::new(Inner {
            base_root: root.clone(),
            target: root.join("Codex.app"),
            root,
            transport: ReleaseTransport::new()?,
            active: Arc::new(Mutex::new(None)),
        })))
    }
    /// Renderer may only select an exact standard-directory Codex candidate.
    /// Its bundle identity, signature and build are checked again before use.
    pub fn for_target(&self, requested: Option<&str>) -> Result<Self> {
        let Some(requested) = requested else {
            return Ok(self.clone());
        };
        let target = PathBuf::from(requested);
        if target == self.0.base_root.join("Codex.app") {
            return Ok(self.clone());
        }
        let home_apps = self.0.base_root.parent().ok_or(InstallError::Changed)?;
        if !matches!(
            target.file_name().and_then(|s| s.to_str()),
            Some("Codex.app" | "ChatGPT.app")
        ) || !matches!(target.parent(), Some(parent) if parent == Path::new("/Applications") || parent == home_apps)
            || target
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .as_deref()
                != target.parent()
        {
            return Err(InstallError::Changed);
        }
        let normalized = target
            .parent()
            .ok_or(InstallError::Changed)?
            .canonicalize()
            .map_err(|_| InstallError::Changed)?
            .join(target.file_name().ok_or(InstallError::Changed)?);
        if normalized.as_os_str() != std::ffi::OsStr::new(requested) {
            return Err(InstallError::Changed);
        }
        use sha2::{Digest, Sha256};
        let key = format!("{:x}", Sha256::digest(requested.as_bytes()));
        let root = self.0.base_root.join("targets").join(key);
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && root.is_dir()
                    && target_receipt(&root).is_ok_and(|receipt| receipt == target) => {}
            _ => return Err(InstallError::Changed),
        }
        Ok(Self(Arc::new(Inner {
            base_root: self.0.base_root.clone(),
            root,
            target,
            transport: self.0.transport.clone(),
            active: self.0.active.clone(),
        })))
    }
    pub fn recovery_targets(&self) -> Result<Vec<String>> {
        let targets = self.0.base_root.join("targets");
        let metadata = match fs::symlink_metadata(&targets) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(InstallError::Storage),
        };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(InstallError::Storage);
        }
        let mut paths = Vec::new();
        for item in fs::read_dir(&targets)
            .map_err(|_| InstallError::Storage)?
            .take(64)
        {
            let root = item.map_err(|_| InstallError::Storage)?.path();
            let metadata = fs::symlink_metadata(&root).map_err(|_| InstallError::Storage)?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                return Err(InstallError::Storage);
            }
            if let Ok(path) = target_receipt(&root) {
                if self.for_target(path.to_str()).is_ok() {
                    paths.push(path.to_string_lossy().into_owned());
                }
            }
        }
        Ok(paths)
    }
    pub fn status(&self) -> Result<OperationStatus> {
        let active = self.0.active.lock().map_err(|_| InstallError::Storage)?;
        Ok(OperationStatus {
            operation_id: active.as_ref().map(|a| a.id.clone()),
            phase: active.as_ref().map(|a| a.phase),
        })
    }
    /// Local-only inspection; no feed requests or automatic recovery.
    pub async fn local(&self, action: LocalAction) -> Result<LocalReport> {
        let lease = self.lease()?;
        lease.phase("local_inspection");
        tokio::task::spawn_blocking(move || {
            let root = &lease.inner.root;
            let mut report = empty_local_report(&lease.inner.target);
            if !root.try_exists().map_err(|_| InstallError::Storage)? {
                return match action {
                    LocalAction::Inspect => {
                        report.installed_build = installed(&lease.inner.target)?;
                        Ok(report)
                    }
                    LocalAction::Reveal => {
                        reveal_local(lease.inner.target.parent().ok_or(InstallError::Changed)?)?;
                        Ok(report)
                    }
                };
            }
            // Local inspection and reveal never launch an app.
            let backend = FsBackend::open_for_target(root, &lease.inner.target)?;
            if matches!(action, LocalAction::Reveal) {
                reveal_local(lease.inner.target.parent().ok_or(InstallError::Changed)?)?;
                return Ok(report);
            }
            read_local_report(root, &lease.inner.target, &backend)
        })
        .await
        .map_err(|_| InstallError::Storage)?
    }

    pub(crate) async fn prepare_local_launch(&self) -> Result<PreparedLocalLaunch> {
        let lease = self.lease()?;
        lease.phase("launching");
        tokio::task::spawn_blocking(move || {
            let root = &lease.inner.root;
            if !root.try_exists().map_err(|_| InstallError::Storage)? {
                return Err(InstallError::Changed);
            }
            let backend = FsBackend::open_for_target(root, &lease.inner.target)?;
            let report = read_local_report(root, &lease.inner.target, &backend)?;
            validate_local_launch(&report)?;
            let bundle = lease.inner.target.clone();
            Ok(PreparedLocalLaunch {
                _filesystem: backend,
                _lease: lease,
                bundle,
                report,
            })
        })
        .await
        .map_err(|_| InstallError::Storage)?
    }
    pub fn cancel(&self, operation_id: &str) -> Result<bool> {
        let active = self.0.active.lock().map_err(|_| InstallError::Storage)?;
        if let Some(a) = active
            .as_ref()
            .filter(|a| a.id == operation_id && !a.committing)
        {
            a.token.cancel();
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn lease(&self) -> Result<Lease> {
        let mut active = self.0.active.lock().map_err(|_| InstallError::Storage)?;
        if active.is_some() {
            return Err(InstallError::Busy);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let token = Cancellation::default();
        *active = Some(Active {
            id: id.clone(),
            token: token.clone(),
            committing: false,
            phase: "checking",
        });
        Ok(Lease {
            inner: self.0.clone(),
            id,
            token,
        })
    }
    pub async fn check(&self, source: Source) -> Result<CheckReport> {
        let lease = self.lease()?;
        let this = self.clone();
        // Owned task preserves cancellation/status if an IPC waiter disappears.
        tokio::spawn(async move {
            let root = this.0.root.clone();
            let target = this.0.target.clone();
            let (installed_build, restore_points, architecture) =
                tokio::task::spawn_blocking(move || {
                    let (build, points) = inspect(&root, &target)?;
                    Ok::<_, InstallError>((build, points, target_arch(&target, build)?))
                })
                .await
                .map_err(|_| InstallError::Storage)??;
            let plan = this
                .0
                .transport
                .fetch_release(
                    source,
                    architecture,
                    installed_build.unwrap_or(0),
                    &lease.token,
                )
                .await?;
            lease.token.check()?;
            let release = plan.metadata().clone();
            Ok(CheckReport {
                source,
                installed_build,
                actual_source: plan.source(),
                highest_detected_build: installed_build,
                comparison_complete: true,
                release,
                target_path: this.0.target.to_string_lossy().into_owned(),
                restore_points,
            })
        })
        .await
        .map_err(|_| InstallError::Storage)?
    }
    pub async fn install(&self, source: Source, expected_build: u64) -> Result<InstallReport> {
        self.install_expected(source, expected_build, None).await
    }
    pub async fn install_expected(
        &self,
        source: Source,
        expected_build: u64,
        expected: Option<(Option<u64>, Source)>,
    ) -> Result<InstallReport> {
        let lease = self.lease()?;
        let this = self.clone();
        tokio::spawn(async move {
            let root = this.0.root.clone();
            let target = this.0.target.clone();
            let (before, architecture) = tokio::task::spawn_blocking(move || {
                initialize(&root)?;
                store_target_receipt(&root, &target)?;
                let backend = FsBackend::open_for_target(&root, &target)?;
                ensure_target_writable(&target)?;
                let build = installed(&target)?;
                not_running(&target)?;
                drop(backend);
                Ok::<_, InstallError>((build, target_arch(&target, build)?))
            })
            .await
            .map_err(|_| InstallError::Storage)??;
            if expected_build <= before.unwrap_or(0)
                || expected.is_some_and(|(build, _)| build != before)
            {
                return Err(InstallError::Changed);
            }
            let plan = this
                .0
                .transport
                .fetch_release(source, architecture, before.unwrap_or(0), &lease.token)
                .await?;
            if plan.metadata().build != expected_build || !plan.metadata().update_available {
                return Err(InstallError::Changed);
            }
            if expected.is_some_and(|(_, source)| source != plan.source()) {
                return Err(InstallError::Changed);
            }
            let cache = this.0.root.join("downloads");
            lease.phase("downloading");
            let verified = this
                .0
                .transport
                .download_observed(plan, &cache, &lease.token, || {
                    lease.phase("verifying_artifact")
                })
                .await?;
            tokio::task::spawn_blocking(move || install_verified(lease, verified, before))
                .await
                .map_err(|_| InstallError::Storage)?
        })
        .await
        .map_err(|_| InstallError::Storage)?
    }
    pub async fn recover(&self) -> Result<Vec<InstallReport>> {
        let lease = self.lease()?;
        tokio::task::spawn_blocking(move || {
            if !lease
                .inner
                .root
                .try_exists()
                .map_err(|_| InstallError::Storage)?
            {
                return Ok(Vec::new());
            }
            let root = lease.inner.root.clone();
            let mut backend = FsBackend::open_for_target(&root, &lease.inner.target)?;
            let journals = backend.scan()?;
            ensure_target_writable(&lease.inner.target)?;
            not_running(&lease.inner.target)?;
            lease.commit()?;
            let mut reports = Vec::new();
            for mut j in journals {
                tx::recover(&mut backend, &SystemVerifier, &root, &mut j)?;
                reports.push(report(&j));
            }
            Ok(reports)
        })
        .await
        .map_err(|_| InstallError::Storage)?
    }
    pub async fn restore(&self, expected: RestorePoint) -> Result<InstallReport> {
        let lease = self.lease()?;
        tokio::task::spawn_blocking(move || {
            let root = lease.inner.root.clone();
            let mut backend = FsBackend::open_for_target(&root, &lease.inner.target)?;
            ensure_target_writable(&lease.inner.target)?;
            let mut j = backend.load(&expected.receipt_id)?;
            let actual = tx::preflight_restore(&backend, &SystemVerifier, &root, &j)?;
            if actual.target_build != expected.target_build
                || actual.previous_build != expected.previous_build
            {
                return Err(InstallError::Changed);
            }
            not_running(&j.install)?;
            lease.commit()?;
            tx::restore_point(&mut backend, &SystemVerifier, &root, &mut j, &actual)?;
            Ok(report(&j))
        })
        .await
        .map_err(|_| InstallError::Storage)?
    }
}
fn host_arch() -> Architecture {
    if std::env::consts::ARCH == "aarch64" {
        Architecture::Arm64
    } else {
        Architecture::X86_64
    }
}
fn target_arch(target: &Path, installed: Option<u64>) -> Result<Architecture> {
    if installed.is_none() {
        return Ok(host_arch());
    }
    match codex_mac_engine::sys::app_arch(&target.to_string_lossy()).as_deref() {
        Some("arm64") => Ok(Architecture::Arm64),
        Some("x86_64") => Ok(Architecture::X86_64),
        _ => Err(InstallError::Verification),
    }
}
fn report(j: &Journal) -> InstallReport {
    InstallReport {
        receipt_id: j.id.clone(),
        build: if j.step == Step::Completed {
            Some(j.target_build)
        } else if j.step == Step::RolledBack {
            j.previous_build
        } else {
            None
        },
        step: j.step,
    }
}
fn owned_dir(path: &Path, private: bool) -> Result<()> {
    let m = fs::symlink_metadata(path).map_err(|_| InstallError::Storage)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & if private { 0o077 } else { 0o022 } != 0
        || path.canonicalize().map_err(|_| InstallError::Storage)? != path
    {
        return Err(InstallError::Storage);
    }
    Ok(())
}
fn initialize(root: &Path) -> Result<()> {
    let ancestor = root
        .ancestors()
        .skip(1)
        .find(|path| path.exists())
        .ok_or(InstallError::Storage)?;
    owned_dir(ancestor, false)?;
    let relative = root
        .strip_prefix(ancestor)
        .map_err(|_| InstallError::Storage)?;
    let mut paths = Vec::new();
    let mut current = ancestor.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(InstallError::Storage);
        };
        current.push(name);
        paths.push((current.clone(), name != "Applications"));
    }
    paths.push((root.join("downloads"), true));
    for (path, private) in paths {
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(InstallError::Storage),
        }
        owned_dir(&path, private)?;
    }
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|_| InstallError::Storage)?;
    Ok(())
}
fn target_parent_writable(target: &Path) -> bool {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let mut parent = target.parent();
    while let Some(path) = parent {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.is_dir()
                    || metadata.file_type().is_symlink()
                    || metadata.mode() & 0o002 != 0
                    || (metadata.uid() != 0 && metadata.uid() != unsafe { libc::geteuid() })
                {
                    return false;
                }
                let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
                    return false;
                };
                return unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => parent = path.parent(),
            Err(_) => return false,
        }
    }
    false
}
fn target_receipt_path(root: &Path) -> PathBuf {
    root.join("target.json")
}
fn target_receipt(root: &Path) -> Result<PathBuf> {
    let path = target_receipt_path(root);
    let metadata = fs::symlink_metadata(&path).map_err(|_| InstallError::Changed)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() > 4096
    {
        return Err(InstallError::Changed);
    }
    let value: TargetReceipt =
        serde_json::from_slice(&fs::read(path).map_err(|_| InstallError::Changed)?)
            .map_err(|_| InstallError::Changed)?;
    if value.schema != 1 {
        return Err(InstallError::Changed);
    }
    let target = PathBuf::from(value.path);
    if !super::clean_absolute(&target)
        || !matches!(
            target.file_name().and_then(|n| n.to_str()),
            Some("Codex.app" | "ChatGPT.app")
        )
    {
        return Err(InstallError::Changed);
    }
    Ok(target)
}
fn store_target_receipt(root: &Path, target: &Path) -> Result<()> {
    let path = target_receipt_path(root);
    let bytes = serde_json::to_vec(&TargetReceipt {
        schema: 1,
        path: target.to_string_lossy().into_owned(),
    })
    .map_err(|_| InstallError::Storage)?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(&bytes).map_err(|_| InstallError::Storage)?;
            file.sync_all().map_err(|_| InstallError::Storage)?;
            File::open(root)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| InstallError::Storage)?;
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && target_receipt(root)? == target => {}
        Err(_) => return Err(InstallError::Storage),
    }
    Ok(())
}
fn ensure_target_writable(target: &Path) -> Result<()> {
    if target_parent_writable(target) {
        Ok(())
    } else {
        Err(InstallError::PermissionDenied)
    }
}
fn installed(app: &Path) -> Result<Option<u64>> {
    match fs::symlink_metadata(&app) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(InstallError::Storage),
        Ok(_) => (),
    }
    let (_, build) = codex_mac_engine::sys::installed_codex_build_at_path(&app.to_string_lossy())
        .ok_or(InstallError::Verification)?;
    SystemVerifier.verify(&app, build)?;
    Ok(Some(build))
}
fn inspect(root: &Path, target: &Path) -> Result<(Option<u64>, Vec<RestorePoint>)> {
    if !root.try_exists().map_err(|_| InstallError::Storage)? {
        return Ok((installed(target)?, Vec::new()));
    }
    let backend = FsBackend::open_for_target(root, target)?;
    let build = installed(target)?;
    let mut points = Vec::new();
    for j in backend.scan()? {
        if let Ok(p) = tx::preflight_restore(&backend, &SystemVerifier, root, &j) {
            points.push(RestorePoint {
                receipt_id: p.receipt_id,
                target_build: p.target_build,
                previous_build: p.previous_build,
            });
        }
    }
    Ok((build, points))
}
fn not_running(app: &Path) -> Result<()> {
    // Reuse engine path/translocation detection; additionally fail closed if the
    // process probe itself fails instead of treating errors as "not running".
    if codex_mac_engine::swap::codex_running_at(app)
        || codex_mac_engine::swap::translocated_instance_running(app)
    {
        return Err(InstallError::Running);
    }
    let mut escaped = String::new();
    for c in app.to_string_lossy().chars() {
        if ".^$*+?()[]{}|".contains(c) || c as u32 == 92 {
            escaped.push(char::from(92));
        }
        escaped.push(c);
    }
    let s = Command::new("/usr/bin/pgrep")
        .args(["-f", &format!("^{escaped}/Contents/MacOS/")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| InstallError::Running)?;
    if s.code() == Some(1) {
        Ok(())
    } else {
        Err(InstallError::Running)
    }
}
fn install_verified(
    lease: Lease,
    verified: VerifiedFile,
    previous: Option<u64>,
) -> Result<InstallReport> {
    let root = lease.inner.root.clone();
    let mut backend = FsBackend::open_for_target(&root, &lease.inner.target)?;
    lease.token.check()?;
    if installed(&lease.inner.target)? != previous {
        return Err(InstallError::Changed);
    }
    if verified.build() <= previous.unwrap_or(0) {
        return Err(InstallError::Changed);
    }
    ensure_target_writable(&lease.inner.target)?;
    not_running(&lease.inner.target)?;
    // Only terminal verified receipts are retired, never pending/manual work.
    let receipts = backend.scan()?;
    if receipts.iter().any(|j| {
        !matches!(
            j.step,
            Step::Completed | Step::RolledBack | Step::RetiredCompleted | Step::RetiredRolledBack
        )
    }) {
        return Err(InstallError::ManualRequired);
    }
    for mut j in receipts {
        if matches!(j.step, Step::Completed | Step::RolledBack) {
            tx::retire_receipt(&mut backend, &SystemVerifier, &root, &mut j)?;
        }
    }
    let mut j = backend.prepare(&lease.id, previous, verified.build())?;
    let work = j.staged.parent().ok_or(InstallError::Storage)?;
    lease.phase("extracting");
    unpack(&verified, work, &j.staged, &lease.token)?;
    lease.phase("verifying_application");
    SystemVerifier.verify(&j.staged, j.target_build)?;
    lease.token.check()?;
    if installed(&lease.inner.target)? != previous {
        return Err(InstallError::Changed);
    }
    if j.target_build <= previous.unwrap_or(0) {
        return Err(InstallError::Changed);
    }
    not_running(&j.install)?;
    lease.commit()?;
    // Preserve the existing fsynced journal / exclusive rename implementation;
    // engine swap alone would bypass first-install/recovery receipts.
    tx::begin(&mut backend, &SystemVerifier, &root, &mut j)?;
    if j.step != Step::Completed {
        return Err(InstallError::ManualRequired);
    }
    Ok(report(&j))
}

// Adapted from Manager app/mac_update.rs ZIP preflight + ditto extraction.
// The engine crate itself has no full ZIP extractor; preserve the same native
// metadata-preserving extraction instead of feeding ZIP files to a delta API.
fn unpack(
    verified: &VerifiedFile,
    work: &Path,
    staged: &Path,
    cancel: &Cancellation,
) -> Result<()> {
    let mut package = tempfile::NamedTempFile::new_in(work).map_err(|_| InstallError::Storage)?;
    let mut source = verified
        .file()
        .try_clone()
        .map_err(|_| InstallError::Storage)?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|_| InstallError::Storage)?;
    let mut buf = [0u8; 65536];
    let mut copied = 0u64;
    loop {
        cancel.check()?;
        let n = source.read(&mut buf).map_err(|_| InstallError::Storage)?;
        if n == 0 {
            break;
        }
        copied = copied
            .checked_add(n as u64)
            .ok_or(InstallError::InvalidPackage)?;
        if copied > verified.size() {
            return Err(InstallError::InvalidPackage);
        }
        package
            .write_all(&buf[..n])
            .map_err(|_| InstallError::Storage)?;
    }
    if copied != verified.size() {
        return Err(InstallError::InvalidPackage);
    }
    package
        .as_file()
        .sync_all()
        .map_err(|_| InstallError::Storage)?;
    preflight_zip(package.path(), work, cancel)?;
    let extract = tempfile::Builder::new()
        .prefix("extract-")
        .tempdir_in(work)
        .map_err(|_| InstallError::Storage)?;
    let mut child = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(package.path())
        .arg(extract.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| InstallError::InvalidPackage)?;
    let start = Instant::now();
    loop {
        if cancel.check().is_err() || start.elapsed() > Duration::from_secs(300) {
            let _ = child.kill();
            let _ = child.wait();
            cancel.check()?;
            return Err(InstallError::InvalidPackage);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return Err(InstallError::InvalidPackage),
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(InstallError::InvalidPackage);
            }
        }
    }
    let mut apps = Vec::new();
    for e in fs::read_dir(extract.path()).map_err(|_| InstallError::Storage)? {
        let e = e.map_err(|_| InstallError::Storage)?;
        if e.path().extension().is_some_and(|v| v == "app") {
            if !e.file_type().map_err(|_| InstallError::Storage)?.is_dir() {
                return Err(InstallError::InvalidPackage);
            }
            apps.push(e.path());
        }
    }
    if apps.len() != 1 {
        return Err(InstallError::InvalidPackage);
    }
    let app = apps.pop().ok_or(InstallError::InvalidPackage)?;
    measure_tree(&app, cancel)?;
    SystemVerifier.verify(&app, verified.build())?;
    if codex_mac_engine::sys::app_arch(&app.to_string_lossy()).as_deref()
        != Some(if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "x86_64"
        })
    {
        return Err(InstallError::InvalidPackage);
    }
    cancel.check()?;
    fs::rename(&app, staged).map_err(|_| InstallError::Storage)?;
    sync_tree(staged, cancel)?;
    File::open(work)
        .and_then(|f| f.sync_all())
        .map_err(|_| InstallError::Storage)?;
    Ok(())
}
const EXPANDED_LIMIT: u64 = 8 * 1024 * 1024 * 1024;
const ENTRY_LIMIT: usize = 200_000;
fn preflight_zip(path: &Path, work: &Path, cancel: &Cancellation) -> Result<()> {
    let file = File::open(path).map_err(|_| InstallError::Storage)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| InstallError::InvalidPackage)?;
    if archive.is_empty() || archive.len() > ENTRY_LIMIT {
        return Err(InstallError::InvalidPackage);
    }
    let mut seen = std::collections::HashSet::new();
    let mut links = Vec::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    for i in 0..archive.len() {
        cancel.check()?;
        let mut entry = archive
            .by_index(i)
            .map_err(|_| InstallError::InvalidPackage)?;
        let enclosed = entry
            .enclosed_name()
            .ok_or(InstallError::InvalidPackage)?
            .to_path_buf();
        if entry.name().as_bytes().contains(&92)
            || entry.name().as_bytes().contains(&0)
            || enclosed
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(InstallError::InvalidPackage);
        }
        let key = enclosed.to_string_lossy().to_lowercase();
        if !seen.insert(key.clone()) {
            return Err(InstallError::InvalidPackage);
        }
        let declared = entry.size();
        if declared > EXPANDED_LIMIT - total {
            return Err(InstallError::InvalidPackage);
        }
        let is_link = entry.is_symlink();
        let mut target = Vec::new();
        let mut actual = 0u64;
        loop {
            cancel.check()?;
            let n = entry
                .read(&mut buffer)
                .map_err(|_| InstallError::InvalidPackage)?;
            if n == 0 {
                break;
            }
            actual += n as u64;
            if actual > EXPANDED_LIMIT - total || (is_link && actual > 4096) {
                return Err(InstallError::InvalidPackage);
            }
            if is_link {
                target.extend_from_slice(&buffer[..n]);
            }
        }
        if actual != declared {
            return Err(InstallError::InvalidPackage);
        }
        total += actual;
        if is_link {
            let target = std::str::from_utf8(&target).map_err(|_| InstallError::InvalidPackage)?;
            validate_link(&enclosed, target)?;
            links.push(key);
        } else if entry
            .unix_mode()
            .is_some_and(|m| !matches!(m & libc::S_IFMT as u32, 0 | 0o100000 | 0o040000))
        {
            return Err(InstallError::InvalidPackage);
        }
    }
    // Never let ditto write an entry through an archive-created link.
    for link in links {
        let prefix = format!("{link}/");
        if seen.iter().any(|p| p.starts_with(&prefix)) {
            return Err(InstallError::InvalidPackage);
        }
    }
    use std::os::unix::ffi::OsStrExt;
    let path =
        std::ffi::CString::new(work.as_os_str().as_bytes()).map_err(|_| InstallError::Storage)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(InstallError::Storage);
    }
    let stat = unsafe { stat.assume_init() };
    if (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
        < total.saturating_add(512 * 1024 * 1024)
    {
        return Err(InstallError::Storage);
    }
    Ok(())
}
fn validate_link(link: &Path, target: &str) -> Result<()> {
    if target.is_empty() || target.as_bytes().contains(&0) || target.as_bytes().contains(&92) {
        return Err(InstallError::InvalidPackage);
    }
    let mut parts: Vec<_> = link
        .parent()
        .ok_or(InstallError::InvalidPackage)?
        .components()
        .filter_map(|c| {
            if let Component::Normal(v) = c {
                Some(v.to_os_string())
            } else {
                None
            }
        })
        .collect();
    let first = parts
        .first()
        .cloned()
        .filter(|s| Path::new(s).extension().is_some_and(|v| v == "app"))
        .ok_or(InstallError::InvalidPackage)?;
    for c in Path::new(target).components() {
        match c {
            Component::CurDir => (),
            Component::Normal(v) => parts.push(v.to_os_string()),
            Component::ParentDir if parts.len() > 1 => {
                parts.pop();
            }
            _ => return Err(InstallError::InvalidPackage),
        }
    }
    if parts.first() != Some(&first) {
        return Err(InstallError::InvalidPackage);
    }
    Ok(())
}
fn measure_tree(root: &Path, cancel: &Cancellation) -> Result<()> {
    let canonical = root
        .canonicalize()
        .map_err(|_| InstallError::InvalidPackage)?;
    let mut stack = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut count = 0;
    while let Some(p) = stack.pop() {
        cancel.check()?;
        count += 1;
        if count > ENTRY_LIMIT {
            return Err(InstallError::InvalidPackage);
        }
        let m = fs::symlink_metadata(&p).map_err(|_| InstallError::InvalidPackage)?;
        if m.file_type().is_symlink() {
            if !p
                .canonicalize()
                .map_err(|_| InstallError::InvalidPackage)?
                .starts_with(&canonical)
            {
                return Err(InstallError::InvalidPackage);
            }
        } else if m.is_dir() {
            for e in fs::read_dir(p).map_err(|_| InstallError::InvalidPackage)? {
                stack.push(e.map_err(|_| InstallError::InvalidPackage)?.path());
            }
        } else if m.is_file() {
            total = total
                .checked_add(m.len())
                .ok_or(InstallError::InvalidPackage)?;
            if total > EXPANDED_LIMIT {
                return Err(InstallError::InvalidPackage);
            }
        } else {
            return Err(InstallError::InvalidPackage);
        }
    }
    Ok(())
}
fn sync_tree(root: &Path, cancel: &Cancellation) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    let mut dirs = Vec::new();
    while let Some(p) = stack.pop() {
        cancel.check()?;
        let m = fs::symlink_metadata(&p).map_err(|_| InstallError::Storage)?;
        if m.is_dir() {
            for e in fs::read_dir(&p).map_err(|_| InstallError::Storage)? {
                stack.push(e.map_err(|_| InstallError::Storage)?.path());
            }
            dirs.push(p);
        } else if m.is_file() {
            File::open(p)
                .and_then(|f| f.sync_all())
                .map_err(|_| InstallError::Storage)?;
        }
    }
    for p in dirs.into_iter().rev() {
        File::open(p)
            .and_then(|f| f.sync_all())
            .map_err(|_| InstallError::Storage)?;
    }
    Ok(())
}

fn empty_local_report(target: &Path) -> LocalReport {
    LocalReport {
        installed_build: None,
        target_path: target.to_string_lossy().into_owned(),
        restore_points: Vec::new(),
        recovery_required: false,
        update_blocked_reason: if target_parent_writable(target) {
            None
        } else {
            Some("此安装目录不可更新，请选择用户目录安装。")
        },
    }
}

fn read_local_report(root: &Path, target: &Path, backend: &FsBackend) -> Result<LocalReport> {
    let mut report = empty_local_report(target);
    let journals = backend.scan()?;
    report.recovery_required = journals.iter().any(|j| {
        !matches!(
            j.step,
            Step::Completed | Step::RolledBack | Step::RetiredCompleted | Step::RetiredRolledBack
        )
    });
    if !report.recovery_required {
        report.installed_build = installed(target)?;
        for j in &journals {
            if let Ok(p) = tx::preflight_restore(backend, &SystemVerifier, root, j) {
                report.restore_points.push(RestorePoint {
                    receipt_id: p.receipt_id,
                    target_build: p.target_build,
                    previous_build: p.previous_build,
                });
            }
        }
    }
    Ok(report)
}

fn validate_local_launch(report: &LocalReport) -> Result<()> {
    if report.recovery_required {
        Err(InstallError::ManualRequired)
    } else if report.installed_build.is_none() {
        Err(InstallError::Changed)
    } else {
        Ok(())
    }
}

// Only opens the native-owned installation directory in Finder. App launches
// must go through osir_tools' profile-aware, single-instance controller.
fn reveal_local(path: &Path) -> Result<()> {
    let mut child = Command::new("/usr/bin/open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| InstallError::Storage)?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(InstallError::Storage)
                }
            }
            Ok(None) if start.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(InstallError::Storage);
            }
        }
    }
}

#[cfg(test)]
#[path = "real_updater_acceptance_tests.rs"]
mod real_updater_acceptance_tests;

#[cfg(test)]
mod local_launch_tests {
    use super::*;

    #[test]
    fn launch_requires_verified_installation_and_completed_recovery() {
        let mut report = empty_local_report(Path::new("/fixture/OSIR Codex/Codex.app"));
        assert!(matches!(
            validate_local_launch(&report),
            Err(InstallError::Changed)
        ));
        report.installed_build = Some(22530);
        assert!(validate_local_launch(&report).is_ok());
        report.recovery_required = true;
        assert!(matches!(
            validate_local_launch(&report),
            Err(InstallError::ManualRequired)
        ));
    }
    #[test]
    fn selected_target_is_exact_and_receipt_survives_a_missing_bundle() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().canonicalize().unwrap();
        let applications = home.join("Applications");
        let root = applications.join("OSIR Codex");
        initialize(&root).unwrap();
        let target = applications.join("ChatGPT.app");
        fs::create_dir(&target).unwrap();
        let coordinator = InstallCoordinator(Arc::new(Inner {
            base_root: root.clone(),
            root: root.clone(),
            target: root.join("Codex.app"),
            transport: ReleaseTransport::new().unwrap(),
            active: Arc::new(Mutex::new(None)),
        }));
        assert!(coordinator
            .for_target(Some(&format!(
                "{}/../Applications/ChatGPT.app",
                applications.display()
            )))
            .is_err());
        assert!(coordinator
            .for_target(Some(&applications.join("Other.app").to_string_lossy()))
            .is_err());
        let selected = coordinator.for_target(target.to_str()).unwrap();
        initialize(&selected.0.root).unwrap();
        store_target_receipt(&selected.0.root, &target).unwrap();
        assert!(store_target_receipt(&selected.0.root, &applications.join("Codex.app")).is_err());
        fs::remove_dir(&target).unwrap();
        let reopened = coordinator.for_target(target.to_str()).unwrap();
        assert_eq!(reopened.0.target, target);
        assert_eq!(
            coordinator.recovery_targets().unwrap(),
            vec![target.to_string_lossy().into_owned()]
        );
        assert!(target_parent_writable(&target));
        fs::set_permissions(&applications, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!target_parent_writable(&target));
    }
}
