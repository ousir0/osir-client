//! Explicit offline single-thread resume repair. Schema evidence: Manager
//! codex_sessions.rs / codex_session_metadata.rs. No startup work or IPC here.
//! Run on a blocking worker; keep Desktop/CLI/app-server closed until completion.
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};
const DB_LIMIT: u64 = 64 * 1024 * 1024;
const ROLLOUT_LIMIT: u64 = 32 * 1024 * 1024;
const ROW_LIMIT: usize = 100_000;
static OPERATION: Mutex<()> = Mutex::new(());
pub type Result<T> = std::result::Result<T, RepairError>;
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairError {
    UnsupportedPlatform,
    WritersRunning,
    ProcessCheckFailed,
    Busy,
    UnsafePath,
    TooLarge,
    UnsupportedSchema,
    InvalidRollout,
    UnmappedRoute,
    Changed,
    PendingRecovery,
    Storage,
    RecoveryConflict,
    NoChanges,
}
fn io<T>(r: std::io::Result<T>) -> Result<T> {
    r.map_err(|_| RepairError::Storage)
}
fn sql<T>(r: rusqlite::Result<T>) -> Result<T> {
    r.map_err(|_| RepairError::UnsupportedSchema)
}
fn hash(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Route {
    pub provider: String,
    pub model: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mapping {
    pub thread_id: String,
    pub source: Route,
    pub target: Route,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub mapping: Mapping,
    pub changed_records: usize,
    pub changed_offsets: usize,
    pub history_cache_present: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub operation_id: String,
    pub restored: bool,
}
/// Native one-use capability: neither Deserialize nor Serialize. Retain behind a
/// random confirmation ID; only preview() may be returned to the renderer.
pub struct Plan {
    binding: Option<TargetBinding>,
    created: std::time::Instant,
    roots: Roots,
    config: Option<String>,
    mapping: Mapping,
    rollout: PathBuf,
    before: Vec<Vec<u8>>,
    after: Vec<Vec<u8>>,
    view: Preview,
}
impl Plan {
    pub(crate) fn bind_target(&mut self, binding: TargetBinding) {
        self.binding = Some(binding);
    }
    pub fn preview(&self) -> &Preview {
        &self.view
    }
}
#[derive(Clone)]
struct Roots {
    home: PathBuf,
    sqlite: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    #[serde(default)]
    binding: Option<TargetBinding>,
    version: u32,
    config: Option<String>,
    mapping: Mapping,
    rollout: PathBuf,
    before: Vec<String>,
    after: Vec<String>,
}

#[cfg(unix)]
fn open_path(path: &Path, directory: bool) -> Result<File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };
    if !path.is_absolute() {
        return Err(RepairError::UnsafePath);
    }
    let parts: Vec<_> = path.components().collect();
    let mut fd = io(File::open("/"))?;
    for (i, p) in parts.iter().enumerate().skip(1) {
        let Component::Normal(name) = p else {
            return Err(RepairError::UnsafePath);
        };
        let name = CString::new(name.as_bytes()).map_err(|_| RepairError::UnsafePath)?;
        let dir = i + 1 < parts.len() || directory;
        let raw = unsafe {
            libc::openat(
                fd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | if dir { libc::O_DIRECTORY } else { 0 },
            )
        };
        if raw < 0 {
            return Err(RepairError::UnsafePath);
        }
        fd = unsafe { File::from_raw_fd(raw) };
        let m = io(fd.metadata())?;
        if !dir
            && (!m.is_file()
                || m.nlink() != 1
                || m.uid() != unsafe { libc::geteuid() }
                || m.mode() & 0o022 != 0)
        {
            return Err(RepairError::UnsafePath);
        }
    }
    Ok(fd)
}
#[cfg(not(unix))]
fn open_path(_: &Path, _: bool) -> Result<File> {
    Err(RepairError::UnsupportedPlatform)
}
fn read(p: &Path, max: u64) -> Result<Vec<u8>> {
    let f = open_path(p, false)?;
    if io(f.metadata())?.len() > max {
        return Err(RepairError::TooLarge);
    }
    let mut b = Vec::new();
    io(f.take(max + 1).read_to_end(&mut b))?;
    if b.len() as u64 > max {
        return Err(RepairError::TooLarge);
    }
    Ok(b)
}
fn exists(p: &Path) -> Result<bool> {
    match fs::symlink_metadata(p) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(RepairError::Storage),
    }
}
fn owned_dir(p: &Path) -> Result<()> {
    let f = open_path(p, true)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = io(f.metadata())?;
        if m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o022 != 0 {
            return Err(RepairError::UnsafePath);
        }
    }
    Ok(())
}
fn sidecars(p: &Path) -> Result<()> {
    for s in ["-wal", "-shm", "-journal"] {
        let mut n = p.as_os_str().to_os_string();
        n.push(s);
        if exists(Path::new(&n))? {
            return Err(RepairError::Busy);
        }
    }
    Ok(())
}
fn versions(sqlite: &Path) -> Result<()> {
    for entry in io(fs::read_dir(sqlite))? {
        let name = io(entry)?.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(".sqlite")
            && ((name.starts_with("state_") && name != "state_5.sqlite")
                || (name.starts_with("thread_history_") && name != "thread_history_1.sqlite"))
        {
            return Err(RepairError::UnsupportedSchema);
        }
    }
    Ok(())
}
fn config(home: &Path) -> Result<(PathBuf, Option<String>)> {
    let p = home.join("config.toml");
    if !exists(&p)? {
        return Ok((home.into(), None));
    }
    let b = read(&p, 1024 * 1024)?;
    let doc: toml::Value = std::str::from_utf8(&b)
        .map_err(|_| RepairError::UnsupportedSchema)?
        .parse()
        .map_err(|_| RepairError::UnsupportedSchema)?;
    let sqlite = match doc.get("sqlite_home") {
        None => home.into(),
        Some(v) => PathBuf::from(v.as_str().ok_or(RepairError::UnsafePath)?),
    };
    owned_dir(&sqlite)?;
    Ok((sqlite, Some(hash(&b))))
}
/// A native observation, never a UI boolean. Deliberately conservative across
/// all homes. It cannot prevent a user manually launching an uncooperative writer.
fn stopped() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let s=std::process::Command::new("/usr/bin/pgrep").args(["-if",r"(^|/|[[:space:]])codex([[:space:]]|$)|Codex.app/Contents/|ChatGPT.app/Contents/.*[Cc]odex"])
 .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map_err(|_|RepairError::ProcessCheckFailed)?;
        match s.code() {
            Some(1) => Ok(()),
            Some(0) => Err(RepairError::WritersRunning),
            _ => Err(RepairError::ProcessCheckFailed),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(RepairError::UnsupportedPlatform)
    }
}
fn valid(m: &Mapping) -> Result<()> {
    for s in [
        &m.thread_id,
        &m.source.provider,
        &m.source.model,
        &m.target.provider,
        &m.target.model,
    ] {
        if s.is_empty() || s.len() > 256 || s.trim() != s || s.chars().any(char::is_control) {
            return Err(RepairError::UnmappedRoute);
        }
    }
    if m.source == m.target {
        return Err(RepairError::UnmappedRoute);
    }
    Ok(())
}
fn route(p: Option<&str>, model: Option<&str>, m: &Mapping) -> Result<()> {
    if [&m.source, &m.target]
        .iter()
        .any(|r| p == Some(r.provider.as_str()) && model == Some(r.model.as_str()))
    {
        Ok(())
    } else {
        Err(RepairError::UnmappedRoute)
    }
}
fn transform(bytes: &[u8], m: &Mapping) -> Result<(Vec<u8>, BTreeMap<i64, i64>, usize)> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(RepairError::InvalidRollout);
    }
    let mut out = Vec::new();
    let mut boundaries = BTreeMap::from([(0, 0)]);
    let mut end = 0;
    let mut changed = 0;
    for (i, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if i >= ROW_LIMIT || line.len() > 4 * 1024 * 1024 {
            return Err(RepairError::TooLarge);
        }
        let mut v: Value = serde_json::from_slice(line).map_err(|_| RepairError::InvalidRollout)?;
        let original = v.clone();
        let kind = v["type"]
            .as_str()
            .ok_or(RepairError::InvalidRollout)?
            .to_owned();
        let p = v["payload"]
            .as_object_mut()
            .ok_or(RepairError::InvalidRollout)?;
        if i == 0 && kind != "session_meta" {
            return Err(RepairError::InvalidRollout);
        }
        match kind.as_str() {
            "session_meta" => {
                if i != 0 || p.get("id").and_then(Value::as_str) != Some(m.thread_id.as_str()) {
                    return Err(RepairError::InvalidRollout);
                }
                let provider = p.get("model_provider").and_then(Value::as_str);
                if provider != Some(m.source.provider.as_str())
                    && provider != Some(m.target.provider.as_str())
                {
                    return Err(RepairError::UnmappedRoute);
                }
                if let Some(model) = p.get("model") {
                    route(provider, model.as_str(), m)?;
                }
                p.insert("model_provider".into(), m.target.provider.clone().into());
                if p.contains_key("model") {
                    p.insert("model".into(), m.target.model.clone().into());
                }
            }
            "event_msg"
                if p.get("type").and_then(Value::as_str) == Some("thread_settings_applied") =>
            {
                if p.get("thread_id").and_then(Value::as_str) != Some(m.thread_id.as_str()) {
                    return Err(RepairError::InvalidRollout);
                }
                let s = p
                    .get_mut("thread_settings")
                    .and_then(Value::as_object_mut)
                    .ok_or(RepairError::InvalidRollout)?;
                route(
                    s.get("model_provider_id").and_then(Value::as_str),
                    s.get("model").and_then(Value::as_str),
                    m,
                )?;
                s.insert("model_provider_id".into(), m.target.provider.clone().into());
                s.insert("model".into(), m.target.model.clone().into());
            }
            // turn_context records the model used historically; it is not a resume override.
            "turn_context" | "event_msg" | "response_item" => {}
            _ => return Err(RepairError::InvalidRollout),
        }
        if v == original {
            out.extend_from_slice(line)
        } else {
            serde_json::to_writer(&mut out, &v).map_err(|_| RepairError::InvalidRollout)?;
            if line.ends_with(b"\r\n") {
                out.extend_from_slice(b"\r\n")
            } else {
                out.push(b'\n')
            }
            changed += 1;
        }
        end += line.len() as i64;
        boundaries.insert(end, out.len() as i64);
    }
    Ok((out, boundaries, changed))
}
fn db(p: &Path) -> Result<Connection> {
    let c = sql(Connection::open_with_flags(
        p,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ))?;
    sql(c.execute_batch(
        "PRAGMA trusted_schema=OFF; PRAGMA journal_mode=DELETE; PRAGMA foreign_keys=ON;",
    ))?;
    let mut ticks = 0;
    c.progress_handler(
        1000,
        Some(move || {
            ticks += 1;
            ticks > 100_000
        }),
    );
    let ok: String = sql(c.query_row("PRAGMA quick_check", [], |r| r.get(0)))?;
    if ok != "ok" {
        return Err(RepairError::UnsupportedSchema);
    }
    let n:i64=sql(c.query_row("SELECT count(*) FROM sqlite_schema WHERE type IN ('trigger','view') OR lower(sql) LIKE '%virtual table%'",[],|r|r.get(0)))?;
    if n != 0 {
        return Err(RepairError::UnsupportedSchema);
    }
    Ok(c)
}
fn columns(c: &Connection, table: &str, fields: &[&str]) -> Result<()> {
    let mut q = sql(c.prepare(&format!("PRAGMA table_info({table})")))?;
    let names: Vec<String> = sql(sql(q.query_map([], |r| r.get(1)))?.collect())?;
    if fields.iter().any(|f| !names.iter().any(|n| n == f)) {
        return Err(RepairError::UnsupportedSchema);
    }
    Ok(())
}
fn offset_schema(c: &Connection) -> Result<()> {
    let mut q = sql(c.prepare("SELECT m.name,p.name FROM sqlite_schema m, pragma_table_info(m.name) p WHERE m.type='table' AND lower(p.name) LIKE '%offset%'"))?;
    let fields: Vec<(String, String)> =
        sql(sql(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?))))?.collect())?;
    for (table, field) in fields {
        if !matches!(
            (table.as_str(), field.as_str()),
            (
                "thread_turns",
                "rollout_byte_offset" | "rollout_end_byte_offset"
            ) | (
                "thread_history_projection_state",
                "next_rollout_byte_offset"
            )
        ) {
            return Err(RepairError::UnsupportedSchema);
        }
    }
    Ok(())
}
fn index(c: &Connection, m: &Mapping) -> Result<PathBuf> {
    columns(
        c,
        "threads",
        &["id", "model_provider", "model", "rollout_path"],
    )?;
    let mut q = sql(
        c.prepare("SELECT model_provider,model,rollout_path FROM threads WHERE id=?1 LIMIT 2")
    )?;
    let rows: Vec<(String, Option<String>, String)> = sql(sql(
        q.query_map([&m.thread_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    )?
    .collect())?;
    if rows.len() != 1 {
        return Err(RepairError::UnsupportedSchema);
    }
    route(Some(&rows[0].0), rows[0].1.as_deref(), m)?;
    Ok(PathBuf::from(&rows[0].2))
}
fn rollout_path(roots: &Roots, path: &Path) -> Result<PathBuf> {
    let p = if path.is_absolute() {
        path.into()
    } else {
        roots.home.join(path)
    };
    if !(p.starts_with(roots.home.join("sessions"))
        || p.starts_with(roots.home.join("archived_sessions")))
        || p.extension().is_none_or(|s| s != "jsonl")
    {
        return Err(RepairError::UnsafePath);
    }
    owned_dir(p.parent().ok_or(RepairError::UnsafePath)?)?;
    let mut parent = p.parent();
    while let Some(dir) = parent {
        if dir == roots.home {
            break;
        }
        if !dir.starts_with(&roots.home) {
            return Err(RepairError::UnsafePath);
        }
        owned_dir(dir)?;
        parent = dir.parent();
    }
    let _ = open_path(&p, false)?;
    Ok(p)
}
fn sync(p: &Path) -> Result<()> {
    io(open_path(p, true)?.sync_all())
}
fn write_new(p: &Path, b: &[u8]) -> Result<()> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = io(o.open(p))?;
    io(f.write_all(b))?;
    io(f.sync_all())?;
    sync(p.parent().ok_or(RepairError::UnsafePath)?)
}
fn private_dir(p: &Path) -> Result<()> {
    let mut b = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    io(b.create(p))?;
    sync(p.parent().ok_or(RepairError::UnsafePath)?)
}

// SQLite is opened only on private copies, never on source files during preview.
fn prepare(
    roots: &Roots,
    m: &Mapping,
    before: &[Vec<u8>],
) -> Result<(PathBuf, Vec<Vec<u8>>, usize, usize)> {
    let temp = io(tempfile::tempdir())?;
    let base = io(temp.path().canonicalize())?;
    let state = base.join("state");
    write_new(&state, &before[1])?;
    let c = db(&state)?;
    let rollout = rollout_path(roots, &index(&c, m)?)?;
    let (out, boundaries, records) = transform(&before[0], m)?;
    let index_changes = sql(c.execute(
        "UPDATE threads SET model_provider=?1,model=?2 WHERE id=?3 AND (model_provider IS NOT ?1 OR model IS NOT ?2)",
        params![m.target.provider, m.target.model, m.thread_id],
    ))?;
    drop(c);
    let mut after = vec![out, read(&state, DB_LIMIT)?];
    let mut offsets = 0;
    if before.len() == 3 {
        let path = base.join("history");
        write_new(&path, &before[2])?;
        let mut h = db(&path)?;
        offset_schema(&h)?;
        columns(
            &h,
            "thread_turns",
            &[
                "thread_id",
                "turn_id",
                "rollout_byte_offset",
                "rollout_end_byte_offset",
            ],
        )?;
        columns(
            &h,
            "thread_history_projection_state",
            &[
                "thread_id",
                "next_rollout_byte_offset",
                "next_rollout_ordinal",
            ],
        )?;
        // Ordinals remain unchanged because no records are added or removed.
        let mut q = sql(h.prepare("SELECT next_rollout_byte_offset,next_rollout_ordinal FROM thread_history_projection_state WHERE thread_id=?1 LIMIT 2"))?;
        let projection: Vec<(i64, i64)> =
            sql(sql(q.query_map([&m.thread_id], |r| Ok((r.get(0)?, r.get(1)?))))?.collect())?;
        if projection.len() > 1 {
            return Err(RepairError::UnsupportedSchema);
        }
        for (offset, ordinal) in projection {
            if ordinal < 0 || boundaries.keys().position(|v| *v == offset) != Some(ordinal as usize)
            {
                return Err(RepairError::UnsupportedSchema);
            }
        }
        drop(q);
        let tx = sql(h.transaction())?;
        for (table, key, fields) in [
            (
                "thread_turns",
                "turn_id",
                &["rollout_byte_offset", "rollout_end_byte_offset"][..],
            ),
            (
                "thread_history_projection_state",
                "thread_id",
                &["next_rollout_byte_offset"][..],
            ),
        ] {
            for field in fields {
                let mut q = sql(tx.prepare(&format!(
                    "SELECT {key},{field} FROM {table} WHERE thread_id=?1 LIMIT 100001"
                )))?;
                let rows: Vec<(String, Option<i64>)> = sql(sql(
                    q.query_map([&m.thread_id], |r| Ok((r.get(0)?, r.get(1)?)))
                )?
                .collect())?;
                if rows.len() > ROW_LIMIT {
                    return Err(RepairError::TooLarge);
                }
                drop(q);
                for (id, old) in rows {
                    let Some(old) = old else { continue };
                    let new = *boundaries.get(&old).ok_or(RepairError::UnsupportedSchema)?;
                    if new != old {
                        let n=sql(tx.execute(&format!("UPDATE {table} SET {field}=?1 WHERE thread_id=?2 AND {key}=?3 AND {field}=?4"),params![new,m.thread_id,id,old]))?;
                        if n != 1 {
                            return Err(RepairError::UnsupportedSchema);
                        }
                        offsets += 1;
                    }
                }
            }
        }
        sql(tx.commit())?;
        drop(h);
        after.push(read(&path, DB_LIMIT)?);
    }
    if after[0].len() as u64 > ROLLOUT_LIMIT {
        return Err(RepairError::TooLarge);
    }
    if index_changes == 0 && records == 0 && offsets == 0 {
        return Err(RepairError::NoChanges);
    }
    Ok((rollout, after, records, offsets))
}
fn targets(r: &Roots, p: &Path, n: usize) -> Vec<PathBuf> {
    let mut v = vec![p.into(), r.sqlite.join("state_5.sqlite")];
    if n == 3 {
        v.push(r.sqlite.join("thread_history_1.sqlite"))
    }
    v
}
fn hashes(v: &[Vec<u8>]) -> Vec<String> {
    v.iter().map(|b| hash(b)).collect()
}
fn stable(r: &Roots, cfg: &Option<String>, paths: &[PathBuf], expected: &[String]) -> Result<()> {
    versions(&r.sqlite)?;
    let (sqlite, now) = config(&r.home)?;
    if sqlite != r.sqlite || &now != cfg {
        return Err(RepairError::Changed);
    }
    if paths.len() == 2 && exists(&r.sqlite.join("thread_history_1.sqlite"))? {
        return Err(RepairError::Changed);
    }
    for (i, p) in paths.iter().enumerate() {
        if i > 0 {
            sidecars(p)?
        }
        if hash(&read(p, if i == 0 { ROLLOUT_LIMIT } else { DB_LIMIT })?) != expected[i] {
            return Err(RepairError::Changed);
        }
    }
    Ok(())
}
/// Caller uses native get_codex_config_dir(); renderer must never select paths.
/// Target is an explicit native validated saved provider/model, not a default.
pub fn preview(home: &Path, mapping: Mapping) -> Result<Plan> {
    preview_with_idle(home, mapping, stopped)
}
fn preview_with_idle(home: &Path, mapping: Mapping, idle: impl Fn() -> Result<()>) -> Result<Plan> {
    let _gate = OPERATION.try_lock().map_err(|_| RepairError::Busy)?;
    idle()?;
    valid(&mapping)?;
    let home = io(home.canonicalize())?;
    owned_dir(&home)?;
    let (sqlite, cfg) = config(&home)?;
    owned_dir(&sqlite)?;
    let roots = Roots { home, sqlite };
    let state = roots.sqlite.join("state_5.sqlite");
    sidecars(&state)?;
    let state_bytes = read(&state, DB_LIMIT)?;
    let temp = io(tempfile::tempdir())?;
    let copy = io(temp.path().canonicalize())?.join("state");
    write_new(&copy, &state_bytes)?;
    let c = db(&copy)?;
    let rollout = rollout_path(&roots, &index(&c, &mapping)?)?;
    drop(c);
    let mut before = vec![read(&rollout, ROLLOUT_LIMIT)?, state_bytes];
    let h = roots.sqlite.join("thread_history_1.sqlite");
    if exists(&h)? {
        sidecars(&h)?;
        before.push(read(&h, DB_LIMIT)?);
    }
    let (_, after, records, offsets) = prepare(&roots, &mapping, &before)?;
    stable(
        &roots,
        &cfg,
        &targets(&roots, &rollout, before.len()),
        &hashes(&before),
    )?;
    idle()?;
    let view = Preview {
        mapping: mapping.clone(),
        changed_records: records,
        changed_offsets: offsets,
        history_cache_present: before.len() == 3,
    };
    Ok(Plan {
        binding: None,
        created: std::time::Instant::now(),
        roots,
        config: cfg,
        mapping,
        rollout,
        before,
        after,
        view,
    })
}
struct Lock {
    _file: File,
}
fn lock(r: &Roots) -> Result<Lock> {
    let p = r.home.join(".osir-history-repair.lock");
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let f = io(o.open(&p))?;
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
        let m = io(f.metadata())?;
        if m.nlink() != 1 || m.uid() != unsafe { libc::geteuid() } {
            return Err(RepairError::UnsafePath);
        }
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(RepairError::Busy);
        }
    }
    Ok(Lock { _file: f })
}
fn operation_root(r: &Roots) -> Result<PathBuf> {
    let p = r.home.join("osir-history-repairs");
    if !exists(&p)? {
        private_dir(&p)?
    }
    owned_dir(&p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if io(open_path(&p, true)?.metadata())?.mode() & 0o077 != 0 {
            return Err(RepairError::UnsafePath);
        }
    }
    Ok(p)
}
fn no_pending(root: &Path, except: Option<&Path>) -> Result<()> {
    for e in io(fs::read_dir(root))? {
        let p = io(e)?.path();
        owned_dir(&p)?;
        if except == Some(p.as_path()) {
            continue;
        }
        if exists(&p.join("journal.json"))?
            && !exists(&p.join("restored"))?
            && (!exists(&p.join("completed"))? || exists(&p.join("restoring"))?)
        {
            return Err(RepairError::PendingRecovery);
        }
    }
    Ok(())
}
fn replace(p: &Path, b: &[u8]) -> Result<()> {
    let parent = p.parent().ok_or(RepairError::UnsafePath)?;
    owned_dir(parent)?;
    let mut t = io(tempfile::NamedTempFile::new_in(parent))?;
    io(t.write_all(b))?;
    if let Ok(modified) = open_path(p, false)?.metadata().and_then(|m| m.modified()) {
        io(t.as_file()
            .set_times(fs::FileTimes::new().set_modified(modified)))?;
    }
    io(t.as_file().sync_all())?;
    t.persist(p).map_err(|_| RepairError::Storage)?;
    sync(parent)
}
/// Explicit user confirmation consumes the native preview. Durable before/after
/// backups and journal precede all source replacements. Errors retain recovery.
pub fn apply(plan: Plan) -> Result<Receipt> {
    apply_with_idle(plan, stopped)
}
fn apply_with_idle(plan: Plan, idle: impl Fn() -> Result<()>) -> Result<Receipt> {
    let _gate = OPERATION.try_lock().map_err(|_| RepairError::Busy)?;
    idle()?;
    if plan.created.elapsed() > std::time::Duration::from_secs(300) {
        return Err(RepairError::Changed);
    }
    let _lock = lock(&plan.roots)?;
    let root = operation_root(&plan.roots)?;
    no_pending(&root, None)?;
    let paths = targets(&plan.roots, &plan.rollout, plan.before.len());
    stable(&plan.roots, &plan.config, &paths, &hashes(&plan.before))?;
    let id = uuid::Uuid::new_v4().to_string();
    let dir = root.join(&id);
    private_dir(&dir)?;
    for (i, (a, b)) in plan.before.iter().zip(&plan.after).enumerate() {
        write_new(&dir.join(format!("{i}.before")), a)?;
        write_new(&dir.join(format!("{i}.after")), b)?;
    }
    let j = Journal {
        binding: plan.binding,
        version: 1,
        config: plan.config,
        mapping: plan.mapping,
        rollout: plan.rollout,
        before: hashes(&plan.before),
        after: hashes(&plan.after),
    };
    write_new(
        &dir.join("journal.json"),
        &serde_json::to_vec(&j).map_err(|_| RepairError::Storage)?,
    )?;
    if let Err(error) = finish(
        &plan.roots,
        &dir,
        &j,
        &plan.before,
        &plan.after,
        false,
        &idle,
    ) {
        // Roll back only while still offline and while every image is exactly
        // before/after. A running writer or external edit preserves pending work.
        if finish(
            &plan.roots,
            &dir,
            &j,
            &plan.before,
            &plan.after,
            true,
            &idle,
        )
        .is_err()
        {
            return Err(RepairError::PendingRecovery);
        }
        return Err(error);
    }
    Ok(Receipt {
        operation_id: id,
        restored: false,
    })
}
fn finish(
    r: &Roots,
    dir: &Path,
    j: &Journal,
    before: &[Vec<u8>],
    after: &[Vec<u8>],
    restore: bool,
    idle: impl Fn() -> Result<()>,
) -> Result<()> {
    let paths = targets(r, &j.rollout, before.len());
    let mut current = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let h = hash(&read(p, if i == 0 { ROLLOUT_LIMIT } else { DB_LIMIT })?);
        if h != j.before[i] && h != j.after[i] {
            return Err(RepairError::RecoveryConflict);
        }
        current.push(h);
    }
    let desired = if restore { before } else { after };
    if exists(&dir.join("restored"))? && current != j.before {
        return Err(RepairError::RecoveryConflict);
    }
    if exists(&dir.join("completed"))?
        && !exists(&dir.join("restored"))?
        && !exists(&dir.join("restoring"))?
        && current != j.after
    {
        return Err(RepairError::RecoveryConflict);
    }
    idle()?;
    stable(r, &j.config, &paths, &current)?;
    if restore && !exists(&dir.join("restoring"))? {
        write_new(&dir.join("restoring"), b"1\n")?;
    }
    for (i, p) in paths.iter().enumerate() {
        idle()?;
        stable(r, &j.config, &paths, &current)?;
        if current[i] != hash(&desired[i]) {
            replace(p, &desired[i])?;
            current[i] = hash(&desired[i]);
        }
    }
    stable(r, &j.config, &paths, &current)?;
    idle()?;
    let marker = dir.join(if restore { "restored" } else { "completed" });
    if !exists(&marker)? {
        write_new(&marker, b"1\n")?
    }
    Ok(())
}
/// Explicit interrupted apply completion or rollback. Any later external file
/// change refuses whole-file recovery. Backups are retained, never auto-deleted.
pub fn recover(home: &Path, operation_id: &str, restore: bool) -> Result<Receipt> {
    recover_with_idle(home, operation_id, restore, stopped)
}
fn recover_with_idle(
    home: &Path,
    operation_id: &str,
    restore: bool,
    idle: impl Fn() -> Result<()>,
) -> Result<Receipt> {
    let _gate = OPERATION.try_lock().map_err(|_| RepairError::Busy)?;
    idle()?;
    if uuid::Uuid::parse_str(operation_id)
        .map_err(|_| RepairError::UnsafePath)?
        .to_string()
        != operation_id
    {
        return Err(RepairError::UnsafePath);
    }
    let home = io(home.canonicalize())?;
    owned_dir(&home)?;
    let (sqlite, cfg) = config(&home)?;
    owned_dir(&sqlite)?;
    let r = Roots { home, sqlite };
    let _lock = lock(&r)?;
    let dir = operation_root(&r)?.join(operation_id);
    owned_dir(&dir)?;
    no_pending(dir.parent().ok_or(RepairError::UnsafePath)?, Some(&dir))?;
    let j: Journal = serde_json::from_slice(&read(&dir.join("journal.json"), 64 * 1024)?)
        .map_err(|_| RepairError::RecoveryConflict)?;
    if j.version != 1
        || j.config != cfg
        || !(2..=3).contains(&j.before.len())
        || j.before.len() != j.after.len()
    {
        return Err(RepairError::RecoveryConflict);
    }
    valid(&j.mapping)?;
    let mut before = Vec::new();
    let mut after = Vec::new();
    for i in 0..j.before.len() {
        let a = read(
            &dir.join(format!("{i}.before")),
            if i == 0 { ROLLOUT_LIMIT } else { DB_LIMIT },
        )?;
        let b = read(
            &dir.join(format!("{i}.after")),
            if i == 0 { ROLLOUT_LIMIT } else { DB_LIMIT },
        )?;
        if hash(&a) != j.before[i] || hash(&b) != j.after[i] {
            return Err(RepairError::RecoveryConflict);
        }
        before.push(a);
        after.push(b);
    }
    // Re-derive every changed byte from the backed-up schema/mapping, not merely
    // journal hashes or paths. Unknown schema or tampered backups cannot commit.
    let (rollout, derived, _, _) = prepare(&r, &j.mapping, &before)?;
    if rollout != j.rollout || hashes(&derived) != j.after {
        return Err(RepairError::RecoveryConflict);
    }
    if (exists(&dir.join("restored"))? || exists(&dir.join("restoring"))?) && !restore {
        return Err(RepairError::RecoveryConflict);
    }
    finish(&r, &dir, &j, &before, &after, restore, idle)?;
    Ok(Receipt {
        operation_id: operation_id.into(),
        restored: restore,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Restoring,
    Completed,
    Restored,
    IncompleteBackup,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedOperation {
    pub operation_id: String,
    pub state: OperationState,
}
/// Read-only discovery after an error/restart. States describe durable receipts,
/// not a claim that the current history still matches; recover revalidates it.
pub fn operations(home: &Path) -> Result<Vec<SavedOperation>> {
    let home = io(home.canonicalize())?;
    owned_dir(&home)?;
    let root = home.join("osir-history-repairs");
    if !exists(&root)? {
        return Ok(Vec::new());
    }
    owned_dir(&root)?;
    let mut out = Vec::new();
    for entry in io(fs::read_dir(&root))? {
        if out.len() >= 1000 {
            return Err(RepairError::TooLarge);
        }
        let entry = io(entry)?;
        let p = entry.path();
        owned_dir(&p)?;
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| RepairError::UnsafePath)?;
        if uuid::Uuid::parse_str(&id)
            .map_err(|_| RepairError::UnsafePath)?
            .to_string()
            != id
        {
            return Err(RepairError::UnsafePath);
        }
        let state = if !exists(&p.join("journal.json"))? {
            OperationState::IncompleteBackup
        } else if exists(&p.join("restored"))? {
            OperationState::Restored
        } else if exists(&p.join("restoring"))? {
            OperationState::Restoring
        } else if exists(&p.join("completed"))? {
            OperationState::Completed
        } else {
            OperationState::Pending
        };
        out.push(SavedOperation {
            operation_id: id,
            state,
        });
    }
    out.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn mapping() -> Mapping {
        Mapping {
            thread_id: "fixture-thread".into(),
            source: Route {
                provider: "old".into(),
                model: "old-model".into(),
            },
            target: Route {
                provider: "long-new-provider".into(),
                model: "new-model".into(),
            },
        }
    }
    fn rollout() -> Vec<u8> {
        [json!({"type":"session_meta","payload":{"id":"fixture-thread","model_provider":"old"}}),
         json!({"type":"turn_context","payload":{"model":"historical-model"}}),
         json!({"type":"event_msg","payload":{"type":"thread_settings_applied","thread_id":"fixture-thread","thread_settings":{"model_provider_id":"old","model":"old-model"}}}),
         json!({"type":"response_item","payload":{"text":"PRIVATE FIXTURE"}})]
        .iter().map(|v|format!("{v}\n")).collect::<String>().into_bytes()
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        roots: Roots,
        path: PathBuf,
        before: Vec<Vec<u8>>,
    }
    impl Fixture {
        fn new() -> Self {
            Self::with_rollout(rollout())
        }
        fn with_rollout(bytes: Vec<u8>) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().canonicalize().unwrap();
            let roots = Roots {
                home: home.clone(),
                sqlite: home.clone(),
            };
            private_dir(&home.join("sessions")).unwrap();
            let path = home.join("sessions/thread.jsonl");
            write_new(&path, &bytes).unwrap();
            let state = home.join("state_5.sqlite");
            let c = Connection::open(&state).unwrap();
            c.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY,model_provider TEXT,model TEXT,rollout_path TEXT); INSERT INTO threads VALUES('other','keep','keep-model','keep-path');").unwrap();
            c.execute(
                "INSERT INTO threads VALUES('fixture-thread','old','old-model',?1)",
                [path.to_str().unwrap()],
            )
            .unwrap();
            drop(c);
            let history = home.join("thread_history_1.sqlite");
            let h = Connection::open(&history).unwrap();
            h.execute_batch("CREATE TABLE thread_turns(thread_id TEXT,turn_id TEXT,rollout_byte_offset INTEGER,rollout_end_byte_offset INTEGER); CREATE TABLE thread_history_projection_state(thread_id TEXT PRIMARY KEY,next_rollout_byte_offset INTEGER,next_rollout_ordinal INTEGER);").unwrap();
            h.execute(
                "INSERT INTO thread_turns VALUES('fixture-thread','turn',0,?1)",
                [bytes.len() as i64],
            )
            .unwrap();
            h.execute(
                "INSERT INTO thread_history_projection_state VALUES('fixture-thread',?1,?2)",
                params![
                    bytes.len() as i64,
                    bytes.iter().filter(|b| **b == b'\n').count() as i64
                ],
            )
            .unwrap();
            drop(h);
            let before = vec![
                bytes,
                read(&state, DB_LIMIT).unwrap(),
                read(&history, DB_LIMIT).unwrap(),
            ];
            Self {
                _temp: temp,
                roots,
                path,
                before,
            }
        }
        fn journal(&self, after: &[Vec<u8>]) -> Journal {
            Journal {
                binding: None,
                version: 1,
                config: None,
                mapping: mapping(),
                rollout: self.path.clone(),
                before: hashes(&self.before),
                after: hashes(after),
            }
        }
        fn images(&self) -> Vec<Vec<u8>> {
            targets(&self.roots, &self.path, 3)
                .iter()
                .map(|p| fs::read(p).unwrap())
                .collect()
        }
    }
    #[test]
    fn preview_apply_and_rollback_preserve_multiturn_bytes_and_interior_offsets() {
        // Different line endings and multibyte content make character offsets
        // and a constant file-length adjustment observably wrong.
        let bytes = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"fixture-thread\",\"model_provider\":\"old\"}}\r\n",
            "{ \"type\":\"turn_context\",\"payload\":{\"model\":\"historical-model\"} }\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_id\":\"fixture-thread\",\"thread_settings\":{\"model_provider_id\":\"old\",\"model\":\"old-model\"}}}\r\n",
            "{ \"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"合成第一轮 🌿\"}]} }\r\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"synthetic-call\",\"name\":\"fixture_tool\",\"arguments\":\"{}\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"historical-second-model\"}}\r\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_id\":\"fixture-thread\",\"thread_settings\":{\"model_provider_id\":\"old\",\"model\":\"old-model\"}}}\n",
            "{  \"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"合成第二轮 ✅\"}]}  }\r\n"
        ).as_bytes().to_vec();
        assert_ne!(
            std::str::from_utf8(&bytes).unwrap().chars().count(),
            bytes.len()
        );
        let mut f = Fixture::with_rollout(bytes);
        let original_rollout = f.before[0].clone();
        let lines: Vec<_> = original_rollout.split_inclusive(|b| *b == b'\n').collect();
        let first = lines[0].len() as i64;
        let second = lines[..5].iter().map(|line| line.len() as i64).sum::<i64>();
        let history = f.roots.sqlite.join("thread_history_1.sqlite");
        let h = Connection::open(&history).unwrap();
        h.execute("DELETE FROM thread_turns", []).unwrap();
        h.execute(
            "INSERT INTO thread_turns VALUES('fixture-thread','first',?1,?2)",
            params![first, second],
        )
        .unwrap();
        h.execute(
            "INSERT INTO thread_turns VALUES('fixture-thread','second',?1,?2)",
            params![second, f.before[0].len() as i64],
        )
        .unwrap();
        h.execute(
            "INSERT INTO thread_turns VALUES('other','untouched',777,999)",
            [],
        )
        .unwrap();
        h.execute("UPDATE thread_history_projection_state SET next_rollout_byte_offset=?1,next_rollout_ordinal=5 WHERE thread_id='fixture-thread'", [second]).unwrap();
        h.execute(
            "INSERT INTO thread_history_projection_state VALUES('other',123,456)",
            [],
        )
        .unwrap();
        drop(h);
        f.before = f.images();

        let mut plan = preview_with_idle(&f.roots.home, mapping(), || Ok(())).unwrap();
        assert_eq!(plan.preview().changed_records, 3);
        assert!(plan.preview().history_cache_present);
        assert_eq!(f.images(), f.before); // Preview must not touch SQLite or JSONL.
        plan.bind_target(TargetBinding {
            provider_id: "synthetic-provider".into(),
            model_id: "new-model".into(),
            fingerprint: "synthetic-binding".into(),
        });
        let receipt = apply_with_idle(plan, || Ok(())).unwrap();
        assert!(!receipt.restored);
        let applied = f.images();
        let after_lines: Vec<_> = applied[0].split_inclusive(|b| *b == b'\n').collect();
        assert_eq!(after_lines.len(), lines.len());
        for i in [1, 3, 4, 5, 7] {
            assert_eq!(after_lines[i], lines[i]);
        }
        for i in [0, 2, 6] {
            assert_eq!(
                after_lines[i].ends_with(b"\r\n"),
                lines[i].ends_with(b"\r\n")
            );
        }
        let after_first = after_lines[0].len() as i64;
        let after_second = after_lines[..5]
            .iter()
            .map(|line| line.len() as i64)
            .sum::<i64>();
        assert_ne!(after_first, first);
        assert_ne!(after_second, second);
        let h = Connection::open(&history).unwrap();
        for (turn, start, end) in [
            ("first", after_first, after_second),
            ("second", after_second, applied[0].len() as i64),
            ("untouched", 777, 999),
        ] {
            assert_eq!(h.query_row("SELECT rollout_byte_offset,rollout_end_byte_offset FROM thread_turns WHERE turn_id=?1", [turn], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, i64>(1)?))).unwrap(), (start,end));
        }
        for (thread, offset, ordinal) in [("fixture-thread", after_second, 5), ("other", 123, 456)]
        {
            assert_eq!(h.query_row("SELECT next_rollout_byte_offset,next_rollout_ordinal FROM thread_history_projection_state WHERE thread_id=?1", [thread], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, i64>(1)?))).unwrap(), (offset,ordinal));
        }
        drop(h);
        let c = Connection::open(f.roots.sqlite.join("state_5.sqlite")).unwrap();
        assert_eq!(
            c.query_row(
                "SELECT model_provider,model FROM threads WHERE id='fixture-thread'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            )
            .unwrap(),
            (mapping().target.provider, mapping().target.model)
        );
        assert_eq!(
            c.query_row(
                "SELECT model_provider,model,rollout_path FROM threads WHERE id='other'",
                [],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?
                ))
            )
            .unwrap(),
            ("keep".into(), "keep-model".into(), "keep-path".into())
        );
        drop(c);
        assert!(source(&f.roots.home, "fixture-thread").unwrap() == mapping().target);
        let saved = operations(&f.roots.home).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].operation_id, receipt.operation_id);
        assert!(matches!(saved[0].state, OperationState::Completed));
        let bound = recovery_binding(&f.roots.home, &receipt.operation_id).unwrap();
        assert_eq!(bound.fingerprint, "synthetic-binding");

        // Reopen from the durable receipt; no Plan or in-memory snapshots are used.
        let restored =
            recover_with_idle(&f.roots.home, &receipt.operation_id, true, || Ok(())).unwrap();
        assert!(restored.restored);
        assert_eq!(f.images(), f.before);
        assert!(source(&f.roots.home, "fixture-thread").unwrap() == mapping().source);
        assert!(matches!(
            operations(&f.roots.home).unwrap()[0].state,
            OperationState::Restored
        ));
        assert!(matches!(
            recover_with_idle(&f.roots.home, &receipt.operation_id, false, || Ok(())),
            Err(RepairError::RecoveryConflict)
        ));
        assert_eq!(f.images(), f.before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let operation = f
                .roots
                .home
                .join("osir-history-repairs")
                .join(&receipt.operation_id);
            for name in [
                "journal.json",
                "0.before",
                "1.before",
                "2.before",
                "0.after",
                "1.after",
                "2.after",
            ] {
                assert_eq!(
                    fs::metadata(operation.join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o077,
                    0
                );
            }
        }
    }
    #[test]
    fn invalid_offsets_and_changed_previews_never_replace_source_images() {
        for (offset, ordinal) in [(1, 0), (0, 1)] {
            let f = Fixture::new();
            let h = Connection::open(f.roots.sqlite.join("thread_history_1.sqlite")).unwrap();
            h.execute("UPDATE thread_history_projection_state SET next_rollout_byte_offset=?1,next_rollout_ordinal=?2", params![offset,ordinal]).unwrap();
            drop(h);
            let before = f.images();
            assert!(matches!(
                preview_with_idle(&f.roots.home, mapping(), || Ok(())),
                Err(RepairError::UnsupportedSchema)
            ));
            assert_eq!(f.images(), before);
            assert!(operations(&f.roots.home).unwrap().is_empty());
        }
        for changed in ["rollout", "index", "cache", "config", "expired"] {
            let f = Fixture::new();
            let mut plan = preview_with_idle(&f.roots.home, mapping(), || Ok(())).unwrap();
            match changed {
                "rollout" => {
                    let mut bytes = f.before[0].clone();
                    bytes.extend_from_slice(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"synthetic-external-change\"}}\n");
                    fs::write(&f.path, bytes).unwrap();
                }
                "index" => {
                    let c = Connection::open(f.roots.sqlite.join("state_5.sqlite")).unwrap();
                    c.execute(
                        "UPDATE threads SET model='external-model' WHERE id='other'",
                        [],
                    )
                    .unwrap();
                }
                "cache" => {
                    let c =
                        Connection::open(f.roots.sqlite.join("thread_history_1.sqlite")).unwrap();
                    c.execute("UPDATE thread_turns SET rollout_byte_offset=1", [])
                        .unwrap();
                }
                "config" => write_new(
                    &f.roots.home.join("config.toml"),
                    b"# externally changed configuration\n",
                )
                .unwrap(),
                "expired" => {
                    plan.created = std::time::Instant::now() - std::time::Duration::from_secs(301)
                }
                _ => unreachable!(),
            }
            let before = f.images();
            assert!(matches!(
                apply_with_idle(plan, || Ok(())),
                Err(RepairError::Changed)
            ));
            assert_eq!(f.images(), before);
            assert!(operations(&f.roots.home).unwrap().is_empty());
        }
    }
    #[test]
    fn real_schema_updates_route_and_offsets_but_preserves_conversation_and_other_threads() {
        let f = Fixture::new();
        let (_, after, records, offsets) = prepare(&f.roots, &mapping(), &f.before).unwrap();
        assert_eq!(records, 2);
        assert_eq!(offsets, 2);
        let before_lines: Vec<_> = f.before[0].split_inclusive(|b| *b == b'\n').collect();
        let after_lines: Vec<_> = after[0].split_inclusive(|b| *b == b'\n').collect();
        assert_eq!(before_lines[1], after_lines[1]);
        assert_eq!(before_lines[3], after_lines[3]);
        let copy = f.roots.home.join("result.sqlite");
        write_new(&copy, &after[1]).unwrap();
        let c = db(&copy).unwrap();
        assert_eq!(
            c.query_row(
                "SELECT model_provider FROM threads WHERE id='other'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "keep"
        );
        assert_eq!(
            hash(&read(&f.path, ROLLOUT_LIMIT).unwrap()),
            hash(&f.before[0])
        );
        let second = prepare(&f.roots, &mapping(), &f.before).unwrap().1;
        assert_eq!(hashes(&after), hashes(&second)); // journal derivation is reproducible
    }
    #[test]
    fn interrupted_apply_and_restore_are_recoverable_and_external_changes_refused() {
        let f = Fixture::new();
        let after = prepare(&f.roots, &mapping(), &f.before).unwrap().1;
        let j = f.journal(&after);
        let dir = f.roots.home.join("operation");
        private_dir(&dir).unwrap();
        replace(&f.path, &after[0]).unwrap(); // interruption between JSONL and SQLite
        finish(&f.roots, &dir, &j, &f.before, &after, false, || Ok(())).unwrap();
        assert_eq!(
            hash(&read(&f.roots.home.join("state_5.sqlite"), DB_LIMIT).unwrap()),
            j.after[1]
        );
        finish(&f.roots, &dir, &j, &f.before, &after, true, || Ok(())).unwrap();
        finish(&f.roots, &dir, &j, &f.before, &after, true, || Ok(())).unwrap();
        assert_eq!(read(&f.path, ROLLOUT_LIMIT).unwrap(), f.before[0]);
        fs::write(&f.path, b"external new history\n").unwrap();
        assert!(matches!(
            finish(&f.roots, &dir, &j, &f.before, &after, true, || Ok(())),
            Err(RepairError::RecoveryConflict)
        ));
    }
    #[test]
    fn running_writer_and_unknown_route_never_modify_source() {
        let f = Fixture::new();
        let after = prepare(&f.roots, &mapping(), &f.before).unwrap().1;
        let j = f.journal(&after);
        let dir = f.roots.home.join("operation");
        private_dir(&dir).unwrap();
        assert!(matches!(
            finish(&f.roots, &dir, &j, &f.before, &after, false, || Err(
                RepairError::WritersRunning
            )),
            Err(RepairError::WritersRunning)
        ));
        assert_eq!(read(&f.path, ROLLOUT_LIMIT).unwrap(), f.before[0]);
        let mut bad = mapping();
        bad.source.provider = "unowned".into();
        assert!(matches!(
            prepare(&f.roots, &bad, &f.before),
            Err(RepairError::UnmappedRoute)
        ));
        fs::write(f.roots.home.join("state_6.sqlite"), b"unknown version").unwrap();
        assert!(matches!(
            stable(&f.roots, &None, &targets(&f.roots, &f.path, 3), &j.before),
            Err(RepairError::UnsupportedSchema)
        ));
    }
}

