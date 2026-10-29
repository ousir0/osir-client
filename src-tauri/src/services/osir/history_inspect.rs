//! Explicit read-only inspection. Schema evidence: codex-app-manager's
//! codex_sessions.rs and codex_session_metadata.rs. Never a repair authorization.
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};
const MAX_DB: u64 = 64 * 1024 * 1024;
const MAX_ROLLOUT: u64 = 8 * 1024 * 1024;
const MAX_ROWS: usize = 10_000;
type Result<T> = std::result::Result<T, InspectError>;
/// No raw SQLite errors, paths, IDs or conversation text cross this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectError {
    UnsafePath,
    Unavailable,
    TooLarge,
    Unstable,
    UnsupportedSchema,
    InvalidRollout,
    ThreadNotFound,
    DatabaseUnreadable,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub route_matches_index: bool,
    /// None means no history DB; it does not mean offsets are valid.
    pub offsets_match_rollout: Option<bool>,
    pub checked_offsets: usize,
    pub rollout_records: usize,
    /// Always false: a stable observation is not an offline writer lock.
    pub can_repair: bool,
}
// Directory-relative no-follow opens prevent substituted parent symlinks.
#[cfg(unix)]
fn open_regular(path: &Path) -> Result<fs::File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };
    if !path.is_absolute() {
        return Err(InspectError::UnsafePath);
    }
    let parts: Vec<_> = path.components().collect();
    let mut file = fs::File::open("/").map_err(|_| InspectError::Unavailable)?;
    for (index, part) in parts.iter().enumerate().skip(1) {
        let Component::Normal(name) = part else {
            return Err(InspectError::UnsafePath);
        };
        let name = CString::new(name.as_bytes()).map_err(|_| InspectError::UnsafePath)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if index + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(InspectError::UnsafePath);
        }
        file = unsafe { fs::File::from_raw_fd(fd) };
    }
    let meta = file.metadata().map_err(|_| InspectError::Unavailable)?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(InspectError::UnsafePath);
    }
    Ok(file)
}
#[cfg(not(unix))]
fn open_regular(_: &Path) -> Result<fs::File> {
    Err(InspectError::UnsafePath)
}
#[derive(PartialEq, Eq)]
struct Stamp {
    hash: Vec<u8>,
    modified: Option<std::time::SystemTime>,
    identity: (u64, u64),
}
fn read(path: &Path, max: u64) -> Result<(Vec<u8>, Stamp)> {
    let file = open_regular(path)?;
    let before = file.metadata().map_err(|_| InspectError::Unavailable)?;
    if before.len() > max {
        return Err(InspectError::TooLarge);
    }
    let mut bytes = Vec::new();
    (&file)
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| InspectError::Unavailable)?;
    let after = file.metadata().map_err(|_| InspectError::Unavailable)?;
    if bytes.len() as u64 > max {
        return Err(InspectError::TooLarge);
    }
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || bytes.len() as u64 != after.len()
    {
        return Err(InspectError::Unstable);
    }
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (after.dev(), after.ino())
    };
    #[cfg(not(unix))]
    let identity = (0, 0);
    let stamp = Stamp {
        hash: Sha256::digest(&bytes).to_vec(),
        modified: after.modified().ok(),
        identity,
    };
    Ok((bytes, stamp))
}
fn absent(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(InspectError::Unavailable),
    }
}
fn no_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        if !absent(Path::new(&name))? {
            return Err(InspectError::Unstable);
        }
    }
    Ok(())
}
fn sql<T>(value: rusqlite::Result<T>) -> Result<T> {
    value.map_err(|_| InspectError::DatabaseUnreadable)
}
fn columns(db: &Connection, table: &str, required: &[&str]) -> Result<()> {
    let kind: String = sql(db.query_row(
        "SELECT type FROM sqlite_schema WHERE name=?1",
        [table],
        |row| row.get(0),
    ))
    .map_err(|_| InspectError::UnsupportedSchema)?;
    if kind != "table" {
        return Err(InspectError::UnsupportedSchema);
    }
    // Table names are static constants, never caller or DB input.
    let mut stmt = sql(db.prepare(&format!("PRAGMA table_info({table})")))?;
    let names: BTreeSet<String> = sql(sql(stmt.query_map([], |r| r.get(1)))?.collect())?;
    if required.iter().any(|name| !names.contains(*name)) {
        return Err(InspectError::UnsupportedSchema);
    }
    Ok(())
}
fn snapshot(path: &Path, dest: &Path) -> Result<(Connection, Stamp)> {
    no_sidecars(path)?;
    let (bytes, stamp) = read(path, MAX_DB)?;
    // Only private temporary files are written, never source DBs or sidecars.
    fs::write(dest, bytes).map_err(|_| InspectError::Unavailable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o600))
            .map_err(|_| InspectError::Unavailable)?;
    }
    let db = sql(Connection::open_with_flags(
        dest,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ))?;
    sql(db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;"))?;
    let mut ticks = 0;
    db.progress_handler(
        1000,
        Some(move || {
            ticks += 1;
            ticks > 20_000
        }),
    );
    Ok((db, stamp))
}
/// Native caller supplies canonical absolute roots, including configured sqlite_home.
/// No discovery, scanning, default-route inference, UI content or repair operation.
pub fn inspect(codex_home: &Path, sqlite_home: &Path, thread_id: &str) -> Result<Inspection> {
    inspect_inner(codex_home, sqlite_home, thread_id, || {})
}

