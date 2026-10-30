//! Resume targets are reconstructed from a validated local source. The
//! renderer never supplies executable arguments, project cwd or a shell command.
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::content::{self, SourceLocation};
use super::providers::{codex, codex_history};

const INVALID: &str = "无法确认准确会话，请刷新会话列表后重试";
const OTHER_HOME: &str =
    "此记录来自本机只读历史，当前工具使用隔离配置；请在原工具中续聊，避免打开错误会话";
const MISSING_CWD: &str = "原项目文件夹已不可用，请先恢复该目录后再续聊";
const METADATA_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct ResumePlan {
    pub provider_id: String,
    pub session_id: String,
    pub cwd: PathBuf,
    pub desktop_origin: bool,
}

/// IDs are individual argv values, never shell syntax or an implicit latest
/// session. Keep even the copied display command safe on every platform.
pub(crate) fn arguments(provider: &str, id: &str) -> Result<Vec<String>, String> {
    if id.is_empty()
        || id.starts_with('-')
        || id.len() > 160
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err(INVALID.into());
    }
    let flag = match provider {
        "codex" => "resume",
        "claude" | "gemini" => "--resume",
        "opencode" => "-s",
        _ => return Err("此工具尚未提供可靠的会话续聊方式，请在原工具中继续".into()),
    };
    Ok(vec![flag.into(), id.into()])
}

fn project_directory(raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw);
    if !path.is_absolute() || raw.chars().any(char::is_control) || !path.is_dir() {
        return Err(MISSING_CWD.into());
    }
    path.canonicalize().map_err(|_| MISSING_CWD.into())
}

fn same_root(source: &Path, configured: &Path) -> bool {
    matches!((source.canonicalize(), configured.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

fn current_codex_state_db() -> Result<PathBuf, String> {
    if crate::distribution::is_isolated() {
        return Err(OTHER_HOME.into());
    }
    ensure_current_home("codex")?;
    let home = crate::codex_config::get_codex_config_dir();
    let config = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(config) => config,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err(INVALID.into()),
    };
    config
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| INVALID)?;
    // Discovery includes the old home DB too; a deeplink can only select the
    // currently configured DB. The shared resolver puts the active override last.
    crate::codex_state_db::codex_state_db_paths(&home, &config)
        .into_iter()
        .last()
        .ok_or_else(|| INVALID.into())
}

fn codex_paginated_metadata(
    db: &Path,
    source_id: &str,
    requested_id: &str,
    cli: bool,
    active_state: &Path,
) -> Result<(String, String, bool), String> {
    // The official Desktop URI parser accepts hyphenated UUIDs, not arbitrary
    // CLI IDs or the special "new" route. Never reconstruct a rollout for CLI.
    if cli
        || source_id != requested_id
        || !uuid::Uuid::parse_str(requested_id).ok().is_some_and(|id| {
            id.hyphenated()
                .to_string()
                .eq_ignore_ascii_case(requested_id)
        })
    {
        return Err(INVALID.into());
    }
    if !same_root(db, &codex_history::database_for_state(active_state))
        || !matches!((db.canonicalize(), active_state.canonicalize()), (Ok(history), Ok(state)) if history.parent() == state.parent())
        || !same_root(
            &db.with_file_name(crate::codex_state_db::CODEX_STATE_DB_FILENAME),
            active_state,
        )
    {
        return Err(OTHER_HOME.into());
    }
    let open = |path: &Path| {
        let conn =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| INVALID)?;
        conn.busy_timeout(std::time::Duration::from_millis(300))
            .map_err(|_| INVALID)?;
        Ok::<_, String>(conn)
    };
    let state = open(active_state)?;
    let (id, cwd, source, origin, mode): (String, String, String, Option<String>, String) = state
        .query_row(
            "SELECT id,cwd,source,originator,history_mode FROM threads WHERE id=?1",
            [source_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|_| INVALID)?;
    let source = serde_json::from_str::<serde_json::Value>(&source)
        .unwrap_or(serde_json::Value::String(source));
    if id != requested_id
        || mode != "paginated"
        || !codex::is_desktop_origin(origin.as_deref(), Some(&source))
    {
        return Err(INVALID.into());
    }
    let history = open(db)?;
    history
        .prepare("SELECT turn_id,item_id,rollout_ordinal,created_at_ms,item_json FROM thread_items LIMIT 0")
        .map_err(|_| INVALID)?;
    let has_body: bool = history
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_items WHERE thread_id=?1)",
            [source_id],
            |row| row.get(0),
        )
        .map_err(|_| INVALID)?;
    if !has_body {
        return Err(INVALID.into());
    }
    Ok((id, cwd, true))
}

