//! Explicit offline JSONL fixture repair, NOT a real Codex-home API.
//! No home discovery, scanning, default-route inference or automatic recovery.
//! SQLite indexes, offset caches and concurrent writers are unsupported.

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

const MARKER: &str = ".osir-isolated-history-v1";
const MARKER_BYTES: &[u8] = b"OSIR isolated JSONL fixture v1\n";
const ROLLOUT: &str = "session.jsonl";
const MAX_BYTES: u64 = 8 * 1024 * 1024;
static OPERATION: Mutex<()> = Mutex::new(());

fn fail() -> AppError {
    AppError::Config("History repair refused: unsupported fixture, stale preview, or recovery conflict; retained backups must be inspected".into())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Exact user-confirmed mapping. Never fall back to a default model/key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteMapping {
    pub thread_id: String,
    pub source_provider: String,
    pub source_model: String,
    pub target_provider: String,
    pub target_model: String,
}

/// Native-owned plan: intentionally not deserializable from renderer input.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairPreview {
    root: PathBuf,
    mapping: RouteMapping,
    before_hash: String,
    after_hash: String,
    changed_lines: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    version: u32,
    mapping: RouteMapping,
    before_hash: String,
    after_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairReceipt {
    pub operation_id: String,
    pub changed_lines: usize,
}

// Reject symlink components. macOS /var aliases must be canonicalized by callers.
fn isolated_path(root: &Path) -> Result<PathBuf, AppError> {
    if !cfg!(unix) || !root.is_absolute() {
        return Err(fail());
    }
    let mut part = PathBuf::new();
    for component in root.components() {
        if matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        ) {
            return Err(fail());
        }
        part.push(component.as_os_str());
        if fs::symlink_metadata(&part)
            .map_err(|_| fail())?
            .file_type()
            .is_symlink()
        {
            return Err(fail());
        }
    }
    let canonical = root.canonicalize().map_err(|_| fail())?;
    let temporary = std::env::temp_dir().canonicalize().map_err(|_| fail())?;
    if canonical == temporary || !canonical.starts_with(&temporary) || !canonical.is_dir() {
        return Err(fail());
    }
    Ok(canonical)
}

/// Explicit opt-in, only for a pre-created EMPTY directory under canonical temp.
/// Never call on an existing history directory or expose as automatic repair.
pub fn initialize_isolated_root(root: &Path) -> Result<(), AppError> {
    let _guard = OPERATION.lock().map_err(|_| fail())?;
    let root = isolated_path(root)?;
    if fs::read_dir(&root).map_err(|_| fail())?.next().is_some() {
        return Err(fail());
    }
    write_new(&root.join(MARKER), MARKER_BYTES)
}

fn read_regular(path: &Path, max: u64) -> Result<Vec<u8>, AppError> {
    let meta = fs::symlink_metadata(path).map_err(|_| fail())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max {
        return Err(fail());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(fail());
        }
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| fail())?
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail())?;
    if bytes.len() as u64 > max {
        return Err(fail());
    }
    Ok(bytes)
}

fn validate_root(root: &Path) -> Result<PathBuf, AppError> {
    let root = isolated_path(root)?;
    if read_regular(&root.join(MARKER), 128)? != MARKER_BYTES {
        return Err(fail());
    }
    for entry in fs::read_dir(&root).map_err(|_| fail())? {
        let entry = entry.map_err(|_| fail())?;
        let name = entry.file_name();
        if name != MARKER && name != ROLLOUT && name != "repairs" {
            return Err(fail());
        }
        if entry.file_type().map_err(|_| fail())?.is_symlink() {
            return Err(fail());
        }
    }
    Ok(root)
}

