//! Unwired transaction state machine with separate filesystem and signature modules.
//! No IPC or production installation coordinator is registered.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Prepared,
    OldMoved,
    NewInstalled,
    RollingBack,
    Completed,
    RolledBack,
    Manual,
    RetiredCompleted,
    RetiredRolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub schema: u32,
    pub id: String,
    pub root: PathBuf,
    pub install: PathBuf,
    pub staged: PathBuf,
    pub backup: PathBuf,
    pub previous_build: Option<u64>,
    pub target_build: u64,
    pub step: Step,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidJournal,
    Storage,
    Verification,
    ManualRequired,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Must verify signature, bundle identity, team, Gatekeeper and expected build.
/// There is deliberately no accepting default or production implementation.
pub trait Verifier {
    fn verify(&self, path: &Path, build: u64) -> Result<()>;
}

/// Host contract: hold one exclusive lease across the entire operation; resolve
/// every path without following symlinks outside the trusted root; reject unsafe
/// file types. persist must atomically replace and fsync journal + parent WITHOUT
/// a missing-primary window. rename/remove must sync affected directory entries.
/// An error is allowed after a mutation landed: recovery inspects actual layout.
/// No implementation of this trait can be reached from IPC in this slice.
pub trait Backend {
    fn check_owned(&self, root: &Path, paths: &[&Path]) -> Result<()>;
    fn exists(&self, path: &Path) -> Result<bool>;
    /// Reject stale receipts and competing non-retired transactions.
    fn check_current(&self, journal: &Journal) -> Result<()>;
    /// Atomically reserve an unused transaction ID; existing receipts must fail.
    fn create(&mut self, journal: &Journal) -> Result<()>;
    fn persist(&mut self, journal: &Journal) -> Result<()>;
    fn rename(&mut self, from: &Path, to: &Path) -> Result<()>;
    fn remove(&mut self, path: &Path) -> Result<()>;
}

fn clean_absolute(p: &Path) -> bool {
    p.is_absolute()
        && p.components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}
impl Journal {
    pub fn prepared(
        root: &Path,
        id: &str,
        previous_build: Option<u64>,
        target_build: u64,
    ) -> Result<Self> {
        let work = root.join("transactions").join(id);
        let j = Self {
            schema: 1,
            id: id.into(),
            root: root.into(),
            install: root.join("Codex.app"),
            staged: work.join("new.app"),
            backup: work.join("old.app"),
            previous_build,
            target_build,
            step: Step::Prepared,
        };
        j.validate(root)?;
        Ok(j)
    }
    pub fn prepared_for_target(
        root: &Path,
        target: &Path,
        id: &str,
        previous_build: Option<u64>,
        target_build: u64,
    ) -> Result<Self> {
        let mut journal = Self::prepared(root, id, previous_build, target_build)?;
        if target != root.join("Codex.app") {
            journal.schema = 2;
            journal.install = target.into();
        }
        journal.validate(root)?;
        Ok(journal)
    }
    /// trusted_root is supplied by the host, NEVER taken from the journal/IPC.
    pub fn validate(&self, trusted_root: &Path) -> Result<()> {
        let valid_id = !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let work = trusted_root.join("transactions").join(&self.id);
        if !matches!(self.schema, 1 | 2)
            || !valid_id
            || !clean_absolute(trusted_root)
            || trusted_root.parent().is_none()
            || self.root != trusted_root
            || !clean_absolute(&self.root)
            || (self.schema == 1 && self.install != trusted_root.join("Codex.app"))
            || (self.schema == 2
                && (!matches!(
                    self.install.file_name().and_then(|n| n.to_str()),
                    Some("Codex.app" | "ChatGPT.app")
                ) || self.install.starts_with(trusted_root)))
            || self.staged != work.join("new.app")
            || self.backup != work.join("old.app")
            || ![&self.install, &self.staged, &self.backup]
                .iter()
                .all(|p| clean_absolute(p))
            || self.target_build == 0
            || self.previous_build == Some(0)
        {
            return Err(Error::InvalidJournal);
        }
        Ok(())
    }
}

