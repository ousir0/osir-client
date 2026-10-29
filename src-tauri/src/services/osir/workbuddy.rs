//! WorkBuddy 5.5.6 DesktopModelsRepo / ModelsJsonStorage compatibility.
//! No app execution or account credential is exposed by this module's DTOs.
use super::key_vault::{NativeKeySecret, GATEWAY_ORIGIN};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant, SystemTime},
};

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const READ_ERROR: &str = "无法读取 WorkBuddy 模型配置，请检查文件权限与格式";
const WRITE_ERROR: &str = "WorkBuddy 配置未保存，请检查文件权限后重试";
const CONFLICT: &str = "WorkBuddy 配置已改变，请重新读取后保存";
const OWNER: &str = "_osirClient";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveRequest {
    pub generation: u64,
    pub key_id: i64,
    pub model_ids: Vec<String>,
    pub expected_revision: String,
    pub supports_tool_call: bool,
    pub supports_images: bool,
    pub supports_reasoning: bool,
}
impl SaveRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.key_id <= 0
            || self.model_ids.is_empty()
            || self.model_ids.len() > 50
            || self.expected_revision.len() != 64
            || !self
                .expected_revision
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            || self.model_ids.iter().any(|id| {
                id.trim() != id
                    || id.is_empty()
                    || id.len() > 256
                    || id.chars().any(char::is_control)
            })
            || self.model_ids.iter().collect::<BTreeSet<_>>().len() != self.model_ids.len()
        {
            return Err("请选择密钥与有效模型，并重新读取本地配置".into());
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    id: String,
    managed: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigView {
    path: String,
    revision: String,
    models: Vec<ModelView>,
    isolated: bool,
}

pub fn config_path() -> PathBuf {
    crate::config::get_home_dir().join(".workbuddy/models.json")
}

fn check_path(path: &Path) -> Result<(), String> {
    for item in [path.parent().ok_or(READ_ERROR)?, path] {
        match fs::symlink_metadata(item) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(READ_ERROR.into()),
            Ok(meta) if item == path && !meta.is_file() => return Err(READ_ERROR.into()),
            Ok(meta) if item != path && !meta.is_dir() => return Err(READ_ERROR.into()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(READ_ERROR.into()),
        }
    }
    Ok(())
}

fn read_raw(path: &Path) -> Result<Option<Vec<u8>>, String> {
    check_path(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(READ_ERROR.into()),
    };
    if !file.metadata().map_err(|_| READ_ERROR)?.is_file() {
        return Err(READ_ERROR.into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| READ_ERROR)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(READ_ERROR.into());
    }
    Ok(Some(bytes))
}
fn revision(raw: &Option<Vec<u8>>) -> String {
    let mut hash = Sha256::new();
    hash.update(if raw.is_some() {
        b"present"
    } else {
        b"missing"
    });
    if let Some(raw) = raw {
        hash.update(raw);
    }
    format!("{:x}", hash.finalize())
}
fn document(raw: &Option<Vec<u8>>) -> Result<Value, String> {
    let value = match raw {
        Some(raw) => serde_json::from_slice(raw).map_err(|_| READ_ERROR)?,
        None => json!([]),
    };
    rows(&value)?;
    Ok(value)
}
fn rows(value: &Value) -> Result<&Vec<Value>, String> {
    value
        .as_array()
        .or_else(|| value.get("models")?.as_array())
        .ok_or_else(|| READ_ERROR.into())
}
fn owned(row: &Value, account_id: &str) -> bool {
    row.get(OWNER).is_some_and(|marker| {
        marker.get("version").and_then(Value::as_u64) == Some(1)
            && marker.get("accountId").and_then(Value::as_str) == Some(account_id)
    })
}
fn view(path: &Path, raw: &Option<Vec<u8>>) -> Result<ConfigView, String> {
    let document = document(raw)?;
    Ok(ConfigView {
        path: path.to_string_lossy().into_owned(),
        revision: revision(raw),
        isolated: crate::distribution::is_isolated(),
        // Never return apiKey, arbitrary endpoints, names, or unknown fields.
        models: rows(&document)?
            .iter()
            .filter_map(|row| {
                let id = row.get("id")?.as_str()?;
                if id.len() > 256 || id.chars().any(char::is_control) {
                    return None;
                }
                Some(ModelView {
                    id: id.into(),
                    managed: row.get(OWNER).is_some(),
                })
            })
            .collect(),
    })
}
pub fn read() -> Result<ConfigView, String> {
    let path = config_path();
    view(&path, &read_raw(&path)?)
}

// Same mkdir lock path as WorkBuddy's proper-lockfile wrapper. An active writer
// keeps the directory fresh; only an empty lock stale for a minute is recovered.
struct Lock {
    path: PathBuf,
    started: Instant,
    metadata: fs::Metadata,
    compromised: Arc<AtomicBool>,
    stop: mpsc::Sender<()>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}
fn same_lock(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    if !b.is_dir() || b.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        a.created().ok().is_some() && a.created().ok() == b.created().ok()
    }
}
impl Lock {
    fn owns(&self) -> bool {
        fs::symlink_metadata(&self.path).is_ok_and(|current| same_lock(&self.metadata, &current))
    }
    fn check(&self) -> Result<(), String> {
        if self.compromised.load(Ordering::Relaxed)
            || !self.owns()
            || self.started.elapsed().as_secs() >= 4
        {
            Err(CONFLICT.into())
        } else {
            Ok(())
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
        if self.owns() {
            let _ = fs::remove_dir(&self.path);
        }
    }
}
fn lock(path: &Path) -> Result<Lock, String> {
    let directory = path.parent().ok_or(WRITE_ERROR)?;
    check_path(path)?;
    fs::create_dir_all(directory).map_err(|_| WRITE_ERROR)?;
    check_path(path)?;
    let path = directory.join(".models.json.workbuddy.lock");
    if fs::create_dir(&path).is_err() {
        // Active WorkBuddy locks renew every five seconds. Only an empty lock
        // untouched for a full minute can be left over from an interrupted run.
        let stale = fs::symlink_metadata(&path)
            .ok()
            .filter(|meta| meta.is_dir() && !meta.file_type().is_symlink())
            .filter(|meta| {
                meta.modified()
                    .ok()
                    .and_then(|time| SystemTime::now().duration_since(time).ok())
                    .is_some_and(|age| age.as_secs() > 60)
            });
        if stale.is_none() || fs::remove_dir(&path).is_err() || fs::create_dir(&path).is_err() {
            return Err("WorkBuddy 正在修改模型配置，请稍后重试".into());
        }
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| WRITE_ERROR)?;
    let compromised = Arc::new(AtomicBool::new(false));
    let (stop, receiver) = mpsc::channel();
    #[cfg(unix)]
    let heartbeat = {
        let directory = File::open(&path).map_err(|_| WRITE_ERROR)?;
        let compromised = compromised.clone();
        Some(std::thread::spawn(move || {
            while receiver.recv_timeout(Duration::from_secs(2))
                == Err(mpsc::RecvTimeoutError::Timeout)
            {
                if directory
                    .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))
                    .is_err()
                {
                    compromised.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }))
    };
    #[cfg(not(unix))]
    let heartbeat = {
        drop(receiver);
        None
    };
    Ok(Lock {
        path,
        started: Instant::now(),
        metadata,
        compromised,
        stop,
        heartbeat,
    })
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| WRITE_ERROR)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| WRITE_ERROR.into())
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_file_name(format!(".models.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        private_write(&temporary, bytes)?;
        check_path(path)?;
        fs::rename(&temporary, path).map_err(|_| WRITE_ERROR)?;
        #[cfg(unix)]
        File::open(path.parent().ok_or(WRITE_ERROR)?)
            .and_then(|file| file.sync_all())
            .map_err(|_| WRITE_ERROR)?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
fn backup(path: &Path, raw: &[u8]) -> Result<(), String> {
    let directory = path.parent().ok_or(WRITE_ERROR)?.join("backups");
    match fs::symlink_metadata(&directory) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(WRITE_ERROR.into())
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&directory).map_err(|_| WRITE_ERROR)?
        }
        Err(_) => return Err(WRITE_ERROR.into()),
    }
    private_write(
        &directory.join(format!("models.json.osir-{}.bak", uuid::Uuid::new_v4())),
        raw,
    )?;
    #[cfg(unix)]
    File::open(&directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| WRITE_ERROR)?;
    Ok(())
}