/// The native command supplies get_codex_config_dir(), never a renderer path.
/// sqlite_home is read from the same config.toml used by codex-app-manager.
/// Unlike its permissive fallback, malformed/relative overrides fail closed here.
pub fn inspect_configured(home: &Path, thread_id: &str) -> Result<Inspection> {
    inspect_configured_inner(home, thread_id, || {})
}

fn inspect_configured_inner(
    home: &Path,
    thread_id: &str,
    before_inspect: impl FnOnce(),
) -> Result<Inspection> {
    if !home.is_absolute() {
        return Err(InspectError::UnsafePath);
    }
    // Resolve native-selected directory aliases (including macOS /var) once.
    let home = home.canonicalize().map_err(|_| InspectError::Unavailable)?;
    let config = home.join("config.toml");
    let missing = absent(&config)?;
    let (sqlite_home, stamp) = if missing {
        (home.clone(), None)
    } else {
        let (bytes, stamp) = read(&config, 1024 * 1024)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| InspectError::UnsupportedSchema)?;
        let doc: toml::Value = text.parse().map_err(|_| InspectError::UnsupportedSchema)?;
        let sqlite = match doc.get("sqlite_home") {
            None => home.clone(),
            Some(value) => {
                let path = Path::new(value.as_str().ok_or(InspectError::UnsupportedSchema)?);
                if !path.is_absolute()
                    || path.components().any(|c| matches!(c, Component::ParentDir))
                {
                    return Err(InspectError::UnsafePath);
                }
                path.canonicalize().map_err(|_| InspectError::Unavailable)?
            }
        };
        (sqlite, Some(stamp))
    };
    before_inspect();
    let result = inspect(&home, &sqlite_home, thread_id)?;
    if missing {
        if !absent(&config)? {
            return Err(InspectError::Unstable);
        }
    } else if Some(read(&config, 1024 * 1024)?.1) != stamp {
        return Err(InspectError::Unstable);
    }
    Ok(result)
}
fn inspect_inner(
    home: &Path,
    sqlite_home: &Path,
    id: &str,
    after_read: impl FnOnce(),
) -> Result<Inspection> {
    if !home.is_absolute() || !sqlite_home.is_absolute() || id.is_empty() || id.len() > 256 {
        return Err(InspectError::UnsafePath);
    }
    let temp = tempfile::tempdir().map_err(|_| InspectError::Unavailable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| InspectError::Unavailable)?;
    }
    let state_path = sqlite_home.join("state_5.sqlite");
    let history_path = sqlite_home.join("thread_history_1.sqlite");
    let (db, state_stamp) = snapshot(&state_path, &temp.path().join("state.sqlite"))?;
    columns(
        &db,
        "threads",
        &["id", "model_provider", "model", "rollout_path"],
    )?;
    let mut stmt = sql(
        db.prepare("SELECT model_provider,model,rollout_path FROM threads WHERE id=?1 LIMIT 2")
    )?;
    let mut rows = sql(stmt.query([id]))?;
    let row = sql(rows.next())?.ok_or(InspectError::ThreadNotFound)?;
    let provider: String = sql(row.get(0))?;
    let model: Option<String> = sql(row.get(1))?;
    let path: String = sql(row.get(2))?;
    if sql(rows.next())?.is_some() {
        return Err(InspectError::UnsupportedSchema);
    }
    let rollout = PathBuf::from(path);
    let rollout = if rollout.is_absolute() {
        rollout
    } else {
        home.join(rollout)
    };
    if !(rollout.starts_with(home.join("sessions"))
        || rollout.starts_with(home.join("archived_sessions")))
        || rollout.extension().is_none_or(|ext| ext != "jsonl")
    {
        return Err(InspectError::UnsafePath);
    }
    let (bytes, rollout_stamp) = read(&rollout, MAX_ROLLOUT)?;
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(InspectError::InvalidRollout);
    }
    let mut boundaries = vec![0_i64];
    let mut route_matches = true;
    let mut model_seen = false;
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if line.len() > 1024 * 1024 || index >= MAX_ROWS {
            return Err(InspectError::TooLarge);
        }
        let value: Value =
            serde_json::from_slice(line).map_err(|_| InspectError::InvalidRollout)?;
        let kind = value["type"].as_str().ok_or(InspectError::InvalidRollout)?;
        let p = &value["payload"];
        if !p.is_object() {
            return Err(InspectError::InvalidRollout);
        }
        if index == 0 && kind != "session_meta" {
            return Err(InspectError::InvalidRollout);
        }
        match kind {
            "session_meta" => {
                if index != 0 || p["id"].as_str() != Some(id) {
                    return Err(InspectError::InvalidRollout);
                }
                route_matches &= p["model_provider"].as_str() == Some(provider.as_str());
                if let Some(m) = p.get("model") {
                    model_seen = true;
                    route_matches &= m.as_str() == model.as_deref();
                }
            }
            "event_msg" if p["type"] == "thread_settings_applied" => {
                if p["thread_id"].as_str() != Some(id) {
                    return Err(InspectError::InvalidRollout);
                }
                let settings = &p["thread_settings"];
                model_seen = true;
                route_matches &= settings["model_provider_id"].as_str() == Some(provider.as_str())
                    && settings["model"].as_str() == model.as_deref();
            }
            "turn_context" => {
                model_seen = true;
                route_matches &= p["model"].as_str() == model.as_deref();
            }
            "event_msg" | "response_item" => {}
            _ => return Err(InspectError::InvalidRollout),
        }
        boundaries.push(boundaries.last().unwrap() + line.len() as i64);
    }
    route_matches &= model_seen && model.is_some();
    let mut checked = 0;
    let history_missing = absent(&history_path)?;
    let mut history_stamp = None;
    let offsets_match = if history_missing {
        None
    } else {
        let (history, stamp) = snapshot(&history_path, &temp.path().join("history.sqlite"))?;
        history_stamp = Some(stamp);
        columns(
            &history,
            "thread_turns",
            &[
                "thread_id",
                "rollout_byte_offset",
                "rollout_end_byte_offset",
            ],
        )?;
        columns(
            &history,
            "thread_history_projection_state",
            &[
                "thread_id",
                "next_rollout_byte_offset",
                "next_rollout_ordinal",
            ],
        )?;
        let valid = |n: i64| boundaries.binary_search(&n).is_ok();
        let mut matches = true;
        let mut stmt = sql(history.prepare("SELECT rollout_byte_offset,rollout_end_byte_offset FROM thread_turns WHERE thread_id=?1 LIMIT 10001"))?;
        let mut rows = sql(stmt.query([id]))?;
        while let Some(row) = sql(rows.next())? {
            if checked >= MAX_ROWS * 2 {
                return Err(InspectError::TooLarge);
            }
            let start: Option<i64> = sql(row.get(0))?;
            let end: Option<i64> = sql(row.get(1))?;
            checked += 2;
            matches &= matches!((start,end), (Some(a),Some(b)) if a <= b && valid(a) && valid(b));
        }
        let mut stmt = sql(history.prepare("SELECT next_rollout_byte_offset,next_rollout_ordinal FROM thread_history_projection_state WHERE thread_id=?1 LIMIT 2"))?;
        let mut rows = sql(stmt.query([id]))?;
        if let Some(row) = sql(rows.next())? {
            let offset: i64 = sql(row.get(0))?;
            let ordinal: i64 = sql(row.get(1))?;
            checked += 1;
            matches &= ordinal >= 0 && boundaries.get(ordinal as usize) == Some(&offset);
            if sql(rows.next())?.is_some() {
                return Err(InspectError::UnsupportedSchema);
            }
        } else {
            matches = false;
        }
        Some(matches)
    };
    after_read();
    for path in [&state_path, &history_path] {
        no_sidecars(path)?;
    }
    if read(&state_path, MAX_DB)?.1 != state_stamp
        || read(&rollout, MAX_ROLLOUT)?.1 != rollout_stamp
    {
        return Err(InspectError::Unstable);
    }
    if history_missing {
        if !absent(&history_path)? {
            return Err(InspectError::Unstable);
        }
    } else if Some(read(&history_path, MAX_DB)?.1) != history_stamp {
        return Err(InspectError::Unstable);
    }
    Ok(Inspection {
        route_matches_index: route_matches,
        offsets_match_rollout: offsets_match,
        checked_offsets: checked,
        rollout_records: boundaries.len() - 1,
        can_repair: false,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        rollout: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            fs::create_dir(root.join("sessions")).unwrap();
            let rollout = root.join("sessions/rollout-fixture.jsonl");
            let records = [
                json!({"type":"session_meta","payload":{"id":"fixture-thread","model_provider":"openai"}}),
                json!({"type":"event_msg","payload":{"type":"thread_settings_applied","thread_id":"fixture-thread","thread_settings":{"model_provider_id":"openai","model":"fixture-model"}}}),
                json!({"type":"response_item","payload":{"text":"PRIVATE PROMPT BODY MUST NOT ESCAPE"}}),
            ];
            let bytes = records.iter().map(|v| format!("{v}\n")).collect::<String>();
            fs::write(&rollout, &bytes).unwrap();
            let db = Connection::open(root.join("state_5.sqlite")).unwrap();
            db.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY,model_provider TEXT NOT NULL,model TEXT,rollout_path TEXT NOT NULL);").unwrap();
            db.execute(
                "INSERT INTO threads VALUES('fixture-thread','openai','fixture-model',?1)",
                [rollout.to_str().unwrap()],
            )
            .unwrap();
            let history = Connection::open(root.join("thread_history_1.sqlite")).unwrap();
            history.execute_batch("CREATE TABLE thread_turns(thread_id TEXT,turn_id TEXT,rollout_byte_offset INTEGER,rollout_end_byte_offset INTEGER,status TEXT); CREATE TABLE thread_history_projection_state(thread_id TEXT PRIMARY KEY,next_rollout_byte_offset INTEGER,next_rollout_ordinal INTEGER);").unwrap();
            history
                .execute(
                    "INSERT INTO thread_turns VALUES('fixture-thread','turn',0,?1,'completed')",
                    [bytes.len() as i64],
                )
                .unwrap();
            history
                .execute(
                    "INSERT INTO thread_history_projection_state VALUES('fixture-thread',?1,3)",
                    [bytes.len() as i64],
                )
                .unwrap();
            drop(history);
            drop(db);
            Self {
                _temp: temp,
                root,
                rollout,
            }
        }
        fn inspect(&self) -> Result<Inspection> {
            inspect(&self.root, &self.root, "fixture-thread")
        }
        fn change(&self, database: &str, query: &str) {
            Connection::open(self.root.join(database))
                .unwrap()
                .execute_batch(query)
                .unwrap();
        }
        fn source_bytes(&self) -> Vec<Vec<u8>> {
            [
                self.root.join("state_5.sqlite"),
                self.root.join("thread_history_1.sqlite"),
                self.rollout.clone(),
            ]
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect()
        }
    }
    #[test]
    fn stable_history_is_read_only_and_dto_contains_no_content_or_repair_permission() {
        let f = Fixture::new();
        let before = f.source_bytes();
        let result = f.inspect().unwrap();
        assert!(result.route_matches_index);
        assert_eq!(result.offsets_match_rollout, Some(true));
        assert_eq!(result.rollout_records, 3);
        assert!(!result.can_repair);
        let dto = serde_json::to_string(&result).unwrap();
        for private in [
            "PRIVATE",
            "fixture-thread",
            "fixture-model",
            "openai",
            "rollout-fixture",
        ] {
            assert!(!dto.contains(private));
        }
        assert_eq!(f.source_bytes(), before);
        no_sidecars(&f.root.join("state_5.sqlite")).unwrap();
        no_sidecars(&f.root.join("thread_history_1.sqlite")).unwrap();
    }
    #[test]
    fn mismatched_routes_and_offset_boundaries_are_reported_without_repair() {
        let f = Fixture::new();
        f.change(
            "state_5.sqlite",
            "UPDATE threads SET model_provider='other'",
        );
        f.change("thread_history_1.sqlite", "UPDATE thread_turns SET rollout_end_byte_offset=1; UPDATE thread_history_projection_state SET next_rollout_ordinal=999");
        let before = f.source_bytes();
        let result = f.inspect().unwrap();
        assert!(!result.route_matches_index);
        assert_eq!(result.offsets_match_rollout, Some(false));
        assert!(!result.can_repair);
        assert_eq!(f.source_bytes(), before);
    }
    #[test]
    fn missing_history_is_unknown_and_missing_columns_are_unsupported() {
        let f = Fixture::new();
        fs::remove_file(f.root.join("thread_history_1.sqlite")).unwrap();
        assert_eq!(f.inspect().unwrap().offsets_match_rollout, None);
        f.change(
            "state_5.sqlite",
            "ALTER TABLE threads RENAME COLUMN rollout_path TO unknown_path",
        );
        assert_eq!(f.inspect().unwrap_err(), InspectError::UnsupportedSchema);
    }
    #[test]
    fn wal_and_changes_during_inspection_refuse_stable_conclusions() {
        for sidecar in [
            "state_5.sqlite-wal",
            "thread_history_1.sqlite-journal",
            "state_5.sqlite-shm",
        ] {
            let f = Fixture::new();
            fs::write(f.root.join(sidecar), b"").unwrap();
            assert_eq!(f.inspect().unwrap_err(), InspectError::Unstable);
        }
        let f = Fixture::new();
        let result = inspect_inner(&f.root, &f.root, "fixture-thread", || {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(&f.rollout)
                .unwrap()
                .write_all(b" \n")
                .unwrap();
        });
        assert_eq!(result.unwrap_err(), InspectError::Unstable);
        let f = Fixture::new();
        let result = inspect_inner(&f.root, &f.root, "fixture-thread", || {
            f.change(
                "state_5.sqlite",
                "UPDATE threads SET model='concurrent-model'",
            );
        });
        assert_eq!(result.unwrap_err(), InspectError::Unstable);
    }
    #[test]
    fn traversal_symlinks_and_hardlinks_are_refused() {
        let f = Fixture::new();
        f.change(
            "state_5.sqlite",
            "UPDATE threads SET rollout_path='sessions/../state_5.sqlite'",
        );
        assert_eq!(f.inspect().unwrap_err(), InspectError::UnsafePath);
        let f = Fixture::new();
        let moved = f.root.join("original.jsonl");
        fs::rename(&f.rollout, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &f.rollout).unwrap();
        assert_eq!(f.inspect().unwrap_err(), InspectError::UnsafePath);
        fs::remove_file(&f.rollout).unwrap();
        fs::hard_link(&moved, &f.rollout).unwrap();
        assert_eq!(f.inspect().unwrap_err(), InspectError::UnsafePath);
        let f = Fixture::new();
        fs::rename(f.root.join("sessions"), f.root.join("real-sessions")).unwrap();
        std::os::unix::fs::symlink(f.root.join("real-sessions"), f.root.join("sessions")).unwrap();
        assert_eq!(f.inspect().unwrap_err(), InspectError::UnsafePath);
    }
    #[test]
    fn oversized_and_partial_rollouts_are_refused() {
        let f = Fixture::new();
        fs::OpenOptions::new()
            .write(true)
            .open(&f.rollout)
            .unwrap()
            .set_len(MAX_ROLLOUT + 1)
            .unwrap();
        assert_eq!(f.inspect().unwrap_err(), InspectError::TooLarge);
        fs::write(&f.rollout, b"{partial").unwrap();
        assert_eq!(f.inspect().unwrap_err(), InspectError::InvalidRollout);
        let f = Fixture::new();
        fs::OpenOptions::new()
            .write(true)
            .open(f.root.join("state_5.sqlite"))
            .unwrap()
            .set_len(MAX_DB + 1)
            .unwrap();
        assert_eq!(f.inspect().unwrap_err(), InspectError::TooLarge);
    }
    #[test]
    fn explicit_sqlite_home_is_supported_without_default_home_discovery() {
        let f = Fixture::new();
        let sqlite = f.root.join("sqlite");
        fs::create_dir(&sqlite).unwrap();
        for name in ["state_5.sqlite", "thread_history_1.sqlite"] {
            fs::rename(f.root.join(name), sqlite.join(name)).unwrap();
        }
        let result = inspect(&f.root, &sqlite, "fixture-thread").unwrap();
        assert!(result.route_matches_index);
        assert_eq!(result.offsets_match_rollout, Some(true));
    }

    #[test]
    fn configured_native_home_uses_default_or_explicit_sqlite_home_read_only() {
        let f = Fixture::new();
        let before = f.source_bytes();
        assert!(
            inspect_configured(&f.root, "fixture-thread")
                .unwrap()
                .route_matches_index
        );
        assert_eq!(f.source_bytes(), before);
        let sqlite = f.root.join("custom-sqlite");
        fs::create_dir(&sqlite).unwrap();
        for name in ["state_5.sqlite", "thread_history_1.sqlite"] {
            fs::rename(f.root.join(name), sqlite.join(name)).unwrap();
        }
        let config = toml::to_string(&json!({"sqlite_home": sqlite.to_str().unwrap()})).unwrap();
        fs::write(f.root.join("config.toml"), &config).unwrap();
        assert!(
            inspect_configured(&f.root, "fixture-thread")
                .unwrap()
                .route_matches_index
        );
        assert_eq!(
            fs::read_to_string(f.root.join("config.toml")).unwrap(),
            config
        );
        assert!(!f.root.join("state_5.sqlite").exists());
        assert_eq!(fs::read(sqlite.join("state_5.sqlite")).unwrap(), before[0]);
        assert_eq!(
            fs::read(sqlite.join("thread_history_1.sqlite")).unwrap(),
            before[1]
        );
    }

    #[test]
    fn invalid_native_config_never_falls_back_to_default_history() {
        let f = Fixture::new();
        for config in [
            "{broken",
            "sqlite_home = 42",
            "sqlite_home = '../other'",
            "sqlite_home = ''",
        ] {
            fs::write(f.root.join("config.toml"), config).unwrap();
            assert!(inspect_configured(&f.root, "fixture-thread").is_err());
        }
        fs::remove_file(f.root.join("config.toml")).unwrap();
        fs::write(f.root.join("real-config.toml"), "").unwrap();
        std::os::unix::fs::symlink(f.root.join("real-config.toml"), f.root.join("config.toml"))
            .unwrap();
        assert_eq!(
            inspect_configured(&f.root, "fixture-thread").unwrap_err(),
            InspectError::UnsafePath
        );
    }

    #[test]
    fn native_config_change_or_creation_during_inspection_is_unstable() {
        for initially_present in [false, true] {
            let f = Fixture::new();
            let path = f.root.join("config.toml");
            if initially_present {
                fs::write(&path, "# initial").unwrap();
            }
            let result = inspect_configured_inner(&f.root, "fixture-thread", || {
                fs::write(&path, "# changed during inspection").unwrap();
            });
            assert_eq!(result.unwrap_err(), InspectError::Unstable);
        }
    }
}
