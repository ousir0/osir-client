//! Codex's local paginated ThreadItem store (0.165 schema). Reads never start
//! app-server: its history endpoints may migrate/project data on first access.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use crate::session_manager::content::MAX_TEXT_BYTES;
use crate::session_manager::model::{
    ContentRef, DiffFile, DiffOp, EventKind, ImageRef, ImageSource, SessionBlock, SessionMessage,
    ToolStatus,
};

use super::blocks::{
    count_diff_lines, estimate_base64_size, large_text_block, refine_shell_kind, thinking_block,
    tool_call_block, tool_result_block, ToolSource,
};
use super::codex_items::{diff_summary, local_image, status_from_str, strip_file_url};

const DATABASE: &str = "thread_history_1.sqlite";
const PAGE_SIZE: i64 = 256;
const UNAVAILABLE: &str = "无法读取 Codex 本地会话库，请刷新后重试";

pub(crate) fn database_for_state(state: &Path) -> PathBuf {
    state.with_file_name(DATABASE)
}

pub(crate) fn database_paths() -> Vec<PathBuf> {
    super::codex::state_db_paths()
        .iter()
        .map(|path| database_for_state(path))
        .collect()
}

pub(crate) fn source(db: &Path, id: &str) -> String {
    format!("sqlite:{}#{id}", db.display())
}

pub(crate) fn parse_source(raw: &str) -> Option<(PathBuf, String)> {
    let (db, id) = raw.strip_prefix("sqlite:")?.rsplit_once('#')?;
    if id.is_empty() || id.len() > 160 || id.chars().any(char::is_control) {
        return None;
    }
    Some((PathBuf::from(db), id.to_string()))
}

pub(crate) fn is_desktop_source(raw: &str) -> bool {
    let Some((db, id)) = parse_source(raw) else {
        return false;
    };
    let Ok(conn) = open(&db.with_file_name(crate::codex_state_db::CODEX_STATE_DB_FILENAME)) else {
        return false;
    };
    conn.query_row(
        "SELECT source,originator FROM threads WHERE id=?1",
        [id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )
    .ok()
    .is_some_and(|(source, origin)| {
        let source = serde_json::from_str::<Value>(&source).unwrap_or(Value::String(source));
        super::codex::is_desktop_origin(origin.as_deref(), Some(&source))
    })
}

fn open(db: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| UNAVAILABLE)?;
    conn.busy_timeout(Duration::from_millis(300))
        .map_err(|_| UNAVAILABLE)?;
    Ok(conn)
}

pub(crate) fn indexed_threads(db: &Path) -> HashSet<String> {
    let Ok(conn) = open(db) else {
        return HashSet::new();
    };
    // Probe the columns needed by the reader, without reading any body.
    if conn.prepare("SELECT turn_id,item_id,rollout_ordinal,created_at_ms,item_json FROM thread_items LIMIT 0").is_err() {
        return HashSet::new();
    }
    let Ok(mut stmt) = conn.prepare("SELECT DISTINCT thread_id FROM thread_items") else {
        return HashSet::new();
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
        return HashSet::new();
    };
    rows.flatten().collect()
}

