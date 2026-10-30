//! Unix filesystem implementation. Root must already exist, be private (0700),
//! owned by this uid, and be supplied by trusted native configuration.
//! Holds a nonblocking flock for its lifetime. No automatic application discovery.
use super::{Backend, Error, Journal, Result, Step};
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

const MAX_JOURNAL: u64 = 32 * 1024;
const MAX_ENTRIES: usize = 4096;
fn name(s: &OsStr) -> Result<CString> {
    CString::new(s.as_bytes()).map_err(|_| Error::InvalidJournal)
}
fn io<T>(r: std::io::Result<T>) -> Result<T> {
    r.map_err(|_| Error::Storage)
}
fn open_at(parent: &File, n: &OsStr, flags: i32, mode: libc::mode_t) -> Result<File> {
    let n = name(n)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            n.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(Error::Storage);
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(parent: &File, n: &OsStr) -> Result<File> {
    open_at(parent, n, libc::O_RDONLY | libc::O_DIRECTORY, 0)
}
fn private(f: &File, dir: bool) -> Result<()> {
    let m = io(f.metadata())?;
    if m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || (dir && !m.is_dir())
        || (!dir && (!m.is_file() || m.nlink() != 1))
    {
        return Err(Error::InvalidJournal);
    }
    Ok(())
}
fn mkdir(parent: &File, n: &OsStr) -> Result<File> {
    let c = name(n)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) } != 0
        && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
    {
        return Err(Error::Storage);
    }
    let f = directory(parent, n)?;
    private(&f, true)?;
    io(parent.sync_all())?;
    Ok(f)
}
fn stat(parent: &File, n: &OsStr) -> Result<Option<libc::stat>> {
    let n = name(n)?;
    let mut s = std::mem::MaybeUninit::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            n.as_ptr(),
            s.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Ok(Some(unsafe { s.assume_init() }));
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(Error::Storage)
    }
}
fn entries(dir: &File) -> Result<Vec<std::ffi::OsString>> {
    // Open a fresh description: dup would share the directory cursor.
    let fresh = directory(dir, OsStr::new("."))?;
    let fd = unsafe { libc::dup(fresh.as_raw_fd()) };
    if fd < 0 {
        return Err(Error::Storage);
    }
    let p = unsafe { libc::fdopendir(fd) };
    if p.is_null() {
        unsafe { libc::close(fd) };
        return Err(Error::Storage);
    }
    struct Dir(*mut libc::DIR);
    impl Drop for Dir {
        fn drop(&mut self) {
            unsafe { libc::closedir(self.0) };
        }
    }
    let _guard = Dir(p);
    let mut out = Vec::new();
    loop {
        // readdir reports EOF and failure identically except errno.
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let e = unsafe { libc::readdir(p) };
        if e.is_null() {
            if std::io::Error::last_os_error().raw_os_error().unwrap_or(0) != 0 {
                return Err(Error::Storage);
            }
            break;
        }
        let bytes = unsafe { std::ffi::CStr::from_ptr((*e).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        out.push(OsStr::from_bytes(bytes).to_os_string());
        if out.len() > MAX_ENTRIES {
            return Err(Error::Storage);
        }
    }
    Ok(out)
}

pub struct FsBackend {
    root: PathBuf,
    target: PathBuf,
    target_parent: Option<File>,
    dir: File,
    journals: File,
    _lease: File,
}
impl FsBackend {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_for_target(root, &root.join("Codex.app"))
    }
    /// `target` is revalidated by the native coordinator; receipts cannot select
    /// it. The extra descriptor authorizes exactly this bundle, no siblings.
    pub fn open_for_target(root: &Path, target: &Path) -> Result<Self> {
        if !super::clean_absolute(root) || root.parent().is_none() {
            return Err(Error::InvalidJournal);
        }
        let mut dir = io(File::open("/"))?;
        for c in root.components() {
            if let Component::Normal(n) = c {
                dir = directory(&dir, n)?;
            }
        }
        private(&dir, true)?;
        let lease = open_at(
            &dir,
            OsStr::new(".installer.lock"),
            libc::O_RDWR | libc::O_CREAT,
            0o600,
        )?;
        private(&lease, false)?;
        if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::Storage);
        }
        let journals = mkdir(&dir, OsStr::new("journals"))?;
        mkdir(&dir, OsStr::new("transactions"))?;
        let target_parent = if target != root.join("Codex.app") {
            if !super::clean_absolute(target) || target.starts_with(root) {
                return Err(Error::InvalidJournal);
            }
            let parent = target.parent().ok_or(Error::InvalidJournal)?;
            let mut fd = io(File::open("/"))?;
            for component in parent.components() {
                if let Component::Normal(name) = component {
                    fd = directory(&fd, name)?;
                }
            }
            if io(fd.metadata())?.dev() != io(dir.metadata())?.dev() {
                return Err(Error::InvalidJournal);
            }
            Some(fd)
        } else {
            None
        };
        Ok(Self {
            root: root.into(),
            target: target.into(),
            target_parent,
            dir,
            journals,
            _lease: lease,
        })
    }
    pub fn prepare(&self, id: &str, previous: Option<u64>, target: u64) -> Result<Journal> {
        let j = Journal::prepared_for_target(&self.root, &self.target, id, previous, target)?;
        if self.scan()?.iter().any(|receipt| {
            !matches!(
                receipt.step,
                Step::RetiredCompleted | Step::RetiredRolledBack
            )
        }) {
            return Err(Error::ManualRequired);
        }
        if stat(&self.journals, OsStr::new(&format!("{id}.json")))?.is_some() {
            return Err(Error::InvalidJournal);
        }
        let tx = directory(&self.dir, OsStr::new("transactions"))?;
        // Existing staging may contain recovery material: never adopt it.
        if stat(&tx, OsStr::new(id))?.is_some() {
            return Err(Error::InvalidJournal);
        }
        mkdir(&tx, OsStr::new(id))?;
        Ok(j)
    }
    fn parent(&self, p: &Path) -> Result<(File, std::ffi::OsString)> {
        if !super::clean_absolute(p) {
            return Err(Error::InvalidJournal);
        }
        if p == self.target {
            if let Some(parent) = &self.target_parent {
                return Ok((
                    io(parent.try_clone())?,
                    p.file_name().ok_or(Error::InvalidJournal)?.into(),
                ));
            }
        }
        let rel = p
            .strip_prefix(&self.root)
            .map_err(|_| Error::InvalidJournal)?;
        let mut components = rel.components().collect::<Vec<_>>();
        let leaf = match components.pop() {
            Some(Component::Normal(n)) => n.to_os_string(),
            _ => return Err(Error::InvalidJournal),
        };
        let mut dir = io(self.dir.try_clone())?;
        for c in components {
            match c {
                Component::Normal(n) => {
                    dir = directory(&dir, n)?;
                    private(&dir, true)?;
                }
                _ => return Err(Error::InvalidJournal),
            }
        }
        Ok((dir, leaf))
    }
    fn check(&self, p: &Path) -> Result<()> {
        let (d, n) = self.parent(p)?;
        if let Some(s) = stat(&d, &n)? {
            if s.st_mode & libc::S_IFMT != libc::S_IFDIR
                || (s.st_uid != unsafe { libc::geteuid() }
                    && !(self.target_parent.is_some() && s.st_uid == 0))
                || s.st_dev as u64 != io(self.dir.metadata())?.dev()
            {
                return Err(Error::InvalidJournal);
            }
        }
        Ok(())
    }
    pub fn load(&self, id: &str) -> Result<Journal> {
        Journal::prepared(&self.root, id, None, 1)?;
        let f = open_at(
            &self.journals,
            OsStr::new(&format!("{id}.json")),
            libc::O_RDONLY,
            0,
        )?;
        private(&f, false)?;
        if io(f.metadata())?.len() > MAX_JOURNAL {
            return Err(Error::InvalidJournal);
        }
        let mut bytes = Vec::new();
        io(f.take(MAX_JOURNAL + 1).read_to_end(&mut bytes))?;
        if bytes.len() as u64 > MAX_JOURNAL {
            return Err(Error::InvalidJournal);
        }
        let j: Journal = serde_json::from_slice(&bytes).map_err(|_| Error::InvalidJournal)?;
        j.validate(&self.root)?;
        if j.id != id || j.install != self.target {
            return Err(Error::InvalidJournal);
        }
        Ok(j)
    }
    pub fn scan(&self) -> Result<Vec<Journal>> {
        entries(&self.journals)?
            .into_iter()
            .map(|n| {
                let id = n
                    .to_str()
                    .and_then(|s| s.strip_suffix(".json"))
                    .ok_or(Error::InvalidJournal)?;
                self.load(id)
            })
            .collect()
    }
    fn write(&self, j: &Journal, create: bool) -> Result<()> {
        j.validate(&self.root)?;
        if j.install != self.target {
            return Err(Error::InvalidJournal);
        }
        let dest = format!("{}.json", j.id);
        let tmp = format!("{}.pending", j.id);
        let bytes = serde_json::to_vec(j).map_err(|_| Error::Storage)?;
        if bytes.len() as u64 > MAX_JOURNAL {
            return Err(Error::InvalidJournal);
        }
        // Stale pending files fail closed. scan reports them, blocking cleanup.
        let mut f = open_at(
            &self.journals,
            OsStr::new(&tmp),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        io(f.write_all(&bytes))?;
        io(f.sync_all())?;
        let t = name(OsStr::new(&tmp))?;
        let d = name(OsStr::new(&dest))?;
        if create {
            if unsafe {
                libc::linkat(
                    self.journals.as_raw_fd(),
                    t.as_ptr(),
                    self.journals.as_raw_fd(),
                    d.as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(Error::Storage);
            }
            if unsafe { libc::unlinkat(self.journals.as_raw_fd(), t.as_ptr(), 0) } != 0 {
                return Err(Error::Storage);
            }
        } else if unsafe {
            libc::renameat(
                self.journals.as_raw_fd(),
                t.as_ptr(),
                self.journals.as_raw_fd(),
                d.as_ptr(),
            )
        } != 0
        {
            return Err(Error::Storage);
        }
        io(self.journals.sync_all())
    }
}
impl Backend for FsBackend {
    fn check_owned(&self, root: &Path, paths: &[&Path]) -> Result<()> {
        if root != self.root {
            return Err(Error::InvalidJournal);
        }
        // Detect replacement of the path used by external verification tools.
        let mut current = io(File::open("/"))?;
        for c in root.components() {
            if let Component::Normal(n) = c {
                current = directory(&current, n)?;
            }
        }
        let a = io(current.metadata())?;
        let b = io(self.dir.metadata())?;
        if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
            return Err(Error::InvalidJournal);
        }
        if let Some(parent) = &self.target_parent {
            let mut current = io(File::open("/"))?;
            for component in self
                .target
                .parent()
                .ok_or(Error::InvalidJournal)?
                .components()
            {
                if let Component::Normal(name) = component {
                    current = directory(&current, name)?;
                }
            }
            let a = io(current.metadata())?;
            let b = io(parent.metadata())?;
            if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                return Err(Error::InvalidJournal);
            }
        }
        for p in paths {
            self.check(p)?;
        }
        Ok(())
    }
    fn exists(&self, p: &Path) -> Result<bool> {
        self.check(p)?;
        let (d, n) = self.parent(p)?;
        Ok(stat(&d, &n)?.is_some())
    }
    fn check_current(&self, j: &Journal) -> Result<()> {
        if self.load(&j.id)? != *j
            || self.scan()?.iter().any(|other| {
                other.id != j.id
                    && !matches!(other.step, Step::RetiredCompleted | Step::RetiredRolledBack)
            })
        {
            return Err(Error::InvalidJournal);
        }
        Ok(())
    }
    fn create(&mut self, j: &Journal) -> Result<()> {
        self.check_owned(&self.root, &[&j.install, &j.staged, &j.backup])?;
        if j.step != Step::Prepared
            || self.scan()?.iter().any(|receipt| {
                !matches!(
                    receipt.step,
                    Step::RetiredCompleted | Step::RetiredRolledBack
                )
            })
        {
            return Err(Error::InvalidJournal);
        }
        self.write(j, true)
    }
    fn persist(&mut self, j: &Journal) -> Result<()> {
        let old = self.load(&j.id)?;
        self.check_current(&old)?;
        if matches!(old.step, Step::RetiredCompleted | Step::RetiredRolledBack)
            && !(old.step == Step::RetiredCompleted && j.step == Step::RollingBack)
        {
            return Err(Error::InvalidJournal);
        }
        let mut expected = old;
        expected.step = j.step;
        if expected != *j {
            return Err(Error::InvalidJournal);
        }
        self.write(j, false)
    }
    fn rename(&mut self, a: &Path, z: &Path) -> Result<()> {
        self.check_owned(&self.root, &[a, z])?;
        let receipts = self.scan()?;
        if !receipts.iter().any(|j| {
            (a == j.install && z == j.backup && j.step == Step::Prepared)
                || (a == j.staged
                    && z == j.install
                    && matches!(j.step, Step::Prepared | Step::OldMoved))
                || (a == j.backup && z == j.install && j.step == Step::RollingBack)
        }) {
            return Err(Error::InvalidJournal);
        }
        if self.exists(z)? {
            return Err(Error::Storage);
        }
        // Flush the complete candidate/backup tree before making its new name durable.
        let (parent, leaf) = self.parent(a)?;
        let mut budget = 200_000;
        sync_tree(&parent, &leaf, 0, &mut budget)?;
        let (ad, an) = self.parent(a)?;
        let (zd, zn) = self.parent(z)?;
        let an = name(&an)?;
        let zn = name(&zn)?;
        // Linux/macOS exclusive rename prevents accidental target replacement.
        #[cfg(target_os = "macos")]
        let rc = unsafe {
            libc::renameatx_np(
                ad.as_raw_fd(),
                an.as_ptr(),
                zd.as_raw_fd(),
                zn.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::renameat2(
                ad.as_raw_fd(),
                an.as_ptr(),
                zd.as_raw_fd(),
                zn.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc != 0 {
            return Err(Error::Storage);
        }
        io(ad.sync_all())?;
        io(zd.sync_all())
    }
    fn remove(&mut self, p: &Path) -> Result<()> {
        self.check_owned(&self.root, &[p])?;
        // Only the state machine's backup/installed bundle deletion is allowed.
        let receipts = self.scan()?;
        if !receipts
            .iter()
            .any(|j| p == j.install && j.step == Step::RollingBack)
        {
            return Err(Error::InvalidJournal);
        }
        let (d, n) = self.parent(p)?;
        let mut budget = 200_000;
        remove_tree(&d, &n, 0, &mut budget)?;
        io(d.sync_all())
    }
}
fn remove_tree(parent: &File, n: &OsStr, depth: usize, budget: &mut usize) -> Result<()> {
    if depth > 128 || *budget == 0 {
        return Err(Error::Storage);
    }
    *budget -= 1;
    let s = stat(parent, n)?.ok_or(Error::Storage)?;
    let directory_type = s.st_mode & libc::S_IFMT == libc::S_IFDIR;
    if directory_type {
        let d = directory(parent, n)?;
        if io(d.metadata())?.dev() != io(parent.metadata())?.dev() {
            return Err(Error::InvalidJournal);
        }
        for child in entries(&d)? {
            remove_tree(&d, &child, depth + 1, budget)?;
        }
        io(d.sync_all())?;
    }
    // Symlinks within authentic bundles are unlinked, never traversed.
    let n = name(n)?;
    if unsafe {
        libc::unlinkat(
            parent.as_raw_fd(),
            n.as_ptr(),
            if directory_type {
                libc::AT_REMOVEDIR
            } else {
                0
            },
        )
    } != 0
    {
        return Err(Error::Storage);
    }
    io(parent.sync_all())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
    struct AcceptFixture;
    impl super::super::Verifier for AcceptFixture {
        fn verify(&self, _: &Path, _: u64) -> Result<()> {
            Ok(())
        }
    }
    fn root() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::set_permissions(t.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        t
    }
    #[test]
    fn fresh_install_persists_and_loads_receipt() {
        let t = root();
        let p = t.path().canonicalize().unwrap();
        let mut b = FsBackend::open(&p).unwrap();
        let mut j = b.prepare("first", None, 42).unwrap();
        std::fs::create_dir(&j.staged).unwrap();
        super::super::begin(&mut b, &AcceptFixture, &p, &mut j).unwrap();
        assert_eq!(b.load("first").unwrap().step, Step::Completed);
        assert!(j.install.is_dir());
        assert!(b.prepare("first", None, 42).is_err());
    }
    #[test]
    fn external_target_update_and_reopen_restore_leave_sibling_unchanged() {
        let t = root();
        let p = t.path().canonicalize().unwrap();
        let storage = p.join("private-transactions");
        let applications = p.join("Applications");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&storage)
            .unwrap();
        std::fs::create_dir(&applications).unwrap();
        let target = applications.join("ChatGPT.app");
        let sibling = applications.join("Codex.app");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        std::fs::write(target.join("payload"), "old").unwrap();
        std::fs::write(sibling.join("payload"), "other").unwrap();
        let mut backend = FsBackend::open_for_target(&storage, &target).unwrap();
        let mut journal = backend.prepare("selected-target", Some(1), 2).unwrap();
        assert_eq!(journal.schema, 2);
        assert_eq!(journal.install, target);
        std::fs::create_dir(&journal.staged).unwrap();
        std::fs::write(journal.staged.join("payload"), "new").unwrap();
        super::super::begin(&mut backend, &AcceptFixture, &storage, &mut journal).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("payload")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(journal.backup.join("payload")).unwrap(),
            "old"
        );
        drop(backend);
        let mut backend = FsBackend::open_for_target(&storage, &target).unwrap();
        let mut journal = backend.load("selected-target").unwrap();
        let point =
            super::super::preflight_restore(&backend, &AcceptFixture, &storage, &journal).unwrap();
        super::super::restore_point(&mut backend, &AcceptFixture, &storage, &mut journal, &point)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("payload")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(sibling.join("payload")).unwrap(),
            "other"
        );
        assert!(!journal.backup.exists());
    }
    #[test]
    fn external_target_receipt_cannot_redirect_to_a_sibling_or_symlink_parent() {
        let t = root();
        let p = t.path().canonicalize().unwrap();
        let storage = p.join("private-transactions");
        let applications = p.join("Applications");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&storage)
            .unwrap();
        std::fs::create_dir(&applications).unwrap();
        let target = applications.join("Codex.app");
        let mut backend = FsBackend::open_for_target(&storage, &target).unwrap();
        let mut journal = backend.prepare("bound-target", None, 2).unwrap();
        journal.install = applications.join("ChatGPT.app");
        assert_eq!(backend.create(&journal), Err(Error::InvalidJournal));
        assert!(backend.exists(&journal.install).is_err());
        drop(backend);
        let alias = p.join("alias");
        symlink(&applications, &alias).unwrap();
        assert!(FsBackend::open_for_target(&storage, &alias.join("Codex.app")).is_err());
    }
    #[test]
    fn lock_is_exclusive_and_released() {
        let t = root();
        let p = t.path().canonicalize().unwrap();
        let b = FsBackend::open(&p).unwrap();
        assert!(FsBackend::open(&p).is_err());
        drop(b);
        assert!(FsBackend::open(&p).is_ok());
    }
    #[test]
    fn reject_permissions_and_symlink_root() {
        let t = root();
        let p = t.path().canonicalize().unwrap();
        let link = p.join("alias");
        symlink(&p, &link).unwrap();
        assert!(FsBackend::open(&link).is_err());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(FsBackend::open(&p).is_err());
    }
    #[test]
    fn symlink_bundle_never_followed() {
        let t = root();
        let p = t.path().canonicalize().unwrap();
        let b = FsBackend::open(&p).unwrap();
        let j = b.prepare("link", None, 42).unwrap();
        symlink("/", &j.staged).unwrap();
        assert!(b.check_owned(&p, &[&j.staged]).is_err());
    }
    #[test]
    fn bounded_corrupt_and_unknown_journals_block_scan() {
        let t = root();
        let p = t.path().canonicalize().unwrap();
        let b = FsBackend::open(&p).unwrap();
        let f = p.join("journals/bad.json");
        std::fs::write(&f, vec![0; MAX_JOURNAL as usize + 1]).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(b.scan().is_err());
        std::fs::remove_file(&f).unwrap();
        std::fs::write(p.join("journals/interrupted.pending"), b"{}").unwrap();
        assert!(b.scan().is_err());
    }
    #[test]
    fn pending_backup_cannot_be_deleted() {
        let t = root();
        let p = t.path().canonicalize().unwrap();
        let mut b = FsBackend::open(&p).unwrap();
        let j = b.prepare("pending", Some(1), 2).unwrap();
        std::fs::create_dir(&j.backup).unwrap();
        b.create(&j).unwrap();
        assert!(b.remove(&j.backup).is_err());
        assert!(j.backup.exists());
    }
}