pub fn save(
    path: &Path,
    request: &SaveRequest,
    account_id: &str,
    secret: &NativeKeySecret,
    check_session: impl Fn() -> Result<(), String>,
) -> Result<ConfigView, String> {
    request.validate()?;
    let lock = lock(path)?;
    let original = read_raw(path)?;
    if revision(&original) != request.expected_revision {
        return Err(CONFLICT.into());
    }
    let mut document = document(&original)?;
    let models = if document.is_array() {
        document.as_array_mut().unwrap()
    } else {
        document
            .get_mut("models")
            .and_then(Value::as_array_mut)
            .ok_or(READ_ERROR)?
    };
    for id in &request.model_ids {
        let matches: Vec<_> = models
            .iter()
            .enumerate()
            .filter(|(_, row)| row.get("id").and_then(Value::as_str) == Some(id))
            .map(|(i, _)| i)
            .collect();
        if matches.len() > 1
            || matches
                .first()
                .is_some_and(|i| !owned(&models[*i], account_id))
        {
            return Err(
                "所选模型与已有 WorkBuddy 配置重名，请先在 WorkBuddy 中处理后重新读取".into(),
            );
        }
        let index = match matches.first() {
            Some(i) => *i,
            None => {
                models.push(json!({}));
                models.len() - 1
            }
        };
        let row = models[index].as_object_mut().ok_or(READ_ERROR)?;
        row.insert("id".into(), json!(id));
        row.insert("vendor".into(), json!("osir"));
        row.insert(
            "url".into(),
            json!(format!("{GATEWAY_ORIGIN}/v1/chat/completions")),
        );
        row.insert("apiKey".into(), json!(secret.expose()));
        row.insert("supportsToolCall".into(), json!(request.supports_tool_call));
        row.insert("supportsImages".into(), json!(request.supports_images));
        row.insert(
            "supportsReasoning".into(),
            json!(request.supports_reasoning),
        );
        row.insert(
            OWNER.into(),
            json!({"version":1,"accountId":account_id,"keyId":request.key_id}),
        );
    }
    let next = serde_json::to_vec_pretty(&document).map_err(|_| WRITE_ERROR)?;
    if next.len() as u64 > MAX_BYTES {
        return Err(WRITE_ERROR.into());
    }
    if let Some(raw) = &original {
        backup(path, raw)?;
    }
    check_session()?;
    lock.check()?;
    if read_raw(path)? != original {
        return Err(CONFLICT.into());
    }
    let result = atomic_write(path, &next).and_then(|_| {
        if read_raw(path)?.as_deref() != Some(next.as_slice()) {
            return Err(WRITE_ERROR.into());
        }
        check_session()
    });
    if let Err(error) = result {
        // Restore only our exact write. Never overwrite a subsequent user edit.
        if read_raw(path)?.as_deref() == Some(next.as_slice()) {
            match &original {
                Some(raw) => atomic_write(path, raw)?,
                None => fs::remove_file(path)
                    .map_err(|_| "WorkBuddy 保存未确认，请检查本地配置与备份")?,
            }
        } else if read_raw(path)? != original {
            return Err("WorkBuddy 配置发生并发修改，请检查本地配置与备份".into());
        }
        return Err(error);
    }
    view(path, &Some(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(path: &Path) -> SaveRequest {
        SaveRequest {
            generation: 1,
            key_id: 4,
            model_ids: vec!["model-a".into()],
            expected_revision: revision(&read_raw(path).unwrap()),
            supports_tool_call: true,
            supports_images: false,
            supports_reasoning: false,
        }
    }
    fn secret() -> NativeKeySecret {
        NativeKeySecret::new("fixture-only-key".into()).unwrap()
    }
    #[test]
    fn preserves_user_rows_and_object_fields_with_private_backup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.json");
        let original = br#"{"models":[{"id":"other","apiKey":"fixture-other","unknown":true}],"extra":{"keep":1}}"#;
        fs::write(&path, original).unwrap();
        let config = save(&path, &request(&path), "42", &secret(), || Ok(())).unwrap();
        let value = document(&read_raw(&path).unwrap()).unwrap();
        assert_eq!(value["extra"]["keep"], 1);
        assert_eq!(value["models"][0]["unknown"], true);
        assert_eq!(
            value["models"][1]["url"],
            format!("{GATEWAY_ORIGIN}/v1/chat/completions")
        );
        let dto = serde_json::to_string(&config).unwrap();
        assert!(!dto.contains("fixture"));
        assert!(!dto.contains("apiKey"));
        let backup = fs::read_dir(temp.path().join("backups"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read(&backup).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(backup).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn refuses_collisions_conflicts_malformed_and_busy_config() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.json");
        fs::write(&path, br#"[{"id":"model-a","apiKey":"fixture-other"}]"#).unwrap();
        assert!(save(&path, &request(&path), "42", &secret(), || Ok(())).is_err());
        let req = request(&path);
        fs::write(&path, b"[]").unwrap();
        assert!(save(&path, &req, "42", &secret(), || Ok(())).is_err());
        let req = request(&path);
        fs::create_dir(temp.path().join(".models.json.workbuddy.lock")).unwrap();
        assert!(save(&path, &req, "42", &secret(), || Ok(())).is_err());
        fs::remove_dir(temp.path().join(".models.json.workbuddy.lock")).unwrap();
        fs::write(&path, b"{invalid").unwrap();
        assert!(document(&read_raw(&path).unwrap()).is_err());
    }
    #[test]
    fn rolls_back_if_session_changes_after_write_and_does_not_duplicate_updates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.json");
        fs::write(&path, b"[]").unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(save(&path, &request(&path), "42", &secret(), || {
            calls.set(calls.get() + 1);
            if calls.get() > 1 {
                Err("stale".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"[]");
        save(&path, &request(&path), "42", &secret(), || Ok(())).unwrap();
        save(&path, &request(&path), "42", &secret(), || Ok(())).unwrap();
        assert_eq!(
            rows(&document(&read_raw(&path).unwrap()).unwrap())
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn backup_failure_prevents_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.json");
        fs::write(&path, b"[]").unwrap();
        fs::write(temp.path().join("backups"), b"blocked").unwrap();
        assert!(save(&path, &request(&path), "42", &secret(), || Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"[]");
    }
    #[test]
    fn concurrent_user_edit_before_commit_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.json");
        fs::write(&path, b"[]").unwrap();
        let user_edit = br#"[{"id":"user-edited","keep":true}]"#;
        assert!(save(&path, &request(&path), "42", &secret(), || {
            fs::write(&path, user_edit).unwrap();
            Ok(())
        })
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), user_edit);
    }
    #[test]
    fn concurrent_user_edit_after_commit_is_not_overwritten_by_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.json");
        fs::write(&path, b"[]").unwrap();
        let calls = std::cell::Cell::new(0);
        let user_edit = br#"[{"id":"user-edited","keep":true}]"#;
        assert!(save(&path, &request(&path), "42", &secret(), || {
            calls.set(calls.get() + 1);
            if calls.get() > 1 {
                fs::write(&path, user_edit).unwrap();
                Err("stale".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), user_edit);
    }
    #[test]
    fn failed_first_save_removes_only_the_new_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.json");
        let calls = std::cell::Cell::new(0);
        assert!(save(&path, &request(&path), "42", &secret(), || {
            calls.set(calls.get() + 1);
            if calls.get() > 1 {
                Err("stale".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert!(!path.exists());
        assert!(!temp.path().join(".models.json.workbuddy.lock").exists());
    }
    #[test]
    #[cfg(unix)]
    fn refuses_symlink_targets() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("other.json");
        let path = temp.path().join("models.json");
        fs::write(&target, b"[]").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_raw(&path).is_err());
    }
}