/// One SQLite snapshot across every keyset page, then the existing transcript
/// cache/IPC chunks handle delivery. No OFFSET skips, body cap or fixed row cap.
pub(crate) fn load_messages(raw: &str) -> Result<Vec<SessionMessage>, String> {
    let (db, id) = parse_source(raw).ok_or(UNAVAILABLE)?;
    let conn = open(&db)?;
    conn.execute_batch("BEGIN DEFERRED")
        .map_err(|_| UNAVAILABLE)?;
    let mut stmt = conn
        .prepare(
            "SELECT turn_id,item_id,rollout_ordinal,created_at_ms,\
         CASE WHEN length(CAST(item_json AS BLOB)) <= ?4 THEN item_json END \
         FROM thread_items WHERE thread_id=?1 AND (?2 IS NULL OR rollout_ordinal>?2) \
         ORDER BY rollout_ordinal LIMIT ?3",
        )
        .map_err(|_| UNAVAILABLE)?;
    let mut after: Option<i64> = None;
    let mut messages = Vec::new();
    loop {
        let rows = stmt
            .query_map(
                rusqlite::params![id, after, PAGE_SIZE, MAX_TEXT_BYTES],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .map_err(|_| UNAVAILABLE)?;
        let mut count = 0;
        for row in rows {
            let (turn, item_id, ordinal, ts, body) = row.map_err(|_| UNAVAILABLE)?;
            let body = body.ok_or("会话中有超过 32MB 的记录，请在原工具中查看")?;
            let item: Value = serde_json::from_str(&body)
                .map_err(|_| "会话正文格式无法识别，请在原工具中查看")?;
            let locator = Locator {
                turn: &turn,
                item: &item_id,
            };
            let mut message = item_message(&item, &locator);
            message.id = Some(item_id);
            message.turn_id = Some(turn);
            message.ts = Some(ts);
            messages.push(message);
            after = Some(ordinal);
            count += 1;
        }
        if count < PAGE_SIZE {
            break;
        }
    }
    Ok(messages)
}

struct Locator<'a> {
    turn: &'a str,
    item: &'a str,
}

impl Locator<'_> {
    fn content(&self, pointer: impl Into<String>) -> ContentRef {
        ContentRef::Sqlite {
            table: "thread_items".into(),
            id: serde_json::to_string(&(self.turn, self.item)).expect("string tuple"),
            column: "item_json".into(),
            pointer: pointer.into(),
        }
    }

    fn text(&self, value: &Value, pointer: &str) -> SessionBlock {
        let text = value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string());
        large_text_block(text, || Some(self.content(pointer)))
    }

    fn image(&self, url: &str, pointer: &str) -> Option<SessionBlock> {
        let image = if let Some(rest) = url.strip_prefix("data:") {
            let (meta, data) = rest.split_once(',')?;
            ImageRef {
                source: ImageSource::Inline {
                    content: self.content(pointer),
                },
                media_type: meta.split(';').next()?.into(),
                size: estimate_base64_size(data.len()),
                alt: None,
            }
        } else if url.starts_with("file://") || Path::new(url).is_absolute() {
            local_image(strip_file_url(url))
        } else {
            return None;
        };
        Some(SessionBlock::Image { image })
    }
}

fn string<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

