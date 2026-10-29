use super::*;
use std::cell::RefCell;
use std::collections::BTreeSet;

#[derive(Default)]
struct Memory {
    files: BTreeSet<PathBuf>,
    durable: Option<Journal>,
    renames: usize,
    fail_after_rename: Option<usize>,
    fail_persist: bool,
    unsafe_paths: bool,
}
impl Backend for Memory {
    fn check_owned(&self, _: &Path, _: &[&Path]) -> Result<()> {
        if self.unsafe_paths {
            Err(Error::InvalidJournal)
        } else {
            Ok(())
        }
    }
    fn exists(&self, p: &Path) -> Result<bool> {
        Ok(self.files.contains(p))
    }
    fn check_current(&self, j: &Journal) -> Result<()> {
        if self.durable.as_ref() == Some(j) {
            Ok(())
        } else {
            Err(Error::InvalidJournal)
        }
    }
    fn create(&mut self, j: &Journal) -> Result<()> {
        if self.durable.is_some() {
            return Err(Error::Storage);
        }
        self.persist(j)
    }
    fn persist(&mut self, j: &Journal) -> Result<()> {
        if self.fail_persist {
            return Err(Error::Storage);
        }
        self.durable = Some(j.clone());
        Ok(())
    }
    fn rename(&mut self, a: &Path, z: &Path) -> Result<()> {
        assert!(self.durable.is_some(), "rename before journal");
        if self.files.contains(z) || !self.files.remove(a) {
            return Err(Error::Storage);
        }
        self.files.insert(z.into());
        self.renames += 1;
        if self.fail_after_rename == Some(self.renames) {
            return Err(Error::Storage);
        }
        Ok(())
    }
    fn remove(&mut self, p: &Path) -> Result<()> {
        self.files.remove(p);
        Ok(())
    }
}
#[derive(Default)]
struct Verify {
    denied: BTreeSet<PathBuf>,
    seen: RefCell<Vec<(PathBuf, u64)>>,
}
impl Verifier for Verify {
    fn verify(&self, p: &Path, build: u64) -> Result<()> {
        self.seen.borrow_mut().push((p.into(), build));
        if self.denied.contains(p) {
            Err(Error::Verification)
        } else {
            Ok(())
        }
    }
}
fn fixture(previous: Option<u64>) -> (Memory, Verify, Journal) {
    let j = Journal::prepared(Path::new("/fixture-only"), "case-1", previous, 42).unwrap();
    let mut b = Memory::default();
    b.files.insert(j.staged.clone());
    if previous.is_some() {
        b.files.insert(j.install.clone());
    }
    (b, Verify::default(), j)
}
fn resume(b: &mut Memory, v: &Verify) -> Result<Journal> {
    let mut j = b.durable.clone().unwrap();
    let root = j.root.clone();
    recover(b, v, &root, &mut j)?;
    Ok(j)
}
#[test]
fn fresh_and_replace_finish_with_durable_receipt() {
    for previous in [None, Some(41)] {
        let (mut b, v, mut j) = fixture(previous);
        let root = j.root.clone();
        begin(&mut b, &v, &root, &mut j).unwrap();
        assert_eq!(j.step, Step::Completed);
        assert!(b.files.contains(&j.install));
        assert_eq!(b.files.contains(&j.backup), previous.is_some());
        assert!(v.seen.borrow().contains(&(j.install.clone(), 42)));
    }
}
#[test]
fn failed_journal_prevents_first_rename() {
    let (mut b, v, mut j) = fixture(Some(41));
    b.fail_persist = true;
    let root = j.root.clone();
    assert_eq!(begin(&mut b, &v, &root, &mut j), Err(Error::Storage));
    assert_eq!(b.renames, 0);
}
#[test]
fn recover_each_replacement_rename_window() {
    for boundary in [1, 2] {
        let (mut b, v, mut j) = fixture(Some(41));
        b.fail_after_rename = Some(boundary);
        let root = j.root.clone();
        assert_eq!(begin(&mut b, &v, &root, &mut j), Err(Error::Storage));
        b.fail_after_rename = None;
        assert_eq!(resume(&mut b, &v).unwrap().step, Step::Completed);
        assert!(v.seen.borrow().contains(&(j.install.clone(), 42)));
    }
}
#[test]
fn fresh_rename_crash_is_recovered() {
    let (mut b, v, mut j) = fixture(None);
    b.fail_after_rename = Some(1);
    let root = j.root.clone();
    assert!(begin(&mut b, &v, &root, &mut j).is_err());
    assert_eq!(b.durable.as_ref().unwrap().step, Step::Prepared);
    b.fail_after_rename = None;
    assert_eq!(resume(&mut b, &v).unwrap().step, Step::Completed);
}
#[test]
fn failed_candidate_reverification_restores_verified_backup() {
    let (mut b, mut v, mut j) = fixture(Some(41));
    b.fail_after_rename = Some(1);
    let root = j.root.clone();
    assert!(begin(&mut b, &v, &root, &mut j).is_err());
    b.fail_after_rename = None;
    v.denied.insert(j.staged.clone());
    assert_eq!(resume(&mut b, &v).unwrap().step, Step::RolledBack);
    assert!(v.seen.borrow().contains(&(j.backup.clone(), 41)));
    assert!(b.files.contains(&j.install));
}
#[test]
fn invalid_backup_is_manual_and_protected() {
    let (mut b, mut v, mut j) = fixture(Some(41));
    b.fail_after_rename = Some(1);
    let root = j.root.clone();
    assert!(begin(&mut b, &v, &root, &mut j).is_err());
    b.fail_after_rename = None;
    v.denied.extend([j.staged.clone(), j.backup.clone()]);
    assert_eq!(resume(&mut b, &v), Err(Error::ManualRequired));
    let saved = b.durable.clone().unwrap();
    assert_eq!(saved.step, Step::Manual);
    assert!(b.files.contains(&j.backup));
    assert!(!cleanup_allowed(&b, &root, j.backup.parent().unwrap(), &[saved], true).unwrap());
}
#[test]
fn interrupted_rollback_is_not_misclassified_as_install() {
    let (mut b, mut v, mut j) = fixture(Some(41));
    b.fail_after_rename = Some(1);
    let root = j.root.clone();
    assert!(begin(&mut b, &v, &root, &mut j).is_err());
    v.denied.insert(j.staged.clone());
    b.fail_after_rename = Some(2);
    assert_eq!(resume(&mut b, &v), Err(Error::Storage));
    assert_eq!(b.durable.as_ref().unwrap().step, Step::RollingBack);
    b.fail_after_rename = None;
    assert_eq!(resume(&mut b, &v).unwrap().step, Step::RolledBack);
}
#[test]
fn reject_schema_root_traversal_and_symlink_backend_failure() {
    let (mut b, v, j) = fixture(Some(41));
    for case in 0..4 {
        let mut bad = j.clone();
        match case {
            0 => bad.schema = 9,
            1 => bad.root = PathBuf::from("/other"),
            2 => bad.staged = PathBuf::from("/fixture-only/../outside"),
            _ => bad.id = "../escape".into(),
        }
        assert_eq!(
            recover(&mut b, &v, &j.root, &mut bad),
            Err(Error::InvalidJournal)
        );
    }
    b.unsafe_paths = true;
    let mut bad = j.clone();
    assert_eq!(
        recover(&mut b, &v, &j.root, &mut bad),
        Err(Error::InvalidJournal)
    );
    assert_eq!(b.renames, 0);
}
#[test]
fn cleanup_is_fail_closed_and_protects_pending_ancestors() {
    let (b, _, j) = fixture(Some(41));
    assert!(!cleanup_allowed(
        &b,
        &j.root,
        &j.root.join("transactions"),
        &[j.clone()],
        true
    )
    .unwrap());
    assert!(!cleanup_allowed(&b, &j.root, &j.root.join("unused"), &[], false).unwrap());
    assert!(cleanup_allowed(&b, &j.root, &j.root.join("unused"), &[j.clone()], true).unwrap());
}
#[test]
fn serialized_fixture_and_unknown_fields() {
    let j: Journal = serde_json::from_str(include_str!("fixtures/prepared.json")).unwrap();
    j.validate(Path::new("/fixture-only")).unwrap();
    let mut value = serde_json::to_value(j).unwrap();
    value["arbitrary"] = "bad".into();
    assert!(serde_json::from_value::<Journal>(value).is_err());
}