fn transform(bytes: &[u8], mapping: &RouteMapping) -> Result<(Vec<u8>, usize), AppError> {
    if [
        &mapping.thread_id,
        &mapping.source_provider,
        &mapping.source_model,
        &mapping.target_provider,
        &mapping.target_model,
    ]
    .iter()
    .any(|s| s.trim().is_empty() || s.chars().any(char::is_control))
    {
        return Err(fail());
    }
    let mut output = Vec::new();
    let mut changed = 0;
    let mut sessions = 0;
    let mut model_verified = false;
    for (index, line) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        let mut value: Value = serde_json::from_slice(line).map_err(|_| fail())?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(fail)?
            .to_owned();
        let original = value.clone();
        let payload = value
            .get_mut("payload")
            .and_then(Value::as_object_mut)
            .ok_or_else(fail)?;
        match kind.as_str() {
            "session_meta" => {
                sessions += 1;
                if index != 0
                    || payload.get("id").and_then(Value::as_str) != Some(mapping.thread_id.as_str())
                    || payload.get("model_provider").and_then(Value::as_str)
                        != Some(mapping.source_provider.as_str())
                {
                    return Err(fail());
                }
                payload.insert(
                    "model_provider".into(),
                    mapping.target_provider.clone().into(),
                );
                if let Some(model) = payload.get_mut("model") {
                    if model.as_str() != Some(mapping.source_model.as_str()) {
                        return Err(fail());
                    }
                    *model = mapping.target_model.clone().into();
                    model_verified = true;
                }
            }
            "event_msg"
                if payload.get("type").and_then(Value::as_str)
                    == Some("thread_settings_applied") =>
            {
                if payload.get("thread_id").and_then(Value::as_str)
                    != Some(mapping.thread_id.as_str())
                {
                    return Err(fail());
                }
                let settings = payload
                    .get_mut("thread_settings")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(fail)?;
                if settings.get("model_provider_id").and_then(Value::as_str)
                    != Some(mapping.source_provider.as_str())
                    || settings.get("model").and_then(Value::as_str)
                        != Some(mapping.source_model.as_str())
                {
                    return Err(fail());
                }
                settings.insert(
                    "model_provider_id".into(),
                    mapping.target_provider.clone().into(),
                );
                settings.insert("model".into(), mapping.target_model.clone().into());
                model_verified = true;
            }
            "response_item" => {}
            "event_msg"
                if matches!(
                    payload.get("type").and_then(Value::as_str),
                    Some(
                        "user_message"
                            | "agent_message"
                            | "token_count"
                            | "task_started"
                            | "task_complete"
                    )
                ) => {}
            _ => return Err(fail()),
        }
        if value == original {
            output.extend_from_slice(line);
        } else {
            serde_json::to_writer(&mut output, &value).map_err(|_| fail())?;
            if line.ends_with(b"\r\n") {
                output.extend_from_slice(b"\r\n");
            } else if line.ends_with(b"\n") {
                output.push(b'\n');
            }
            changed += 1;
        }
    }
    if sessions != 1 || !model_verified || changed == 0 {
        return Err(fail());
    }
    Ok((output, changed))
}

pub fn preview(root: &Path, mapping: RouteMapping) -> Result<RepairPreview, AppError> {
    let _guard = OPERATION.lock().map_err(|_| fail())?;
    let root = validate_root(root)?;
    let before = read_regular(&root.join(ROLLOUT), MAX_BYTES)?;
    let (after, changed_lines) = transform(&before, &mapping)?;
    Ok(RepairPreview {
        root,
        mapping,
        before_hash: digest(&before),
        after_hash: digest(&after),
        changed_lines,
    })
}

fn sync_dir(path: &Path) -> Result<(), AppError> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| fail())
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| fail())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| fail())?;
    sync_dir(path.parent().ok_or_else(fail)?)
}
fn replace(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    crate::config::stage_write(path, bytes, Some(0o600), true)
        .map_err(|_| fail())?
        .commit()
        .map_err(|_| fail())?;
    sync_dir(path.parent().ok_or_else(fail)?)
}
fn repairs_dir(root: &Path) -> Result<PathBuf, AppError> {
    let path = root.join("repairs");
    if !path.exists() {
        fs::create_dir(&path).map_err(|_| fail())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|_| fail())?;
        }
        sync_dir(root)?;
    }
    if !fs::symlink_metadata(&path).map_err(|_| fail())?.is_dir() {
        return Err(fail());
    }
    Ok(path)
}

/// Revalidate the native plan; persist backup and journal BEFORE replacement.
pub fn apply(plan: &RepairPreview) -> Result<RepairReceipt, AppError> {
    let _guard = OPERATION.lock().map_err(|_| fail())?;
    let root = validate_root(&plan.root)?;
    let path = root.join(ROLLOUT);
    let before = read_regular(&path, MAX_BYTES)?;
    if digest(&before) != plan.before_hash {
        return Err(fail());
    }
    let (after, changed_lines) = transform(&before, &plan.mapping)?;
    if digest(&after) != plan.after_hash {
        return Err(fail());
    }
    let repairs = repairs_dir(&root)?;
    let operation_id = uuid::Uuid::new_v4().to_string();
    let operation = repairs.join(&operation_id);
    fs::create_dir(&operation).map_err(|_| fail())?;
    sync_dir(&repairs)?;
    write_new(&operation.join("before.jsonl"), &before)?;
    let journal = Journal {
        version: 1,
        mapping: plan.mapping.clone(),
        before_hash: plan.before_hash.clone(),
        after_hash: plan.after_hash.clone(),
    };
    write_new(
        &operation.join("journal.json"),
        &serde_json::to_vec(&journal).map_err(|_| fail())?,
    )?;
    if digest(&read_regular(&path, MAX_BYTES)?) != plan.before_hash {
        return Err(fail());
    }
    replace(&path, &after)?;
    if digest(&read_regular(&path, MAX_BYTES)?) != plan.after_hash {
        return Err(fail());
    }
    Ok(RepairReceipt {
        operation_id,
        changed_lines,
    })
}