fn sync_tree(parent: &File, n: &OsStr, depth: usize, budget: &mut usize) -> Result<()> {
    if depth > 128 || *budget == 0 {
        return Err(Error::Storage);
    }
    *budget -= 1;
    let s = stat(parent, n)?.ok_or(Error::Storage)?;
    match s.st_mode & libc::S_IFMT {
        libc::S_IFDIR => {
            let d = directory(parent, n)?;
            if io(d.metadata())?.dev() != io(parent.metadata())?.dev() {
                return Err(Error::InvalidJournal);
            }
            for child in entries(&d)? {
                sync_tree(&d, &child, depth + 1, budget)?;
            }
            io(d.sync_all())
        }
        libc::S_IFREG => {
            let f = open_at(parent, n, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
            let m = io(f.metadata())?;
            if !m.is_file() || m.nlink() != 1 || m.uid() != unsafe { libc::geteuid() } {
                return Err(Error::InvalidJournal);
            }
            io(f.sync_all())
        }
        libc::S_IFLNK => io(parent.sync_all()),
        _ => Err(Error::InvalidJournal),
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture;
    impl super::super::Verifier for Fixture {
        fn verify(&self, p: &Path, _: u64) -> Result<()> {
            if p.is_dir() {
                Ok(())
            } else {
                Err(Error::Verification)
            }
        }
    }
    #[test]
    fn reopen_recovers_old_moved_before_journal_mark() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut backend = FsBackend::open(&root).unwrap();
        let j = backend.prepare("crash", Some(1), 2).unwrap();
        std::fs::create_dir(&j.install).unwrap();
        std::fs::create_dir(&j.staged).unwrap();
        backend.create(&j).unwrap();
        backend.rename(&j.install, &j.backup).unwrap();
        drop(backend);
        let mut backend = FsBackend::open(&root).unwrap();
        let mut saved = backend.load("crash").unwrap();
        assert_eq!(saved.step, Step::Prepared);
        super::super::recover(&mut backend, &Fixture, &root, &mut saved).unwrap();
        assert_eq!(saved.step, Step::Completed);
        assert!(j.install.is_dir());
        assert!(j.backup.exists());
    }
}

#[cfg(test)]
mod saved_restore_tests {
    use super::super::{
        begin, preflight_restore, recover, restore_point, retire_receipt, Verifier,
    };
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct BuildVerifier;
    impl Verifier for BuildVerifier {
        fn verify(&self, p: &Path, build: u64) -> Result<()> {
            if std::fs::read_to_string(p.join("build")).ok().as_deref()
                == Some(build.to_string().as_str())
            {
                Ok(())
            } else {
                Err(Error::Verification)
            }
        }
    }
    fn bundle(p: &Path, build: u64) {
        std::fs::create_dir(p).unwrap();
        std::fs::write(p.join("build"), build.to_string()).unwrap();
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, FsBackend) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let b = FsBackend::open(&root).unwrap();
        (t, root, b)
    }
    #[test]
    fn two_upgrades_retire_history_and_isolate_new_pending() {
        let (_t, root, mut b) = fixture();
        bundle(&root.join("Codex.app"), 1);
        let mut first = b.prepare("first", Some(1), 2).unwrap();
        bundle(&first.staged, 2);
        begin(&mut b, &BuildVerifier, &root, &mut first).unwrap();
        assert!(first.backup.is_dir());
        assert!(b.prepare("second", Some(2), 3).is_err());
        let stale = first.clone();
        retire_receipt(&mut b, &BuildVerifier, &root, &mut first).unwrap();
        let mut second = b.prepare("second", Some(2), 3).unwrap();
        bundle(&second.staged, 3);
        b.create(&second).unwrap();
        b.rename(&second.install, &second.backup).unwrap();
        drop(b);
        let mut b = FsBackend::open(&root).unwrap();
        second = b.load("second").unwrap();
        // The old retired receipt must neither inspect the missing install nor
        // change authority while a newer journal owns the recovery window.
        recover(&mut b, &BuildVerifier, &root, &mut first).unwrap();
        let mut stale = stale;
        assert!(recover(&mut b, &BuildVerifier, &root, &mut stale).is_err());
        assert!(preflight_restore(&b, &BuildVerifier, &root, &first).is_err());
        recover(&mut b, &BuildVerifier, &root, &mut second).unwrap();
        assert!(first.backup.exists());
        assert!(second.backup.exists());
        BuildVerifier.verify(&second.install, 3).unwrap();
        assert!(b.remove(&first.backup).is_err());
        assert!(b.remove(&second.backup).is_err());
        retire_receipt(&mut b, &BuildVerifier, &root, &mut second).unwrap();
        // Even with all receipts retired, old restore cannot replace build 3
        // using consent that was bound to build 2.
        assert!(preflight_restore(&b, &BuildVerifier, &root, &first).is_err());
        let expected = preflight_restore(&b, &BuildVerifier, &root, &second).unwrap();
        restore_point(&mut b, &BuildVerifier, &root, &mut second, &expected).unwrap();
        BuildVerifier.verify(&second.install, 2).unwrap();
        assert_eq!(second.step, Step::RolledBack);
        assert!(first.backup.exists());
        assert!(!second.backup.exists());
    }
    #[test]
    fn explicit_restore_rechecks_consent_and_current_build() {
        let (_t, root, mut b) = fixture();
        bundle(&root.join("Codex.app"), 1);
        let mut j = b.prepare("restore", Some(1), 2).unwrap();
        bundle(&j.staged, 2);
        begin(&mut b, &BuildVerifier, &root, &mut j).unwrap();
        let mut expected = preflight_restore(&b, &BuildVerifier, &root, &j).unwrap();
        expected.previous_build = 999;
        assert!(restore_point(&mut b, &BuildVerifier, &root, &mut j, &expected).is_err());
        expected.previous_build = 1;
        std::fs::write(j.install.join("build"), "3").unwrap();
        assert!(restore_point(&mut b, &BuildVerifier, &root, &mut j, &expected).is_err());
        assert_eq!(b.load("restore").unwrap().step, Step::Completed);
        assert!(j.backup.exists());
    }
    #[test]
    fn completed_rechecks_backup_and_rolled_back_rechecks_install() {
        let (_t, root, mut b) = fixture();
        bundle(&root.join("Codex.app"), 1);
        let mut j = b.prepare("terminal", Some(1), 2).unwrap();
        bundle(&j.staged, 2);
        begin(&mut b, &BuildVerifier, &root, &mut j).unwrap();
        let expected = preflight_restore(&b, &BuildVerifier, &root, &j).unwrap();
        restore_point(&mut b, &BuildVerifier, &root, &mut j, &expected).unwrap();
        recover(&mut b, &BuildVerifier, &root, &mut j).unwrap();
        std::fs::write(j.install.join("build"), "7").unwrap();
        assert_eq!(
            recover(&mut b, &BuildVerifier, &root, &mut j),
            Err(Error::ManualRequired)
        );
        assert_eq!(j.step, Step::Manual);
    }
    #[test]
    fn completed_missing_backup_becomes_manual_without_deleting_install() {
        let (_t, root, mut b) = fixture();
        bundle(&root.join("Codex.app"), 1);
        let mut j = b.prepare("missing", Some(1), 2).unwrap();
        bundle(&j.staged, 2);
        begin(&mut b, &BuildVerifier, &root, &mut j).unwrap();
        std::fs::write(j.backup.join("build"), "9").unwrap();
        assert_eq!(
            recover(&mut b, &BuildVerifier, &root, &mut j),
            Err(Error::ManualRequired)
        );
        assert!(j.install.exists());
        assert!(j.backup.exists());
    }
    #[test]
    fn fresh_rolled_back_requires_absence() {
        let (_t, root, mut b) = fixture();
        let mut j = b.prepare("fresh", None, 1).unwrap();
        bundle(&j.staged, 1);
        b.create(&j).unwrap();
        recover(&mut b, &BuildVerifier, &root, &mut j).unwrap();
        assert_eq!(j.step, Step::RolledBack);
        bundle(&j.install, 1);
        assert_eq!(
            recover(&mut b, &BuildVerifier, &root, &mut j),
            Err(Error::ManualRequired)
        );
    }
}