fn item_message(item: &Value, at: &Locator<'_>) -> SessionMessage {
    let ty = string(item, "type");
    let mut blocks = Vec::new();
    let role = if ty == "userMessage" {
        "user"
    } else {
        "assistant"
    };
    match ty {
        "userMessage" => {
            for (i, part) in item["content"].as_array().into_iter().flatten().enumerate() {
                let pointer = format!("/content/{i}");
                match string(part, "type") {
                    "text" => blocks.push(at.text(&part["text"], &format!("{pointer}/text"))),
                    "image" | "localImage" => {
                        let field = if string(part, "type") == "localImage" {
                            "path"
                        } else {
                            "url"
                        };
                        blocks.push(
                            at.image(string(part, field), &format!("{pointer}/{field}"))
                                .unwrap_or_else(|| at.text(part, &pointer)),
                        );
                    }
                    _ => blocks.push(at.text(part, &pointer)),
                }
            }
        }
        "agentMessage" | "plan" => blocks.push(at.text(&item["text"], "/text")),
        "reasoning" => {
            for key in ["summary", "content"] {
                for (i, part) in item[key].as_array().into_iter().flatten().enumerate() {
                    if let Some(text) = part.as_str() {
                        blocks.push(thinking_block(text, None, None, || {
                            Some(at.content(format!("/{key}/{i}")))
                        }));
                    }
                }
            }
        }
        "commandExecution" => {
            let command = string(item, "command");
            let mut call = tool_call_block(
                ToolSource::Codex,
                at.item,
                "exec_command",
                &json!({"command":command,"cwd":item["cwd"]}),
                || Some(at.content("")),
            );
            if let SessionBlock::ToolCall {
                kind,
                detail,
                by_user,
                ..
            } = &mut call
            {
                *kind = refine_shell_kind(command);
                *detail = item["cwd"].as_str().map(str::to_string);
                *by_user = string(item, "source") == "userShell";
            }
            blocks.push(call);
            let status = if item["exitCode"].as_i64().is_some_and(|code| code != 0) {
                ToolStatus::Error
            } else {
                status_from_str(string(item, "status"))
            };
            let mut result =
                tool_result_block(at.item, status, string(item, "aggregatedOutput"), || {
                    Some(at.content("/aggregatedOutput"))
                });
            if let SessionBlock::ToolResult {
                exit_code,
                duration_ms,
                ..
            } = &mut result
            {
                *exit_code = item["exitCode"].as_i64().and_then(|n| n.try_into().ok());
                *duration_ms = item["durationMs"].as_u64();
            }
            blocks.push(result);
        }
        "fileChange" => {
            let mut call = tool_call_block(
                ToolSource::Codex,
                at.item,
                "FileChange",
                &item["changes"],
                || Some(at.content("/changes")),
            );
            let mut files = Vec::new();
            for change in item["changes"].as_array().into_iter().flatten() {
                let (added, removed) = count_diff_lines(string(change, "diff"));
                let op = match string(&change["kind"], "type") {
                    "add" => DiffOp::Add,
                    "delete" => DiffOp::Delete,
                    _ if change["kind"]["move_path"].is_string() => DiffOp::Rename,
                    _ => DiffOp::Update,
                };
                files.push(DiffFile {
                    path: string(change, "path").into(),
                    op,
                    added,
                    removed,
                });
            }
            if let SessionBlock::ToolCall { diff, title, .. } = &mut call {
                *title = files.first().map(|f| f.path.clone()).unwrap_or_default();
                let full = (files.len() == 1).then(|| at.content("/changes/0/diff"));
                *diff = Some(diff_summary(files, full));
            }
            blocks.push(call);
            blocks.push(tool_result_block(
                at.item,
                status_from_str(string(item, "status")),
                &item["changes"].to_string(),
                || Some(at.content("/changes")),
            ));
        }
        "mcpToolCall" | "dynamicToolCall" => {
            let name = if ty == "mcpToolCall" {
                format!("mcp__{}__{}", string(item, "server"), string(item, "tool"))
            } else {
                string(item, "tool").into()
            };
            blocks.push(tool_call_block(
                ToolSource::Codex,
                at.item,
                &name,
                &item["arguments"],
                || Some(at.content("/arguments")),
            ));
            let output_key = if ty == "mcpToolCall" {
                if item["error"].is_object() {
                    "error"
                } else {
                    "result"
                }
            } else {
                "contentItems"
            };
            let output = &item[output_key];
            let texts: Vec<&str> = output
                .get("content")
                .unwrap_or(output)
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["text"].as_str())
                .collect();
            let text = if texts.is_empty() {
                output.to_string()
            } else {
                texts.join("\n")
            };
            let mut result = tool_result_block(
                at.item,
                status_from_str(string(item, "status")),
                &text,
                || Some(at.content(format!("/{output_key}"))),
            );
            if let SessionBlock::ToolResult { duration_ms, .. } = &mut result {
                *duration_ms = item["durationMs"].as_u64();
            }
            blocks.push(result);
            // Keep structured results, image/file references and future fields
            // accessible even when the readable output contains text only.
            if !texts.is_empty()
                && (output["structuredContent"].is_object()
                    || !output["_meta"].is_null()
                    || output
                        .get("content")
                        .unwrap_or(output)
                        .as_array()
                        .is_some_and(|parts| {
                            parts.iter().any(|part| part["text"].as_str().is_none())
                        }))
            {
                blocks.push(at.text(output, &format!("/{output_key}")));
            }
        }
        "functionCallOutput" => blocks.push(at.text(&item["output"], "/output")),
        "imageView" => blocks.push(
            at.image(string(item, "path"), "/path")
                .unwrap_or_else(|| at.text(item, "")),
        ),
        "contextCompaction" => blocks.push(SessionBlock::event(EventKind::Compaction, None, None)),
        // The whole item remains readable for new/less common types; do not
        // silently discard agent activity, generated attachments or tool data.
        _ => blocks.push(at.text(item, "")),
    }
    if blocks.is_empty() {
        blocks.push(at.text(item, ""));
    }
    SessionMessage::from_blocks(role, None, blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::{
        cache,
        content::{self, SourceLocation, ValidatedSource},
    };

    fn database() -> (tempfile::TempDir, PathBuf, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(DATABASE);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(include_str!(
            "../../../../tests/fixtures/sessions/codex-paginated-schema.sql"
        ))
        .unwrap();
        (dir, db, conn)
    }

    fn insert(conn: &Connection, thread: &str, turn: &str, ordinal: i64, item: Value) {
        conn.execute("INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,created_at_ms,item_json,item_type) VALUES(?1,?2,?3,?4,1793318400000,?5,?6)",
            rusqlite::params![thread, turn, item["id"].as_str().unwrap(), ordinal, item.to_string(), item["type"].as_str().unwrap()]).unwrap();
    }

    #[test]
    fn paginated_fixture_retains_messages_tools_and_order_without_rollout() {
        if crate::config::sqlite_unsupported_in_temp_dir() {
            return;
        }
        let (_dir, db, conn) = database();
        let items: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/sessions/codex-paginated.items.json"
        ))
        .unwrap();
        for (i, item) in items.iter().enumerate().rev() {
            insert(&conn, "fixture", "turn", i as i64, item.clone());
        }
        let bytes = std::fs::read(&db).unwrap();
        let messages = load_messages(&source(&db, "fixture")).unwrap();
        assert_eq!(messages.len(), items.len());
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "OSIR_SYNTHETIC_PAGINATED_USER");
        assert_eq!(
            messages.last().unwrap().content,
            "OSIR_SYNTHETIC_PAGINATED_REPLY"
        );
        assert!(messages
            .iter()
            .all(|m| m.turn_id.as_deref() == Some("turn") && m.ts == Some(1793318400000)));
        assert!(matches!(
            messages[1].blocks[0],
            SessionBlock::Thinking { .. }
        ));
        assert!(messages[2].blocks.iter().any(|b| matches!(
            b,
            SessionBlock::ToolResult {
                exit_code: Some(0),
                duration_ms: Some(2),
                ..
            }
        )));
        assert!(messages[3].blocks.iter().any(|b| matches!(b, SessionBlock::ToolCall {diff:Some(diff), ..} if diff.added==1 && diff.removed==1)));
        assert!(messages[4].content.contains("Synthetic tool output"));
        assert_eq!(
            std::fs::read(&db).unwrap(),
            bytes,
            "reader never modifies database"
        );
        assert_eq!(indexed_threads(&db), HashSet::from(["fixture".into()]));
    }

    #[test]
    fn paginated_keyset_reads_every_page_and_refreshes_wal() {
        if crate::config::sqlite_unsupported_in_temp_dir() {
            return;
        }
        let (_dir, db, conn) = database();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        for i in 0..600 {
            insert(
                &conn,
                "fixture",
                "turn",
                i * 3,
                json!({"type":"agentMessage","id":format!("item-{i}"),"text":format!("Synthetic {i}")}),
            );
        }
        insert(
            &conn,
            "other",
            "turn",
            0,
            json!({"type":"agentMessage","id":"other","text":"Other fixture"}),
        );
        let source = ValidatedSource {
            provider_id: "codex".into(),
            raw: source(&db, "fixture"),
            location: SourceLocation::Sqlite {
                db: db.clone(),
                session_id: "fixture".into(),
            },
        };
        let before = cache::source_fingerprint(&source).unwrap();
        let messages = load_messages(&source.raw).unwrap();
        assert_eq!(messages.len(), 600);
        assert_eq!(messages[599].id.as_deref(), Some("item-599"));
        conn.execute(
            "UPDATE thread_items SET item_json=?1 WHERE thread_id='fixture' AND item_id='item-599'",
            [
                json!({"type":"agentMessage","id":"item-599","text":"Updated synthetic"})
                    .to_string(),
            ],
        )
        .unwrap();
        assert_ne!(cache::source_fingerprint(&source).unwrap(), before);
        assert_eq!(
            load_messages(&source.raw).unwrap()[599].content,
            "Updated synthetic"
        );
    }

    #[test]
    fn paginated_long_content_uses_scoped_full_refs_and_character_pages() {
        if crate::config::sqlite_unsupported_in_temp_dir() {
            return;
        }
        let (_dir, db, conn) = database();
        let long = "合成正文".repeat(3000);
        insert(
            &conn,
            "fixture",
            "turn",
            0,
            json!({"type":"agentMessage","id":"reused","text":long}),
        );
        insert(
            &conn,
            "fixture",
            "second",
            1,
            json!({"type":"agentMessage","id":"reused","text":"Second turn"}),
        );
        insert(
            &conn,
            "other",
            "turn",
            0,
            json!({"type":"agentMessage","id":"reused","text":"Other thread"}),
        );
        insert(
            &conn,
            "other",
            "turn",
            1,
            json!({"type":"agentMessage","id":"other-only","text":"Other thread only"}),
        );
        let source = ValidatedSource {
            provider_id: "codex".into(),
            raw: source(&db, "fixture"),
            location: SourceLocation::Sqlite {
                db,
                session_id: "fixture".into(),
            },
        };
        let messages = load_messages(&source.raw).unwrap();
        let SessionBlock::Text {
            full: Some(reference),
            ..
        } = &messages[0].blocks[0]
        else {
            panic!("full text reference")
        };
        let full = content::resolve_content_ref(&source, reference).unwrap();
        assert_eq!(full, long);
        let first = content::paginate(&full, Some(0), Some(7));
        let second = content::paginate(&full, first.next_offset, Some(7));
        assert_eq!(first.text.chars().count(), 7);
        assert_eq!(
            second.text,
            full.chars().skip(7).take(7).collect::<String>()
        );
        let ContentRef::Sqlite { table, column, .. } = reference else {
            unreachable!()
        };
        let forged = ContentRef::Sqlite {
            table: table.clone(),
            column: column.clone(),
            id: serde_json::to_string(&("second", "reused")).unwrap(),
            pointer: "/text".into(),
        };
        assert_eq!(
            content::resolve_content_ref(&source, &forged).unwrap(),
            "Second turn"
        );
        let foreign = ContentRef::Sqlite {
            table: table.clone(),
            column: column.clone(),
            id: serde_json::to_string(&("turn", "other-only")).unwrap(),
            pointer: "/text".into(),
        };
        assert!(content::resolve_content_ref(&source, &foreign).is_err());
    }

    #[test]
    fn paginated_unknown_items_remain_readable_and_invalid_json_fails() {
        if crate::config::sqlite_unsupported_in_temp_dir() {
            return;
        }
        let (_dir, db, conn) = database();
        insert(
            &conn,
            "fixture",
            "turn",
            0,
            json!({"type":"futureType","id":"new","payload":"Synthetic future body"}),
        );
        assert!(load_messages(&source(&db, "fixture")).unwrap()[0]
            .content
            .contains("Synthetic future body"));
        conn.execute("UPDATE thread_items SET item_json='{'", [])
            .unwrap();
        assert!(load_messages(&source(&db, "fixture")).is_err());
    }

    #[test]
    fn paginated_unknown_schema_is_unavailable_and_source_origin_is_preserved() {
        if crate::config::sqlite_unsupported_in_temp_dir() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(DATABASE);
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE thread_items(thread_id TEXT);")
            .unwrap();
        assert!(indexed_threads(&db).is_empty());
        assert!(load_messages(&source(&db, "fixture")).is_err());
        let state = dir
            .path()
            .join(crate::codex_state_db::CODEX_STATE_DB_FILENAME);
        let conn = Connection::open(&state).unwrap();
        conn.execute_batch("CREATE TABLE threads(id TEXT,source TEXT,originator TEXT); INSERT INTO threads VALUES('desktop','vscode','Codex Desktop'); INSERT INTO threads VALUES('cli','cli','Codex Desktop'); INSERT INTO threads VALUES('editor','vscode',NULL);").unwrap();
        assert!(is_desktop_source(&source(&db, "desktop")));
        assert!(!is_desktop_source(&source(&db, "cli")));
        assert!(!is_desktop_source(&source(&db, "editor")));
    }
}
