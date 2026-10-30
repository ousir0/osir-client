//! Historical subscription-install identity only; never an account session.
use super::legacy_connect::LegacyConnectSnapshot;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use tauri::Manager;

const MAX_BYTES: u64 = 32 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredLegacySnapshot {
    version: u8,
    kind: String,
    captured_at: String,
    user_id: i64,
    display_name: Option<String>,
    subscriptions: Vec<SubscriptionIdentity>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionIdentity {
    id: i64,
    group_name: Option<String>,
    status: String,
    expires_at: Option<String>,
}

fn safe_text(value: &str) -> bool {
    value.len() <= 256 && !value.chars().any(char::is_control)
}

impl StoredLegacySnapshot {
    fn valid(&self) -> bool {
        self.version == 1
            && self.kind == "legacy_subscription_grant"
            && chrono::DateTime::parse_from_rfc3339(&self.captured_at).is_ok()
            && self.user_id > 0
            && self.display_name.as_deref().map_or(true, safe_text)
            && self.subscriptions.len() <= 100
            && self.subscriptions.iter().all(|s| {
                s.id > 0
                    && safe_text(&s.status)
                    && s.group_name.as_deref().map_or(true, safe_text)
                    && s.expires_at.as_deref().map_or(true, safe_text)
            })
    }
}

fn path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("legacy-subscription-snapshot.json"))
        .map_err(|_| "无法定位订阅授权快照".into())
}

/// Call only AFTER native provider import succeeds. Not an IPC write API.
/// Explicit projection excludes email, balance, keys, provider settings and tokens.
pub(crate) fn save(app: &tauri::AppHandle, source: &LegacyConnectSnapshot) -> Result<(), String> {
    let snapshot = StoredLegacySnapshot {
        version: 1,
        kind: "legacy_subscription_grant".into(),
        captured_at: chrono::Utc::now().to_rfc3339(),
        user_id: source.account.user_id,
        display_name: source.account.display_name.clone(),
        subscriptions: source
            .account
            .subscriptions
            .iter()
            .map(|s| SubscriptionIdentity {
                id: s.id,
                group_name: s.group_name.clone(),
                status: s.status.clone(),
                expires_at: s.expires_at.clone(),
            })
            .collect(),
    };
    if !snapshot.valid() {
        return Err("订阅授权快照元数据无效".into());
    }
    let bytes = serde_json::to_vec(&snapshot).map_err(|_| "无法保存订阅授权快照")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("订阅授权快照过大".into());
    }
    let target = path(app)?;
    let directory = target.parent().ok_or("无法定位订阅授权快照")?;
    std::fs::create_dir_all(directory).map_err(|_| "无法保存订阅授权快照")?;
    // Same-directory atomic replacement; tempfile defaults to private permissions.
    let mut temp =
        tempfile::NamedTempFile::new_in(directory).map_err(|_| "无法保存订阅授权快照")?;
    temp.write_all(&bytes).map_err(|_| "无法保存订阅授权快照")?;
    temp.as_file()
        .sync_all()
        .map_err(|_| "无法保存订阅授权快照")?;
    temp.persist(target).map_err(|_| "无法保存订阅授权快照")?;
    Ok(())
}

/// Local bounded read only: never authorizes, exchanges, or creates a key.
#[tauri::command]
pub fn osir_legacy_snapshot(app: tauri::AppHandle) -> Result<Option<StoredLegacySnapshot>, String> {
    let target = path(&app)?;
    let file = match std::fs::File::open(target) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("无法读取订阅授权历史快照".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取订阅授权历史快照")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("订阅授权历史快照无效".into());
    }
    let snapshot: StoredLegacySnapshot =
        serde_json::from_slice(&bytes).map_err(|_| "订阅授权历史快照无效")?;
    if !snapshot.valid() {
        return Err("订阅授权历史快照无效".into());
    }
    Ok(Some(snapshot))
}