/// Read-only source route discovery for the selected native index entry. A
/// renderer cannot supply the source path/provider/model. Mixed historical
/// resume settings are deliberately refused rather than guessed.
pub fn source(home: &Path, thread_id: &str) -> Result<Route> {
    let _gate = OPERATION.try_lock().map_err(|_| RepairError::Busy)?;
    if thread_id.is_empty() || thread_id.len() > 256 {
        return Err(RepairError::InvalidRollout);
    }
    let home = io(home.canonicalize())?;
    owned_dir(&home)?;
    let (sqlite, cfg) = config(&home)?;
    let roots = Roots { home, sqlite };
    versions(&roots.sqlite)?;
    let state = roots.sqlite.join("state_5.sqlite");
    sidecars(&state)?;
    let state_bytes = read(&state, DB_LIMIT)?;
    let temp = io(tempfile::tempdir())?;
    let copy = io(temp.path().canonicalize())?.join("state");
    write_new(&copy, &state_bytes)?;
    let c = db(&copy)?;
    columns(
        &c,
        "threads",
        &["id", "model_provider", "model", "rollout_path"],
    )?;
    let mut stmt = sql(
        c.prepare("SELECT model_provider,model,rollout_path FROM threads WHERE id=?1 LIMIT 2")
    )?;
    let rows: Vec<(String, Option<String>, String)> = sql(sql(
        stmt.query_map([thread_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    )?
    .collect())?;
    if rows.len() != 1 {
        return Err(RepairError::UnsupportedSchema);
    }
    let path = rollout_path(&roots, Path::new(&rows[0].2))?;
    let bytes = read(&path, ROLLOUT_LIMIT)?;
    let mut provider: Option<String> = None;
    let mut model: Option<String> = None;
    for (i, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        if i >= ROW_LIMIT || line.len() > 4 * 1024 * 1024 {
            return Err(RepairError::TooLarge);
        }
        let v: Value = serde_json::from_slice(line).map_err(|_| RepairError::InvalidRollout)?;
        if i == 0 {
            if v["type"] != "session_meta" || v["payload"]["id"].as_str() != Some(thread_id) {
                return Err(RepairError::InvalidRollout);
            }
            provider = v["payload"]["model_provider"].as_str().map(str::to_owned);
            model = v["payload"]["model"].as_str().map(str::to_owned);
        } else if v["type"] == "event_msg" && v["payload"]["type"] == "thread_settings_applied" {
            if v["payload"]["thread_id"].as_str() != Some(thread_id) {
                return Err(RepairError::InvalidRollout);
            }
            let s = &v["payload"]["thread_settings"];
            if s["model_provider_id"].as_str() != provider.as_deref() {
                return Err(RepairError::UnmappedRoute);
            }
            let next = s["model"].as_str().ok_or(RepairError::UnmappedRoute)?;
            if model.as_ref().is_some_and(|old| old != next) {
                return Err(RepairError::UnmappedRoute);
            }
            model = Some(next.into());
        }
    }
    let provider = provider.ok_or(RepairError::UnmappedRoute)?;
    if model.is_none() && rows[0].0 == provider {
        model = rows[0].1.clone();
    }
    let model = model.ok_or(RepairError::UnmappedRoute)?;
    // Validate strings without requiring a fictitious target to construct a Mapping.
    for value in [&provider, &model] {
        if value.is_empty()
            || value.len() > 256
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(RepairError::UnmappedRoute);
        }
    }
    sidecars(&state)?;
    if hash(&read(&state, DB_LIMIT)?) != hash(&state_bytes)
        || hash(&read(&path, ROLLOUT_LIMIT)?) != hash(&bytes)
        || config(&roots.home)? != (roots.sqlite, cfg)
    {
        return Err(RepairError::Changed);
    }
    Ok(Route { provider, model })
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TargetBinding {
    pub provider_id: String,
    pub model_id: String,
    pub fingerprint: String,
}
/// Read-only native recovery binding, used before completing an interrupted
/// operation. Rollback does not need a currently enabled destination provider.
pub(crate) fn recovery_binding(home: &Path, id: &str) -> Result<TargetBinding> {
    if uuid::Uuid::parse_str(id)
        .map_err(|_| RepairError::UnsafePath)?
        .to_string()
        != id
    {
        return Err(RepairError::UnsafePath);
    }
    let home = io(home.canonicalize())?;
    owned_dir(&home)?;
    let root = home.join("osir-history-repairs");
    owned_dir(&root)?;
    let dir = root.join(id);
    owned_dir(&dir)?;
    let j: Journal = serde_json::from_slice(&read(&dir.join("journal.json"), 64 * 1024)?)
        .map_err(|_| RepairError::RecoveryConflict)?;
    if j.version != 1 {
        return Err(RepairError::RecoveryConflict);
    }
    j.binding.ok_or(RepairError::RecoveryConflict)
}
