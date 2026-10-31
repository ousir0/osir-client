//! Strict tool-file adapter over the existing live engine. No directory backup,
//! credentials IPC, database replacement, or provider/state pointer changes.
//! Conflict detection shares the existing writer's optimistic rename boundary;
//! callers must ask the user to close external tool writers before restoring.
use crate::{
    config::{stage_write, StagedWrite},
    live::{
        engine::{self, DeviceStore, LiveFile},
        patch::{Guarded, WholeFile},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const MAX_FILE: u64 = 4 * 1024 * 1024;
const MAX_META: u64 = 32768;
const MAX_SNAPSHOTS: usize = 100;
#[cfg(windows)]
#[path = "tool_backup_windows.rs"]
mod windows;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Codex,
    Claude,
    Opencode,
    Gemini,
}
impl Tool {
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Opencode => "opencode",
            Self::Gemini => "gemini",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "code")]
pub enum BackupError {
    Storage,
    UnsafePath,
    TooLarge,
    InvalidSnapshot,
    Changed,
    PreviewExpired,
    PendingRecovery,
    ToolBusy,
    LimitReached,
    InvalidConfirmation,
}
type Result<T> = std::result::Result<T, BackupError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileInfo {
    pub path: String,
    pub hash: Option<String>,
    pub size_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub id: String,
    pub tool: Tool,
    pub created_at: String,
    pub safety: bool,
    pub files: Vec<FileInfo>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupState {
    pub snapshots: Vec<Snapshot>,
    pub pending_recovery: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub current_hash: Option<String>,
    pub snapshot_hash: Option<String>,
    pub current_size: u64,
    pub snapshot_size: u64,
    pub changed: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub preview_id: String,
    pub snapshot_id: String,
    pub tool: Tool,
    pub files: Vec<FileDiff>,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RestoreStatus {
    Restored,
    NoChanges,
    RolledBack,
    RecoveryRequired,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub status: RestoreStatus,
    pub safety_snapshot_id: Option<String>,
    pub conflicts: Vec<String>,
}
struct Grant {
    expires: Instant,
    root: PathBuf,
    snapshot: Snapshot,
    baseline: Vec<FileInfo>,
}
fn grants() -> &'static Mutex<HashMap<String, Grant>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Grant>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    safety_id: String,
    before: Vec<FileInfo>,
    after: Vec<FileInfo>,
}
pub struct ToolBackups {
    store: DeviceStore,
    tool: Tool,
    paths: Vec<PathBuf>,
    root: PathBuf,
}
impl ToolBackups {
    pub fn configured(tool: Tool) -> Self {
        // Explicit names only; do not include auth.json, OAuth files, .claude.json,
        // project files, sessions, databases or any path supplied by the UI.
        let paths = match tool {
            Tool::Codex => vec![
                crate::codex_config::get_codex_config_path(),
                crate::codex_config::get_codex_model_catalog_path(),
            ],
            Tool::Claude => {
                let dir = crate::config::get_claude_config_dir();
                vec![dir.join("settings.json"), dir.join("claude.json")]
            }
            Tool::Opencode => {
                let dir = crate::opencode_config::get_opencode_dir();
                vec![dir.join("opencode.json"), dir.join("opencode.jsonc")]
            }
            Tool::Gemini => vec![
                crate::gemini_config::get_gemini_env_path(),
                crate::gemini_config::get_gemini_settings_path(),
            ],
        };
        Self::new(DeviceStore::for_device(), tool, paths)
    }
    fn new(store: DeviceStore, tool: Tool, paths: Vec<PathBuf>) -> Self {
        let root = store.file("backups/osir-tool-config").join(tool.id());
        Self {
            store,
            tool,
            paths,
            root,
        }
    }
    fn journal_path(&self) -> PathBuf {
        self.root.join("restore-pending.json")
    }
    fn ready(&self) -> Result<()> {
        self.idle()?;
        self.no_pending_restore()
    }
    fn no_pending_restore(&self) -> Result<()> {
        if read(&self.journal_path(), MAX_META, true)?.is_some() {
            return Err(BackupError::PendingRecovery);
        }
        Ok(())
    }
    fn idle(&self) -> Result<()> {
        self.settled(true)
    }
    fn settled(&self, require_detached: bool) -> Result<()> {
        // Capture is read-only and safe while attached under the caller's switch
        // lock. Restore/recovery still require detachment. Never call load(): a
        // read-only check must not rename malformed live state.
        safe_path(&self.store.state_path())?;
        let state = crate::mode::state::identity_snapshot(&self.store)
            .map_err(|_| BackupError::ToolBusy)?;
        if state
            .apps
            .get(self.tool.id())
            .is_some_and(|s| s.pending.is_some() || (require_detached && s.attached))
        {
            return Err(BackupError::ToolBusy);
        }
        Ok(())
    }
    fn capture(&self) -> Result<Vec<Option<Vec<u8>>>> {
        self.paths
            .iter()
            .map(|p| read(p, MAX_FILE, false))
            .collect()
    }
    fn infos(&self, bytes: &[Option<Vec<u8>>]) -> Vec<FileInfo> {
        self.paths
            .iter()
            .zip(bytes)
            .map(|(p, b)| info(p, b.as_deref()))
            .collect()
    }
    fn check(&self, expected: &[FileInfo]) -> Result<()> {
        if self.infos(&self.capture()?) != expected {
            return Err(BackupError::Changed);
        }
        Ok(())
    }
    fn dir(&self, id: &str) -> Result<PathBuf> {
        valid_id(id)?;
        Ok(self.root.join(id))
    }
    fn valid_infos(&self, files: &[FileInfo]) -> bool {
        files.len() == self.paths.len()
            && files.iter().zip(&self.paths).all(|(f, p)| {
                f.path == p.to_string_lossy()
                    && f.size_bytes <= MAX_FILE
                    && f.hash.as_ref().map_or(f.size_bytes == 0, |h| {
                        h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())
                    })
            })
    }
    fn load(&self, id: &str) -> Result<Snapshot> {
        let bytes = read(&self.dir(id)?.join("manifest.json"), MAX_META, true)?
            .ok_or(BackupError::InvalidSnapshot)?;
        let s: Snapshot =
            serde_json::from_slice(&bytes).map_err(|_| BackupError::InvalidSnapshot)?;
        if s.id != id
            || s.tool != self.tool
            || !self.valid_infos(&s.files)
            || chrono::DateTime::parse_from_rfc3339(&s.created_at).is_err()
        {
            return Err(BackupError::InvalidSnapshot);
        }
        Ok(s)
    }
    fn payload(&self, s: &Snapshot) -> Result<Vec<Option<Vec<u8>>>> {
        s.files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let bytes = read(&self.dir(&s.id)?.join(format!("{i}.bin")), MAX_FILE, true)?;
                if info(Path::new(&f.path), bytes.as_deref()) != *f {
                    return Err(BackupError::InvalidSnapshot);
                }
                Ok(bytes)
            })
            .collect()
    }
    fn save(&self, bytes: &[Option<Vec<u8>>], safety: bool) -> Result<Snapshot> {
        private_dir(&self.store.file("backups/osir-tool-config"))?;
        private_dir(&self.root)?;
        if !safety && self.list_locked()?.snapshots.len() >= MAX_SNAPSHOTS {
            return Err(BackupError::LimitReached);
        }
        let s = Snapshot {
            id: uuid::Uuid::new_v4().to_string(),
            tool: self.tool,
            created_at: chrono::Utc::now().to_rfc3339(),
            safety,
            files: self.infos(bytes),
        };
        let dir = self.dir(&s.id)?;
        private_dir(&dir)?;
        for (i, b) in bytes.iter().enumerate() {
            if let Some(b) = b {
                private_write(&dir.join(format!("{i}.bin")), b)?;
            }
        }
        // A manifest is the publication marker. Incomplete snapshots are not listed.
        json_write(&dir.join("manifest.json"), &s)?;
        Ok(s)
    }
    fn list_locked(&self) -> Result<BackupState> {
        safe_path(&self.root)?;
        let mut snapshots = vec![];
        match fs::read_dir(&self.root) {
            Ok(entries) => {
                for (count, entry) in entries.enumerate() {
                    if count > 1000 {
                        return Err(BackupError::LimitReached);
                    }
                    let entry = entry.map_err(|_| BackupError::Storage)?;
                    let id = entry.file_name().to_string_lossy().into_owned();
                    if valid_id(&id).is_ok()
                        && read(&entry.path().join("manifest.json"), MAX_META, true)?.is_some()
                    {
                        snapshots.push(self.load(&id)?);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(BackupError::Storage),
        }
        snapshots.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(BackupState {
            snapshots,
            pending_recovery: read(&self.journal_path(), MAX_META, true)?.is_some(),
        })
    }
    pub fn list(&self) -> Result<BackupState> {
        let _g = engine::lock_app(self.tool.id());
        self.list_locked()
    }
    pub fn create(&self) -> Result<Snapshot> {
        let _g = engine::lock_app(self.tool.id());
        self.ready()?;
        let bytes = self.capture()?;
        self.check(&self.infos(&bytes))?;
        self.save(&bytes, false)
    }
    /// Native pre-write boundary only. Caller holds the tool switch lock; this
    /// takes only the engine lock and releases it before the live writer starts.
    /// Never resolve credentials or expand the configured file allowlist here.
    pub(crate) fn before_write(&self) -> Result<Snapshot> {
        let _g = engine::lock_app(self.tool.id());
        self.settled(false)?;
        self.no_pending_restore()?;
        let bytes = self.capture()?;
        let files = self.infos(&bytes);
        let snapshots = self.list_locked()?.snapshots;
        // Reuse any intact identical snapshot, including at the capacity limit.
        // A manifest alone is not a usable backup: verify its private payload.
        if let Some(snapshot) = snapshots.iter().find(|s| s.files == files) {
            private_dir(&self.store.file("backups/osir-tool-config"))?;
            private_dir(&self.root)?;
            private_dir(&self.dir(&snapshot.id)?)?;
            self.payload(snapshot)?;
            self.check(&files)?;
            return Ok(snapshot.clone());
        }
        self.check(&files)?;
        let snapshot = self.save(&bytes, false)?;
        // External edits during backup publication must refuse the live write.
        self.check(&files)?;
        Ok(snapshot)
    }
    pub fn preview(&self, id: &str) -> Result<Preview> {
        let _g = engine::lock_app(self.tool.id());
        self.ready()?;
        let snapshot = self.load(id)?;
        self.payload(&snapshot)?;
        let baseline = self.infos(&self.capture()?);
        self.check(&baseline)?;
        let files = baseline
            .iter()
            .zip(&snapshot.files)
            .map(|(a, b)| FileDiff {
                path: a.path.clone(),
                current_hash: a.hash.clone(),
                snapshot_hash: b.hash.clone(),
                current_size: a.size_bytes,
                snapshot_size: b.size_bytes,
                changed: a.hash != b.hash,
            })
            .collect();
        let id = uuid::Uuid::new_v4().to_string();
        let snapshot_id = snapshot.id.clone();
        let mut cache = grants().lock().map_err(|_| BackupError::Storage)?;
        cache.retain(|_, g| g.expires > Instant::now());
        if cache.len() >= 32 {
            return Err(BackupError::LimitReached);
        }
        cache.insert(
            id.clone(),
            Grant {
                expires: Instant::now() + Duration::from_secs(600),
                root: self.root.clone(),
                snapshot,
                baseline,
            },
        );
        Ok(Preview {
            preview_id: id,
            snapshot_id,
            tool: self.tool,
            files,
        })
    }
    pub fn restore(&self, id: &str, confirmed: bool) -> Result<RestoreReport> {
        self.restore_inner(id, confirmed, &mut |_, _| Ok(()))
    }
    fn restore_inner(
        &self,
        id: &str,
        confirmed: bool,
        hook: &mut dyn FnMut(usize, &Self) -> Result<()>,
    ) -> Result<RestoreReport> {
        if !confirmed {
            return Err(BackupError::InvalidConfirmation);
        }
        valid_id(id)?;
        let _g = engine::lock_app(self.tool.id());
        self.ready()?;
        let grant = grants()
            .lock()
            .map_err(|_| BackupError::Storage)?
            .remove(id)
            .ok_or(BackupError::PreviewExpired)?;
        if grant.expires <= Instant::now()
            || grant.root != self.root
            || grant.snapshot.tool != self.tool
        {
            return Err(BackupError::PreviewExpired);
        }
        let source = self.load(&grant.snapshot.id)?;
        if source.files != grant.snapshot.files {
            return Err(BackupError::InvalidSnapshot);
        }
        let desired = self.payload(&source)?;
        self.check(&grant.baseline)?;
        if source.files == grant.baseline {
            return Ok(report(RestoreStatus::NoChanges, None, vec![]));
        }
        let before = self.capture()?;
        if self.infos(&before) != grant.baseline {
            return Err(BackupError::Changed);
        }
        let safety = self.save(&before, true)?;
        let journal = Journal {
            safety_id: safety.id.clone(),
            before: grant.baseline,
            after: source.files,
        };
        let mut staged = Vec::new();
        for i in 0..self.paths.len() {
            staged.push(Prepared::new(
                &self.paths[i],
                &journal.before[i],
                before[i].clone(),
                desired[i].clone(),
            )?);
        }
        self.check(&journal.before)?;
        json_write(&self.journal_path(), &journal)?;
        let mut expected = journal.before.clone();
        let applied: Result<()> = (|| {
            for (i, item) in staged.iter_mut().enumerate() {
                hook(i, self)?;
                self.check(&expected)?;
                safe_path(&self.store.first_write_backup_dir())?;
                engine::ensure_first_write_backup(
                    &self.store,
                    &self.paths[i],
                    before[i].as_deref(),
                )
                .map_err(|_| BackupError::Storage)?;
                self.check(&expected)?;
                if journal.before[i] != journal.after[i] {
                    item.publish()?;
                }
                expected[i] = journal.after[i].clone();
            }
            self.check(&journal.after)
        })();
        if applied.is_err() {
            return self.rollback(&journal);
        }
        let status = if remove(&self.journal_path()).is_ok() {
            RestoreStatus::Restored
        } else {
            RestoreStatus::RecoveryRequired
        };
        Ok(report(status, Some(safety.id), vec![]))
    }
    pub fn recover(&self) -> Result<RestoreReport> {
        let _g = engine::lock_app(self.tool.id());
        self.idle()?;
        let bytes =
            read(&self.journal_path(), MAX_META, true)?.ok_or(BackupError::InvalidSnapshot)?;
        let journal: Journal =
            serde_json::from_slice(&bytes).map_err(|_| BackupError::InvalidSnapshot)?;
        self.rollback(&journal)
    }
    fn rollback(&self, j: &Journal) -> Result<RestoreReport> {
        if !self.valid_infos(&j.before) || !self.valid_infos(&j.after) {
            return Err(BackupError::InvalidSnapshot);
        }
        let safety = self.load(&j.safety_id)?;
        if !safety.safety || safety.files != j.before {
            return Err(BackupError::InvalidSnapshot);
        }
        let bytes = self.payload(&safety)?;
        let mut conflicts = vec![];
        for i in (0..self.paths.len()).rev() {
            let result: Result<()> = (|| {
                let current = read(&self.paths[i], MAX_FILE, false)?;
                let meta = info(&self.paths[i], current.as_deref());
                if meta == j.before[i] {
                    return Ok(());
                }
                // Never roll back a new external edit to either old version.
                if meta != j.after[i] {
                    return Err(BackupError::Changed);
                }
                let mut item = Prepared::new(&self.paths[i], &meta, current, bytes[i].clone())?;
                if info(
                    &self.paths[i],
                    read(&self.paths[i], MAX_FILE, false)?.as_deref(),
                ) != meta
                {
                    return Err(BackupError::Changed);
                }
                item.publish()
            })();
            if result.is_err() {
                conflicts.push(self.paths[i].to_string_lossy().into_owned());
            }
        }
        if conflicts.is_empty()
            && (self.check(&j.before).is_err() || remove(&self.journal_path()).is_err())
        {
            conflicts = self
                .paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
        }
        Ok(report(
            if conflicts.is_empty() {
                RestoreStatus::RolledBack
            } else {
                RestoreStatus::RecoveryRequired
            },
            Some(safety.id),
            conflicts,
        ))
    }
}
fn report(
    status: RestoreStatus,
    safety_snapshot_id: Option<String>,
    conflicts: Vec<String>,
) -> RestoreReport {
    RestoreReport {
        status,
        safety_snapshot_id,
        conflicts,
    }
}
struct Prepared {
    path: PathBuf,
    staged: Option<StagedWrite>,
    delete: bool,
}
impl Prepared {
    fn new(
        path: &Path,
        expected: &FileInfo,
        current: Option<Vec<u8>>,
        desired: Option<Vec<u8>>,
    ) -> Result<Self> {
        safe_path(path)?;
        let patch = Guarded {
            expected_pre: expected.hash.clone(),
            then: desired.map_or(WholeFile::Delete, WholeFile::Write),
        };
        let plan = engine::plan_from(&LiveFile::private(path), &patch, current)
            .map_err(|_| BackupError::Changed)?;
        let staged = if plan.is_noop() {
            None
        } else {
            engine::stage(&plan).map_err(|_| BackupError::Storage)?
        };
        Ok(Self {
            path: path.into(),
            staged,
            delete: plan.planned.is_none() && !plan.is_noop(),
        })
    }
    fn publish(&mut self) -> Result<()> {
        safe_path(&self.path)?;
        if let Some(staged) = self.staged.take() {
            staged.commit().map_err(|_| BackupError::Storage)?;
        } else if self.delete {
            remove(&self.path)?;
        }
        Ok(())
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        if let Some(staged) = &self.staged {
            let _ = fs::remove_file(staged.tmp_path());
        }
    }
}
fn valid_id(id: &str) -> Result<()> {
    if uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id) {
        Ok(())
    } else {
        Err(BackupError::InvalidSnapshot)
    }
}
fn info(path: &Path, bytes: Option<&[u8]>) -> FileInfo {
    FileInfo {
        path: path.to_string_lossy().into_owned(),
        hash: engine::digest(bytes),
        size_bytes: bytes.map_or(0, |b| b.len() as u64),
    }
}
fn safe_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(BackupError::UnsafePath);
    }
    let mut cursor = PathBuf::new();
    for c in path.components() {
        if matches!(c, Component::ParentDir | Component::CurDir) {
            return Err(BackupError::UnsafePath);
        }
        cursor.push(c);
        match fs::symlink_metadata(&cursor) {
            #[cfg(windows)]
            Ok(m) if windows::is_reparse(&m) => return Err(BackupError::UnsafePath),
            Ok(m) if m.file_type().is_symlink() || (cursor != path && !m.is_dir()) => {
                return Err(BackupError::UnsafePath)
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(BackupError::Storage),
        }
    }
    Ok(())
}
fn read(path: &Path, limit: u64, private: bool) -> Result<Option<Vec<u8>>> {
    safe_path(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    windows::read_options(&mut options);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut f = match options.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(BackupError::Storage),
    };
    let meta = f.metadata().map_err(|_| BackupError::Storage)?;
    if !meta.is_file() {
        return Err(BackupError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1
            || meta.uid() != unsafe { libc::geteuid() }
            || (private && meta.mode() & 0o077 != 0)
        {
            return Err(BackupError::UnsafePath);
        }
    }
    #[cfg(windows)]
    windows::verify_file(&f, private)?;
    #[cfg(not(any(unix, windows)))]
    {
        let _ = private;
        return Err(BackupError::UnsafePath);
    }
    if meta.len() > limit {
        return Err(BackupError::TooLarge);
    }
    let mut b = vec![];
    (&mut f)
        .take(limit + 1)
        .read_to_end(&mut b)
        .map_err(|_| BackupError::Storage)?;
    if b.len() as u64 > limit {
        return Err(BackupError::TooLarge);
    }
    let after = f.metadata().map_err(|_| BackupError::Storage)?;
    if meta.len() != after.len() || meta.modified().ok() != after.modified().ok() {
        return Err(BackupError::Changed);
    }
    Ok(Some(b))
}
fn private_dir(path: &Path) -> Result<()> {
    safe_path(path)?;
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            private_dir(parent)?;
        } else if !fs::metadata(parent)
            .map_err(|_| BackupError::Storage)?
            .is_dir()
        {
            return Err(BackupError::UnsafePath);
        }
    }
    #[cfg(not(windows))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    #[cfg(windows)]
    let created = windows::create_private_directory(path);
    #[cfg(not(windows))]
    let created = builder.create(path);
    match created {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let metadata = fs::metadata(path).map_err(|_| BackupError::Storage)?;
                if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                    return Err(BackupError::UnsafePath);
                }
            }
        }
        Err(_) => return Err(BackupError::Storage),
    }
    #[cfg(windows)]
    windows::verify_path(path)?;
    Ok(())
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    safe_path(path)?;
    let staged = stage_write(path, bytes, Some(0o600), true).map_err(|_| BackupError::Storage)?;
    #[cfg(windows)]
    windows::verify_path(staged.tmp_path())?;
    staged.commit().map_err(|_| BackupError::Storage)?;
    #[cfg(windows)]
    windows::verify_path(path)?;
    Ok(())
}
fn json_write(path: &Path, value: &impl Serialize) -> Result<()> {
    private_write(
        path,
        &serde_json::to_vec(value).map_err(|_| BackupError::Storage)?,
    )
}
fn remove(path: &Path) -> Result<()> {
    safe_path(path)?;
    crate::config::delete_file(path).map_err(|_| BackupError::Storage)
}
#[cfg(test)]
#[path = "tool_backup_tests.rs"]
mod tests;