/// Explicit rollback; refuse external changes/corrupt backups. Idempotent.
/// Journal is sufficient even if apply was interrupted after replacement.
pub fn restore(root: &Path, operation_id: &str) -> Result<(), AppError> {
    let _guard = OPERATION.lock().map_err(|_| fail())?;
    let root = validate_root(root)?;
    let id = uuid::Uuid::parse_str(operation_id).map_err(|_| fail())?;
    if id.to_string() != operation_id {
        return Err(fail());
    }
    let operation = repairs_dir(&root)?.join(operation_id);
    if fs::symlink_metadata(&operation)
        .map_err(|_| fail())?
        .file_type()
        .is_symlink()
    {
        return Err(fail());
    }
    let journal: Journal =
        serde_json::from_slice(&read_regular(&operation.join("journal.json"), 64 * 1024)?)
            .map_err(|_| fail())?;
    let before = read_regular(&operation.join("before.jsonl"), MAX_BYTES)?;
    if journal.version != 1 || digest(&before) != journal.before_hash {
        return Err(fail());
    }
    let (after, _) = transform(&before, &journal.mapping)?;
    if digest(&after) != journal.after_hash {
        return Err(fail());
    }
    let path = root.join(ROLLOUT);
    let current = digest(&read_regular(&path, MAX_BYTES)?);
    if current == journal.before_hash {
        return Ok(());
    }
    if current != journal.after_hash {
        return Err(fail());
    }
    replace(&path, &before)?;
    if digest(&read_regular(&path, MAX_BYTES)?) != journal.before_hash {
        return Err(fail());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, RouteMapping) {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().canonicalize().unwrap().join("fixture");
        fs::create_dir(&root).unwrap();
        initialize_isolated_root(&root).unwrap();
        fs::write(root.join(ROLLOUT), [
            serde_json::to_vec(&json!({"type":"session_meta","payload":{"id":"thread-1","model_provider":"old","model":"old-model"}})).unwrap(),
            b"\n".to_vec(),
            serde_json::to_vec(&json!({"type":"event_msg","payload":{"type":"thread_settings_applied","thread_id":"thread-1","thread_settings":{"model_provider_id":"old","model":"old-model"}}})).unwrap(),
            b"\n".to_vec(),
            serde_json::to_vec(&json!({"type":"response_item","payload":{"text":"keep"}})).unwrap(),
            b"\n".to_vec(),
        ].concat()).unwrap();
        (
            parent,
            RouteMapping {
                thread_id: "thread-1".into(),
                source_provider: "old".into(),
                source_model: "old-model".into(),
                target_provider: "new".into(),
                target_model: "new-model".into(),
            },
        )
    }

    #[test]
    fn preview_apply_and_restore_are_explicit_and_atomic() {
        let (parent, mapping) = fixture();
        let root = parent.path().canonicalize().unwrap().join("fixture");
        let before = fs::read(root.join(ROLLOUT)).unwrap();
        let plan = preview(&root, mapping).unwrap();
        assert_eq!(plan.changed_lines, 2);
        let receipt = apply(&plan).unwrap();
        let applied = fs::read(root.join(ROLLOUT)).unwrap();
        assert_ne!(applied, before);
        assert!(String::from_utf8_lossy(&applied).contains("new-model"));
        restore(&root, &receipt.operation_id).unwrap();
        assert_eq!(fs::read(root.join(ROLLOUT)).unwrap(), before);
    }

    #[test]
    fn stale_preview_and_unknown_shapes_are_refused_without_mutation() {
        let (parent, mapping) = fixture();
        let root = parent.path().canonicalize().unwrap().join("fixture");
        let plan = preview(&root, mapping).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(root.join(ROLLOUT))
            .unwrap()
            .write_all(b" ")
            .unwrap();
        let unchanged = fs::read(root.join(ROLLOUT)).unwrap();
        assert!(apply(&plan).is_err());
        assert_eq!(fs::read(root.join(ROLLOUT)).unwrap(), unchanged);
        fs::write(
            root.join(ROLLOUT),
            b"{\"type\":\"unknown\",\"payload\":{}}\n",
        )
        .unwrap();
        assert!(preview(
            &root,
            RouteMapping {
                thread_id: "thread-1".into(),
                source_provider: "old".into(),
                source_model: "old-model".into(),
                target_provider: "new".into(),
                target_model: "new-model".into()
            }
        )
        .is_err());
    }

    #[test]
    fn non_fixture_and_symlink_inputs_are_rejected() {
        let outside = tempfile::tempdir().unwrap();
        assert!(preview(
            outside.path(),
            RouteMapping {
                thread_id: "x".into(),
                source_provider: "a".into(),
                source_model: "b".into(),
                target_provider: "c".into(),
                target_model: "d".into()
            }
        )
        .is_err());
    }
}