fn ensure_current_home(provider: &str) -> Result<(), String> {
    let matches = match provider {
        "codex" => same_root(
            &super::sources::codex_home(),
            &crate::codex_config::get_codex_config_dir(),
        ),
        "claude" => same_root(
            &super::sources::claude_home(),
            &crate::config::get_claude_config_dir(),
        ),
        "gemini" => same_root(
            &super::sources::gemini_home(),
            &crate::gemini_config::get_gemini_dir(),
        ),
        "opencode" => !super::sources::is_read_only(provider),
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(OTHER_HOME.into())
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Header {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    id: Option<String>,
    cwd: Option<String>,
    directory: Option<String>,
    #[serde(rename = "relocatedCwd")]
    relocated_cwd: Option<String>,
    payload: Payload,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Payload {
    id: Option<String>,
    cwd: Option<String>,
    originator: Option<String>,
    source: serde_json::Value,
}

fn json_lines_header(provider: &str, path: &Path) -> Result<(String, String, bool), String> {
    let reader: Box<dyn BufRead + Send> = if provider == "codex" {
        crate::codex_rollout_file::open_reader(path).map_err(|_| INVALID)?
    } else {
        Box::new(BufReader::new(File::open(path).map_err(|_| INVALID)?))
    };
    let mut reader = BufReader::new(reader.take(METADATA_BYTES));
    let mut id = None;
    let mut cwd = None;
    let mut desktop = false;
    for _ in 0..100 {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|_| INVALID)? == 0 {
            break;
        }
        let Ok(header) = serde_json::from_str::<Header>(&line) else {
            continue;
        };
        if provider == "codex" {
            if header.kind != "session_meta" {
                continue;
            }
            if header.payload.source.get("subagent").is_some() {
                return Err(INVALID.into());
            }
            desktop = codex::is_desktop_origin(
                header.payload.originator.as_deref(),
                Some(&header.payload.source),
            );
            id = header.payload.id;
            cwd = header.payload.cwd;
        } else {
            id = id.or(header.session_id);
            cwd = cwd.or(header.cwd);
        }
        if id.is_some() && cwd.is_some() {
            break;
        }
    }
    if provider == "claude" {
        // Match Claude's explicit /cd metadata without loading message bodies.
        let mut file = File::open(path).map_err(|_| INVALID)?;
        let len = file.metadata().map_err(|_| INVALID)?.len();
        let start = len.saturating_sub(64 * 1024);
        file.seek(SeekFrom::Start(start)).map_err(|_| INVALID)?;
        let mut tail = Vec::new();
        file.take(64 * 1024)
            .read_to_end(&mut tail)
            .map_err(|_| INVALID)?;
        let tail = String::from_utf8_lossy(&tail);
        let lines: Vec<_> = tail.lines().skip(usize::from(start > 0)).collect();
        if let Some(relocated) = lines
            .into_iter()
            .rev()
            .filter_map(|line| serde_json::from_str::<Header>(line).ok())
            .filter(|header| header.kind == "relocated")
            .find_map(|header| header.relocated_cwd)
        {
            cwd = Some(relocated);
        }
    }
    Ok((id.ok_or(INVALID)?, cwd.ok_or(MISSING_CWD)?, desktop))
}

fn json_header(path: &Path) -> Result<Header, String> {
    let file = File::open(path).map_err(|_| INVALID)?;
    if !file.metadata().map_err(|_| INVALID)?.is_file() {
        return Err(INVALID.into());
    }
    serde_json::from_reader(file.take(METADATA_BYTES)).map_err(|_| INVALID.into())
}

fn gemini_header(path: &Path) -> Result<Header, String> {
    let file = File::open(path).map_err(|_| INVALID)?;
    // Legacy Gemini JSON contains the entire transcript. Deserialize only the
    // header fields and stream past ignored messages; a byte cap would truncate
    // valid long sessions even when their metadata is at the start of the file.
    serde_json::from_reader(BufReader::new(file)).map_err(|_| INVALID.into())
}

fn opencode_metadata(source: &SourceLocation) -> Result<(String, String), String> {
    match source {
        SourceLocation::Sqlite { db, session_id } => {
            let conn = rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|_| INVALID)?;
            conn.busy_timeout(std::time::Duration::from_millis(300))
                .map_err(|_| INVALID)?;
            for sql in [
                "SELECT id, directory FROM session_v2 WHERE id=?1",
                "SELECT id, directory FROM session WHERE id=?1",
            ] {
                if let Ok(mut stmt) = conn.prepare(sql) {
                    return stmt
                        .query_row([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
                        .map_err(|_| INVALID.into());
                }
            }
            Err(INVALID.into())
        }
        SourceLocation::Path { path, root } => {
            let id = path.file_name().and_then(|v| v.to_str()).ok_or(INVALID)?;
            let directory = root.join("session");
            let mut stack = vec![directory];
            let mut visited = 0usize;
            while let Some(current) = stack.pop() {
                if visited >= 4096 {
                    break;
                }
                visited += 1;
                let entries = std::fs::read_dir(&current).map_err(|_| INVALID)?;
                for entry in entries.filter_map(Result::ok) {
                    let candidate = entry.path();
                    if candidate.is_dir() {
                        stack.push(candidate);
                        continue;
                    }
                    if candidate.file_name().and_then(|value| value.to_str())
                        != Some(&format!("{id}.json"))
                    {
                        continue;
                    }
                    let candidate = candidate.canonicalize().map_err(|_| INVALID)?;
                    if !candidate.starts_with(root) {
                        return Err(INVALID.into());
                    }
                    let header = json_header(&candidate)?;
                    return Ok((
                        header.id.ok_or(INVALID)?,
                        header.directory.ok_or(MISSING_CWD)?,
                    ));
                }
            }
            Err(INVALID.into())
        }
    }
}

pub(crate) fn prepare(
    provider: &str,
    id: &str,
    source_path: &str,
    cli: bool,
) -> Result<ResumePlan, String> {
    arguments(provider, id)?;
    let source = content::validate_source(provider, source_path).map_err(|_| INVALID)?;
    let (found_id, cwd, desktop_origin) = match (provider, &source.location) {
        ("codex", SourceLocation::Sqlite { db, session_id }) => {
            codex_paginated_metadata(db, session_id, id, cli, &current_codex_state_db()?)?
        }
        ("codex" | "claude", SourceLocation::Path { path, .. }) => {
            json_lines_header(provider, path)?
        }
        ("gemini", SourceLocation::Path { path, root }) => {
            let header = if path.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                let reader =
                    BufReader::new(File::open(path).map_err(|_| INVALID)?.take(METADATA_BYTES));
                reader
                    .lines()
                    .take(100)
                    .filter_map(Result::ok)
                    .filter_map(|line| serde_json::from_str::<Header>(&line).ok())
                    .find(|header| header.session_id.is_some())
                    .ok_or(INVALID)?
            } else {
                gemini_header(path)?
            };
            let project = path.parent().and_then(Path::parent).ok_or(INVALID)?;
            let project_file = project
                .join(".project_root")
                .canonicalize()
                .map_err(|_| MISSING_CWD)?;
            if !project_file.starts_with(root) {
                return Err(INVALID.into());
            }
            let mut cwd = String::new();
            File::open(project_file)
                .map_err(|_| MISSING_CWD)?
                .take(16 * 1024)
                .read_to_string(&mut cwd)
                .map_err(|_| MISSING_CWD)?;
            (header.session_id.ok_or(INVALID)?, cwd.trim().into(), false)
        }
        ("opencode", _) => {
            let (id, cwd) = opencode_metadata(&source.location)?;
            (id, cwd, false)
        }
        _ => return Err(INVALID.into()),
    };
    if found_id != id {
        return Err(INVALID.into());
    }
    if cli {
        ensure_current_home(provider)?;
        if desktop_origin {
            return Err("这是桌面客户端记录，请在原桌面客户端中继续该会话".into());
        }
    }
    Ok(ResumePlan {
        provider_id: provider.into(),
        session_id: id.into(),
        cwd: project_directory(&cwd)?,
        desktop_origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGINATED_ID: &str = "11111111-1111-4111-8111-111111111111";

    fn paginated_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let state = root
            .path()
            .join(crate::codex_state_db::CODEX_STATE_DB_FILENAME);
        let db = codex_history::database_for_state(&state);
        let conn = rusqlite::Connection::open(&state).unwrap();
        conn.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY,cwd TEXT,source TEXT,originator TEXT,history_mode TEXT);").unwrap();
        conn.execute(
            "INSERT INTO threads VALUES(?1,?2,'vscode','Codex Desktop','paginated')",
            rusqlite::params![PAGINATED_ID, root.path().to_str().unwrap()],
        )
        .unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(include_str!(
            "../../../tests/fixtures/sessions/codex-paginated-schema.sql"
        ))
        .unwrap();
        conn.execute("INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,created_at_ms,item_json) VALUES(?1,'turn','item',1,1,'{\"type\":\"agentMessage\",\"text\":\"synthetic\"}')", [PAGINATED_ID]).unwrap();
        (root, db, state)
    }

    #[test]
    fn paginated_desktop_deeplink_binds_active_database_id_and_cwd_read_only() {
        let (root, db, state) = paginated_fixture();
        let before = [std::fs::read(&db).unwrap(), std::fs::read(&state).unwrap()];
        let (id, cwd, desktop) =
            codex_paginated_metadata(&db, PAGINATED_ID, PAGINATED_ID, false, &state).unwrap();
        assert_eq!(id, PAGINATED_ID);
        assert_eq!(
            project_directory(&cwd).unwrap(),
            root.path().canonicalize().unwrap()
        );
        assert!(desktop);
        assert_eq!(
            before,
            [std::fs::read(&db).unwrap(), std::fs::read(&state).unwrap()]
        );
        assert!(!root.path().join("sessions").exists());
    }

    #[test]
    fn paginated_desktop_deeplink_rejects_cli_mismatch_non_uuid_and_missing_body() {
        let (_root, db, state) = paginated_fixture();
        assert!(codex_paginated_metadata(&db, PAGINATED_ID, PAGINATED_ID, true, &state).is_err());
        assert!(codex_paginated_metadata(
            &db,
            PAGINATED_ID,
            "22222222-2222-4222-8222-222222222222",
            false,
            &state
        )
        .is_err());
        for id in ["new", "fixture-123", "11111111111141118111111111111111"] {
            assert!(codex_paginated_metadata(&db, id, id, false, &state).is_err());
        }
        let history = rusqlite::Connection::open(&db).unwrap();
        history
            .execute("UPDATE thread_items SET thread_id='other'", [])
            .unwrap();
        assert!(codex_paginated_metadata(&db, PAGINATED_ID, PAGINATED_ID, false, &state).is_err());
    }

    #[test]
    fn paginated_desktop_deeplink_requires_explicit_origin_and_paginated_state() {
        let (_root, db, state) = paginated_fixture();
        let conn = rusqlite::Connection::open(&state).unwrap();
        for (source, origin, mode) in [
            ("cli", Some("Codex Desktop"), "paginated"),
            ("exec", Some("Codex Desktop"), "paginated"),
            ("vscode", None, "paginated"),
            (
                "{\"subAgent\":\"review\"}",
                Some("Codex Desktop"),
                "paginated",
            ),
            ("desktop", None, "rollout"),
        ] {
            conn.execute(
                "UPDATE threads SET source=?1,originator=?2,history_mode=?3",
                rusqlite::params![source, origin, mode],
            )
            .unwrap();
            assert!(
                codex_paginated_metadata(&db, PAGINATED_ID, PAGINATED_ID, false, &state).is_err()
            );
        }
        conn.execute("DELETE FROM threads", []).unwrap();
        assert!(codex_paginated_metadata(&db, PAGINATED_ID, PAGINATED_ID, false, &state).is_err());
    }

    #[test]
    fn paginated_desktop_deeplink_rejects_stale_roots_and_mismatched_sibling_database() {
        let (_root, db, state) = paginated_fixture();
        let (_other, other_db, other_state) = paginated_fixture();
        assert!(
            codex_paginated_metadata(&db, PAGINATED_ID, PAGINATED_ID, false, &other_state).is_err()
        );
        assert!(
            codex_paginated_metadata(&other_db, PAGINATED_ID, PAGINATED_ID, false, &state).is_err()
        );
        #[cfg(unix)]
        {
            std::fs::remove_file(&db).unwrap();
            std::os::unix::fs::symlink(&other_db, &db).unwrap();
            assert!(
                codex_paginated_metadata(&db, PAGINATED_ID, PAGINATED_ID, false, &state).is_err()
            );
        }
    }

    #[test]
    fn argv_never_accepts_shell_syntax_or_latest_session() {
        for provider in ["codex", "claude", "opencode", "gemini"] {
            let args = arguments(provider, "fixture-123").unwrap();
            assert_eq!(args[1], "fixture-123");
            for id in ["", "--last", "a b", "a;touch bad", "$(bad)", "a\nb"] {
                assert!(arguments(provider, id).is_err(), "{provider}/{id}");
            }
        }
        assert!(arguments("/bin/sh", "fixture").is_err());
    }

    #[test]
    fn bounded_metadata_binds_id_cwd_and_claude_relocation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.jsonl");
        std::fs::write(&path, concat!(
            "{\"sessionId\":\"fixture-123\",\"cwd\":\"/fixture/old\",\"message\":{\"content\":\"synthetic body ignored\"}}\n",
            "{\"type\":\"relocated\",\"relocatedCwd\":\"/fixture/new\"}\n"
        )).unwrap();
        assert_eq!(
            json_lines_header("claude", &path).unwrap(),
            ("fixture-123".into(), "/fixture/new".into(), false)
        );
        std::fs::write(&path, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"fixture-456\",\"cwd\":\"/fixture/project\",\"source\":\"desktop\"}}\n").unwrap();
        assert_eq!(
            json_lines_header("codex", &path).unwrap(),
            ("fixture-456".into(), "/fixture/project".into(), true)
        );
    }

    #[test]
    fn codex_resume_uses_the_same_origin_rules_as_discovery() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.jsonl");
        for (source, desktop) in [
            (serde_json::json!("vscode"), true),
            (serde_json::json!("desktop"), true),
            (serde_json::json!("cli"), false),
            (serde_json::json!("exec"), false),
        ] {
            let payload = serde_json::json!({
                "id": "fixture-123", "cwd": "/fixture/project",
                "originator": "Codex Desktop", "source": source
            });
            std::fs::write(
                &path,
                format!(
                    "{}\n",
                    serde_json::json!({
                        "type": "session_meta", "payload": payload
                    })
                ),
            )
            .unwrap();
            assert_eq!(codex::is_desktop_source(&path), desktop);
            assert_eq!(json_lines_header("codex", &path).unwrap().2, desktop);
        }
        std::fs::write(
            &path,
            serde_json::json!({
                "type": "session_meta", "payload": {
                    "id": "fixture-123", "cwd": "/fixture/project",
                    "source": {"subagent": "review"}
                }
            })
            .to_string(),
        )
        .unwrap();
        assert!(json_lines_header("codex", &path).is_err());
    }

    #[test]
    fn gemini_legacy_header_streams_large_transcripts_without_truncation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.json");
        // Keep the ID after messages to verify field order does not matter.
        let record = serde_json::json!({
            "messages": [{"type": "user", "content": "x".repeat(METADATA_BYTES as usize + 1)}],
            "sessionId": "fixture-123"
        });
        std::fs::write(&path, record.to_string()).unwrap();
        assert_eq!(
            gemini_header(&path).unwrap().session_id.as_deref(),
            Some("fixture-123")
        );
        std::fs::write(&path, "{\"sessionId\":\"fixture-123\",\"messages\":[").unwrap();
        assert!(gemini_header(&path).is_err());
    }

    #[test]
    fn project_directory_and_home_cannot_silently_fall_back() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        assert!(same_root(root.path(), root.path()));
        assert!(!same_root(root.path(), other.path()));
        assert!(project_directory(root.path().to_str().unwrap()).is_ok());
        assert!(project_directory("relative").is_err());
        assert!(project_directory(root.path().join("missing").to_str().unwrap()).is_err());
    }
}
