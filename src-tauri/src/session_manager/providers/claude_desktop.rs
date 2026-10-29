//! Claude Desktop's durable Code/Cowork session index. Cloud chat is not read
//! from cookies/browser caches. No Desktop store is ever modified by this module.
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{claude, utils::truncate_summary};
use crate::session_manager::{sources, SessionMeta};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalRecord {
    session_id: String,
    cli_session_id: Option<String>,
    title: Option<String>,
    created_at: f64,
    last_activity_at: f64,
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

pub(crate) fn application_roots() -> Vec<PathBuf> {
    let home = sources::history_user_home();
    #[cfg(target_os = "macos")]
    let base = home.join("Library/Application Support");
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Roaming"));
    #[cfg(not(any(target_os = "macos", windows)))]
    let base = home.join(".config");
    vec![base.join("Claude"), base.join("Claude-3p")]
}

pub(crate) fn session_roots() -> Vec<PathBuf> {
    application_roots()
        .into_iter()
        .flat_map(|root| {
            [
                root.join("local-agent-mode-sessions"),
                root.join("claude-code-sessions"),
            ]
        })
        .collect()
}

/// Every account/organization is indexed before frontend search or paging.
/// Only known metadata filenames are opened, with a per-record byte bound.
pub fn scan_sessions() -> Vec<SessionMeta> {
    scan_at(
        &application_roots(),
        &sources::claude_home().join("projects"),
    )
}

fn directories(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

fn read_record(path: &Path) -> Option<LocalRecord> {
    if !fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    let record: LocalRecord = serde_json::from_slice(&bytes).ok()?;
    (safe_id(&record.session_id)
        && record.created_at.is_finite()
        && record.last_activity_at.is_finite())
    .then_some(record)
}

fn regular_under(path: &Path, root: &Path) -> Option<PathBuf> {
    // Reject links at the transcript and canonical escapes at any ancestor.
    if !fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    canonical
        .starts_with(root.canonicalize().ok()?)
        .then_some(canonical)
}

fn transcript(
    storage: &Path,
    record: &LocalRecord,
    code: bool,
    cli_projects: &Path,
) -> Option<PathBuf> {
    let cli_id = record.cli_session_id.as_deref().filter(|id| safe_id(id))?;
    let mut candidates = vec![storage.join(&record.session_id)];
    // Desktop shortens a local UUID directory to its first eight hex digits.
    if let Some(id) = record.session_id.strip_prefix("local_") {
        let parts: Vec<_> = id.split('-').collect();
        if parts.len() == 5
            && parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
            && id.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit())
        {
            candidates.push(storage.join(parts[0]));
        }
    }
    for session in candidates {
        let projects = session.join(".claude/projects");
        for project in directories(&projects) {
            if let Some(path) = regular_under(&project.join(format!("{cli_id}.jsonl")), storage) {
                return Some(path);
            }
        }
    }
    if code {
        for project in directories(cli_projects) {
            if let Some(path) =
                regular_under(&project.join(format!("{cli_id}.jsonl")), cli_projects)
            {
                return Some(path);
            }
        }
    }
    None
}

fn scan_at(applications: &[PathBuf], cli_projects: &Path) -> Vec<SessionMeta> {
    scan_at_with_preview(applications, cli_projects, true)
}

fn scan_at_with_preview(
    applications: &[PathBuf],
    cli_projects: &Path,
    preview: bool,
) -> Vec<SessionMeta> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for app in applications {
        for (tree, code) in [
            ("local-agent-mode-sessions", false),
            ("claude-code-sessions", true),
        ] {
            for account in directories(&app.join(tree)) {
                for organization in directories(&account) {
                    for storage in [organization.clone(), organization.join("agent")] {
                        let Ok(entries) = fs::read_dir(&storage) else {
                            continue;
                        };
                        for entry in entries.flatten() {
                            let name = entry.file_name();
                            let name = name.to_string_lossy();
                            if !name.starts_with("local_") || !name.ends_with(".json") {
                                continue;
                            }
                            let Some(record) = read_record(&entry.path()) else {
                                continue;
                            };
                            if name != format!("{}.json", record.session_id) {
                                continue;
                            }
                            let source = transcript(&storage, &record, code, cli_projects);
                            let key = (
                                record.session_id.clone(),
                                source.clone().unwrap_or_else(|| entry.path()),
                            );
                            if !seen.insert(key) {
                                continue;
                            }
                            let parsed = source
                                .as_deref()
                                .filter(|_| preview)
                                .and_then(|path| claude::scan_session_file(path).ok().flatten());
                            let missing = source.is_none();
                            result.push(SessionMeta {
                                provider_id: "claude-desktop".into(),
                                session_id: record.session_id,
                                title: record
                                    .title
                                    .filter(|v| !v.trim().is_empty())
                                    .map(|v| truncate_summary(&v, 160))
                                    .or_else(|| parsed.as_ref().and_then(|m| m.title.clone())),
                                summary: if missing {
                                    Some(
                                        "本机仅保存了会话索引，正文请在 Claude 应用中查看。".into(),
                                    )
                                } else {
                                    parsed.as_ref().and_then(|m| m.summary.clone())
                                },
                                project_dir: parsed.and_then(|m| m.project_dir),
                                created_at: Some(record.created_at as i64),
                                last_active_at: Some(record.last_activity_at as i64),
                                source_path: source.map(|p| p.to_string_lossy().into_owned()),
                                resume_command: None,
                            });
                        }
                    }
                }
            }
        }
    }
    result
}