fn validate<B: Backend>(b: &B, root: &Path, j: &Journal) -> Result<()> {
    j.validate(root)?;
    b.check_owned(root, &[&j.install, &j.staged, &j.backup])
}
fn mark<B: Backend>(b: &mut B, j: &mut Journal, step: Step) -> Result<()> {
    let mut next = j.clone();
    next.step = step;
    b.persist(&next)?;
    *j = next;
    Ok(())
}
fn manual<B: Backend>(b: &mut B, j: &mut Journal) -> Result<()> {
    mark(b, j, Step::Manual)?;
    Err(Error::ManualRequired)
}
fn finish<B: Backend, V: Verifier>(b: &mut B, v: &V, j: &mut Journal) -> Result<()> {
    if v.verify(&j.install, j.target_build).is_err() {
        return rollback(b, v, j);
    }
    if let Some(build) = j.previous_build {
        if !b.exists(&j.backup)? || v.verify(&j.backup, build).is_err() {
            return manual(b, j);
        }
    } else if b.exists(&j.backup)? {
        return manual(b, j);
    }
    mark(b, j, Step::Completed) // retain verified saved restore point
}
fn terminal_check<B: Backend, V: Verifier>(b: &B, v: &V, j: &Journal) -> Result<()> {
    match j.step {
        Step::Completed => {
            if b.exists(&j.staged)? || !b.exists(&j.install)? {
                return Err(Error::Verification);
            }
            v.verify(&j.install, j.target_build)?;
            match j.previous_build {
                Some(build) => {
                    if !b.exists(&j.backup)? {
                        return Err(Error::Verification);
                    }
                    v.verify(&j.backup, build)?;
                }
                None => {
                    if b.exists(&j.backup)? {
                        return Err(Error::Verification);
                    }
                }
            }
        }
        Step::RolledBack => {
            if b.exists(&j.backup)? {
                return Err(Error::Verification);
            }
            match j.previous_build {
                Some(build) => {
                    if !b.exists(&j.install)? {
                        return Err(Error::Verification);
                    }
                    v.verify(&j.install, build)?;
                }
                None => {
                    if b.exists(&j.install)? {
                        return Err(Error::Verification);
                    }
                }
            }
        }
        _ => return Err(Error::InvalidJournal),
    }
    Ok(())
}
/// Retirement removes authority over the shared install, not backup/evidence.
pub fn retire_receipt<B: Backend, V: Verifier>(
    b: &mut B,
    v: &V,
    root: &Path,
    j: &mut Journal,
) -> Result<()> {
    validate(b, root, j)?;
    b.check_current(j)?;
    terminal_check(b, v, j)?;
    let next = match j.step {
        Step::Completed => Step::RetiredCompleted,
        Step::RolledBack => Step::RetiredRolledBack,
        _ => return Err(Error::InvalidJournal),
    };
    mark(b, j, next)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreExpectation {
    pub receipt_id: String,
    pub target_build: u64,
    pub previous_build: u64,
}
pub fn preflight_restore<B: Backend, V: Verifier>(
    b: &B,
    v: &V,
    root: &Path,
    j: &Journal,
) -> Result<RestoreExpectation> {
    validate(b, root, j)?;
    b.check_current(j)?;
    if !matches!(j.step, Step::Completed | Step::RetiredCompleted) {
        return Err(Error::InvalidJournal);
    }
    let previous_build = j.previous_build.ok_or(Error::InvalidJournal)?;
    let mut active = j.clone();
    active.step = Step::Completed;
    terminal_check(b, v, &active)?;
    Ok(RestoreExpectation {
        receipt_id: j.id.clone(),
        target_build: j.target_build,
        previous_build,
    })
}
/// Recheck consent/builds before durable rollback intent; explicit restore consumes
/// the backup. No automatic retry if the currently installed target has changed.
pub fn restore_point<B: Backend, V: Verifier>(
    b: &mut B,
    v: &V,
    root: &Path,
    j: &mut Journal,
    expected: &RestoreExpectation,
) -> Result<()> {
    if &preflight_restore(b, v, root, j)? != expected {
        return Err(Error::InvalidJournal);
    }
    rollback(b, v, j)
}
fn rollback<B: Backend, V: Verifier>(b: &mut B, v: &V, j: &mut Journal) -> Result<()> {
    mark(b, j, Step::RollingBack)?;
    match j.previous_build {
        Some(build) => {
            if b.exists(&j.backup)? {
                if v.verify(&j.backup, build).is_err() {
                    return manual(b, j);
                }
                if b.exists(&j.install)? {
                    b.remove(&j.install)?;
                }
                b.rename(&j.backup, &j.install)?;
            }
            if !b.exists(&j.install)? || v.verify(&j.install, build).is_err() {
                return manual(b, j);
            }
        }
        None => {
            if b.exists(&j.backup)? {
                return manual(b, j);
            }
            if b.exists(&j.install)? {
                b.remove(&j.install)?;
            }
        }
    }
    mark(b, j, Step::RolledBack)
}

/// Fresh and replacement installs both persist Prepared BEFORE first rename.
pub fn begin<B: Backend, V: Verifier>(
    b: &mut B,
    v: &V,
    root: &Path,
    j: &mut Journal,
) -> Result<()> {
    validate(b, root, j)?;
    if j.step != Step::Prepared
        || b.exists(&j.backup)?
        || !b.exists(&j.staged)?
        || b.exists(&j.install)? != j.previous_build.is_some()
    {
        return Err(Error::InvalidJournal);
    }
    v.verify(&j.staged, j.target_build)?;
    if let Some(build) = j.previous_build {
        v.verify(&j.install, build)?;
    }
    b.create(j)?;
    if j.previous_build.is_some() {
        b.rename(&j.install, &j.backup)?;
        mark(b, j, Step::OldMoved)?;
    }
    b.rename(&j.staged, &j.install)?;
    mark(b, j, Step::NewInstalled)?;
    finish(b, v, j)
}

/// Caller loads the durable journal (not a stale in-memory copy) under its lease.
pub fn recover<B: Backend, V: Verifier>(
    b: &mut B,
    v: &V,
    root: &Path,
    j: &mut Journal,
) -> Result<()> {
    validate(b, root, j)?;
    if matches!(j.step, Step::RetiredCompleted | Step::RetiredRolledBack) {
        // History never mutates or validates the shared current installation.
        if j.step == Step::RetiredCompleted {
            if let Some(build) = j.previous_build {
                if !b.exists(&j.backup)? {
                    return Err(Error::Verification);
                }
                v.verify(&j.backup, build)?;
            } else if b.exists(&j.backup)? {
                return Err(Error::Verification);
            }
        } else if b.exists(&j.backup)? {
            return Err(Error::Verification);
        }
        return Ok(());
    }
    b.check_current(j)?;
    if j.step == Step::Manual {
        return Err(Error::ManualRequired);
    }
    if j.step == Step::RollingBack {
        return rollback(b, v, j);
    }
    if matches!(j.step, Step::Completed | Step::RolledBack) {
        if terminal_check(b, v, j).is_err() {
            return manual(b, j);
        }
        return Ok(());
    }
    let (installed, staged, backup) = (
        b.exists(&j.install)?,
        b.exists(&j.staged)?,
        b.exists(&j.backup)?,
    );
    match (installed, staged, backup, j.previous_build.is_some()) {
        (true, true, false, true) if j.step == Step::Prepared => {
            if v.verify(&j.install, j.previous_build.unwrap()).is_err() {
                return manual(b, j);
            }
            mark(b, j, Step::RolledBack) // no destructive work landed
        }
        (false, true, false, false) if j.step == Step::Prepared => mark(b, j, Step::RolledBack),
        (false, true, true, true) if matches!(j.step, Step::Prepared | Step::OldMoved) => {
            if v.verify(&j.staged, j.target_build).is_err() {
                return rollback(b, v, j);
            }
            b.rename(&j.staged, &j.install)?;
            mark(b, j, Step::NewInstalled)?;
            finish(b, v, j)
        }
        (true, false, _, _) if backup == j.previous_build.is_some() => finish(b, v, j),
        (false, false, true, true) => rollback(b, v, j),
        _ => manual(b, j),
    }
}

/// Cleanup must supply ALL journals. Any unreadable/unknown/unsafe journal
/// disables cleanup entirely. Terminal receipts also protect surviving material
/// until a later host explicitly retires the receipt under its exclusive lease.
pub fn cleanup_allowed<B: Backend>(
    b: &B,
    root: &Path,
    candidate: &Path,
    journals: &[Journal],
    scan_complete: bool,
) -> Result<bool> {
    if !scan_complete
        || !clean_absolute(candidate)
        || candidate == root
        || !candidate.starts_with(root)
    {
        return Ok(false);
    }
    b.check_owned(root, &[candidate])?;
    for j in journals {
        validate(b, root, j)?;
        let work = j.staged.parent().ok_or(Error::InvalidJournal)?;
        for protected in [j.install.as_path(), work] {
            if candidate.starts_with(protected) || protected.starts_with(candidate) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "fs_backend.rs"]
pub mod fs_backend;
#[path = "signature_verifier.rs"]
pub mod signature_verifier;

#[cfg(target_os = "macos")]
#[path = "release.rs"]
pub mod release;

#[cfg(target_os = "macos")]
#[path = "download.rs"]
pub mod download;

#[cfg(target_os = "macos")]
#[path = "coordinator.rs"]
pub mod coordinator;