/// IPC cannot turn the shared Claude projects root into unrestricted Desktop
/// history. A file must still be linked by an actual local Desktop index entry.
pub(crate) fn resolve_source(raw: &str) -> Result<(PathBuf, PathBuf), String> {
    let source = Path::new(raw)
        .canonicalize()
        .map_err(|_| "本地会话正文已不可用，请刷新后重试")?;
    let linked = scan_at_with_preview(
        &application_roots(),
        &sources::claude_home().join("projects"),
        false,
    )
    .iter()
    .any(|session| {
        session
            .source_path
            .as_deref()
            .is_some_and(|p| Path::new(p) == source)
    });
    if !linked {
        return Err("此文件不属于 Claude 桌面本地会话索引".into());
    }
    let root = source.parent().ok_or("本地会话目录不可用")?.to_path_buf();
    Ok((root, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(root: &Path, tree: &str, id: &str, cli: &str) -> PathBuf {
        let storage = root.join(tree).join("account/org");
        fs::create_dir_all(&storage).unwrap();
        fs::write(storage.join(format!("{id}.json")), json!({"sessionId":id,"cliSessionId":cli,"title":"Desktop fixture","createdAt":1000,"lastActivityAt":2000}).to_string()).unwrap();
        storage
    }

    fn transcript_fixture(path: &Path, id: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, json!({"type":"user","sessionId":id,"cwd":"/fixture/project","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"Fixture only"}}).to_string()).unwrap();
    }

    #[test]
    fn indexes_local_code_cowork_and_metadata_without_inventing_cloud_history() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Claude");
        let cli = temp.path().join("cli/projects");
        let storage = record(
            &app,
            "local-agent-mode-sessions",
            "local_cowork",
            "cowork-id",
        );
        transcript_fixture(
            &storage.join("local_cowork/.claude/projects/project/cowork-id.jsonl"),
            "cowork-id",
        );
        record(&app, "claude-code-sessions", "local_code", "linked-id");
        transcript_fixture(&cli.join("project/linked-id.jsonl"), "linked-id");
        transcript_fixture(&cli.join("project/unrelated.jsonl"), "unrelated");
        record(&app, "claude-code-sessions", "local_missing", "missing-id");
        let sessions = scan_at(&[app], &cli);
        assert_eq!(sessions.len(), 3);
        assert!(sessions
            .iter()
            .all(|s| s.provider_id == "claude-desktop" && s.resume_command.is_none()));
        assert_eq!(
            sessions.iter().filter(|s| s.source_path.is_some()).count(),
            2
        );
        assert!(sessions
            .iter()
            .find(|s| s.session_id == "local_missing")
            .unwrap()
            .summary
            .as_deref()
            .unwrap()
            .contains("仅保存了会话索引"));
        assert!(scan_at(&[temp.path().join("empty")], &cli).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_transcript_symlinks_escaping_the_desktop_store() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Claude");
        let storage = record(&app, "local-agent-mode-sessions", "local_escape", "escape");
        let outside = temp.path().join("outside.jsonl");
        transcript_fixture(&outside, "escape");
        let project = storage.join("local_escape/.claude/projects/project");
        fs::create_dir_all(&project).unwrap();
        std::os::unix::fs::symlink(outside, project.join("escape.jsonl")).unwrap();
        let sessions = scan_at(&[app], &temp.path().join("none"));
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].source_path.is_none());
    }
}
