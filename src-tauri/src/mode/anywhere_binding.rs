//! Device-local, credential-free Codex model handoff. Never repairs live files.
//!
//! Official models are authoritative only when published through the Stack's
//! saved native catalog. Non-Stack official direct/proxy connections export
//! unavailable, including direct configs without model_catalog_json: a saved
//! default does not establish the account's currently authorized model list.
//! Third-party connections without a catalog use the current provider's
//! explicit published selection; a referenced catalog is always intersected.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{stack, state};
use crate::app_config::AppType;
use crate::live::engine::{sha256_hex, DeviceStore};
use crate::store::AppState;

const TTL_MS: u64 = 30_000;
const UNAVAILABLE: &str = "connection_unavailable";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BindingModel {
    pub id: String,
    pub title: String,
}

/// Sanitized fixture boundary shared with the headless acceptance executable.
/// These are already authorized IDs, never provider settings or credentials.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingFixture {
    pub codex_home: String,
    pub connection_mode: String,
    pub verified: bool,
    pub pending: bool,
    pub proxy_running: bool,
    pub config: String,
    pub catalog: Option<String>,
    pub models: Vec<BindingModel>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn unavailable(code: &'static str, now: u64) -> Value {
    json!({"schemaVersion":1,"source":"osir-client","available":false,
        "expiresAtMs":now.saturating_add(TTL_MS),"errorCode":code})
}

fn valid_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// Pure descriptor builder. Config/catalog are sampled bytes, used only for
/// parsing and hashing; serialization is an explicit allowlist below.
fn build(
    home: &str,
    mode: &str,
    config: &[u8],
    catalog: Option<&[u8]>,
    authorized: Vec<BindingModel>,
    now: u64,
) -> Result<Value, &'static str> {
    if !Path::new(home).is_absolute()
        || !valid_text(home, 4096)
        || !matches!(mode, "direct" | "proxy")
    {
        return Err(UNAVAILABLE);
    }
    let text = std::str::from_utf8(config).map_err(|_| "config_invalid")?;
    let doc: toml::Table = toml::from_str(text).map_err(|_| "config_invalid")?;
    let default = doc
        .get("model")
        .and_then(toml::Value::as_str)
        .filter(|s| valid_text(s, 256))
        .ok_or("default_model_missing")?;
    let effort = match doc.get("model_reasoning_effort") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|s| valid_text(s, 32))
                .ok_or("config_invalid")?,
        ),
    };
    // A reference must be present iff we sampled its exact bytes.
    let referenced = match doc.get("model_catalog_json") {
        None => false,
        Some(value) => {
            value
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("catalog_invalid")?;
            true
        }
    };
    if referenced != catalog.is_some() {
        return Err("catalog_invalid");
    }
    let allowed: BTreeMap<_, _> = authorized.into_iter().map(|m| (m.id, m.title)).collect();
    let mut models = BTreeMap::new();
    if let Some(bytes) = catalog {
        let parsed: Value = serde_json::from_slice(bytes).map_err(|_| "catalog_invalid")?;
        let rows = parsed
            .get("models")
            .and_then(Value::as_array)
            .ok_or("catalog_invalid")?;
        for row in rows {
            let Some(id) = row.get("slug").and_then(Value::as_str) else {
                continue;
            };
            if !allowed.contains_key(id) {
                continue;
            }
            let title = row
                .get("display_name")
                .and_then(Value::as_str)
                .filter(|s| valid_text(s, 256))
                .unwrap_or(id);
            if !valid_text(id, 256) || !valid_text(title, 256) {
                return Err("models_invalid");
            }
            models.insert(id.to_owned(), title.to_owned());
        }
    } else {
        for (id, title) in allowed {
            if !valid_text(&id, 256) || !valid_text(&title, 256) {
                return Err("models_invalid");
            }
            models.insert(id, title);
        }
    }
    if !models.contains_key(default) {
        return Err("default_model_missing");
    }
    if models.len() > 2048 {
        return Err("models_invalid");
    }
    let models: Vec<_> = models
        .into_iter()
        .map(|(id, title)| BindingModel { id, title })
        .collect();
    let mut descriptor = json!({
        "schemaVersion":1,"source":"osir-client","available":true,
        "codexHome":home,"connectionMode":mode,"defaultModel":default,
        "defaultReasoningEffort":effort,"models":models,
        "configSha256":sha256_hex(config),
        "catalogSha256":catalog.map(sha256_hex).unwrap_or_default(),
    });
    // Stable sorted-key JSON; revision and expiry are never inputs to revision.
    let stable = crate::config::sorted_json_bytes(&descriptor).map_err(|_| UNAVAILABLE)?;
    descriptor["revision"] = json!(sha256_hex(&stable));
    descriptor["expiresAtMs"] = json!(now.saturating_add(TTL_MS));
    Ok(descriptor)
}

pub fn build_fixture(input: BindingFixture) -> Value {
    let now = now_ms();
    if input.pending {
        return unavailable("connection_pending", now);
    }
    if !input.verified {
        return unavailable(UNAVAILABLE, now);
    }
    if input.connection_mode == "proxy" && !input.proxy_running {
        return unavailable("proxy_unavailable", now);
    }
    build(
        &input.codex_home,
        &input.connection_mode,
        input.config.as_bytes(),
        input.catalog.as_deref().map(str::as_bytes),
        input.models,
        now,
    )
    .unwrap_or_else(|code| unavailable(code, now))
}

// This fingerprint is private in-memory concurrency evidence. It is NEVER the
// exported revision, logged, persisted or returned to the frontend.
fn sample_stamp(state: &AppState) -> Result<String, &'static str> {
    let home = crate::codex_config::get_codex_config_dir();
    let store = DeviceStore::for_device();
    let snapshot = state::identity_snapshot(&store).map_err(|_| UNAVAILABLE)?;
    let saved = snapshot.apps.get("codex").cloned().unwrap_or_default();
    let mut material = Vec::new();
    let mut paths = vec![
        home.join("config.toml"),
        home.join(crate::codex_config::CC_SWITCH_CODEX_MODEL_CATALOG_FILENAME),
        store.state_path(),
        store.file("native-connection-restore-codex.json"),
    ];
    // Proxy credential rotation is transparent. Only a direct connection's
    // verification depends on the live auth bytes; refreshes must not cause a
    // spurious unavailable publication for a healthy proxy runtime.
    if saved.mode != Some(state::Mode::Proxy) {
        paths.push(home.join("auth.json"));
    }
    for path in paths {
        let bytes = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(UNAVAILABLE),
        };
        material.push(bytes);
    }
    let published = authorized_models(state, &saved)?;
    let settings = crate::settings::get_settings();
    let active_id = active_provider(state, &saved)?.id;
    let bytes = crate::config::sorted_json_bytes(&(
        material,
        published,
        settings.current_provider_codex,
        active_id,
        home,
    ))
    .map_err(|_| UNAVAILABLE)?;
    Ok(sha256_hex(&bytes))
}

fn active_provider(
    state: &AppState,
    saved: &state::AppLiveState,
) -> Result<crate::provider::Provider, &'static str> {
    let id = if saved.mode == Some(state::Mode::Proxy) {
        saved.proxy_route.clone()
    } else {
        // A stale local pointer is unavailable rather than allowing the normal
        // accessor to repair settings during a read-only export.
        match crate::settings::get_settings().current_provider_codex {
            Some(id) => Some(id),
            None => state
                .db
                .get_current_provider("codex")
                .map_err(|_| UNAVAILABLE)?,
        }
    }
    .ok_or(UNAVAILABLE)?;
    state
        .db
        .get_provider_by_id(&id, "codex")
        .map_err(|_| UNAVAILABLE)?
        .ok_or(UNAVAILABLE)
}

fn authorized_models(
    state: &AppState,
    saved: &state::AppLiveState,
) -> Result<Vec<BindingModel>, &'static str> {
    let active = active_provider(state, saved)?;
    let mut ids = BTreeSet::new();
    if saved.mode == Some(state::Mode::Proxy) && saved.stack.enabled {
        let members = stack::published_members(
            &state.db,
            &AppType::Codex,
            &saved.stack,
            saved.proxy_route.as_deref(),
        )
        .map_err(|_| UNAVAILABLE)?;
        for member in members {
            if crate::services::provider::codex_direct::is_official(&member.provider) {
                ids.extend(stack::official_model_ids(&saved.stack, &member.key));
            } else {
                // Legacy non-managed default routes publish plain IDs. Managed
                // members (including the default) always publish stable IDs.
                if member.route
                    && !stack::is_osir_managed(&member.provider)
                    && !saved.stack.managed_identity_required
                {
                    ids.extend(plain_model_ids(&member.provider));
                } else {
                    ids.extend(stack::codex_model_ids(&member.key, &member.provider));
                }
            }
        }
        // A legacy route is omitted by published_members because it publishes
        // plain IDs. Still restrict those IDs to the current route alone.
        if !saved.stack.managed_identity_required
            && !stack::is_osir_managed(&active)
            && !crate::services::provider::codex_direct::is_official(&active)
        {
            ids.extend(plain_model_ids(&active));
        }
    } else if crate::services::provider::codex_direct::is_official(&active) {
        // Non-Stack official connections have no verified native published
        // model snapshot here. Provider defaults (or an old live catalog) do
        // not establish current account entitlement, so export unavailable.
        return Err(UNAVAILABLE);
    } else {
        ids.extend(plain_model_ids(&active));
    }
    Ok(ids
        .into_iter()
        .map(|id| BindingModel {
            title: id.clone(),
            id,
        })
        .collect())
}

fn plain_model_ids(provider: &crate::provider::Provider) -> Vec<String> {
    if stack::is_osir_managed(provider) {
        crate::codex_config::managed_codex_published_models(&provider.settings_config)
    } else {
        crate::codex_config::codex_published_models(
            &provider.settings_config,
            provider
                .settings_config
                .get("config")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )
    }
}

fn sample_descriptor(
    state: &AppState,
    connection: (Option<bool>, &str, Option<bool>),
    now: u64,
) -> Result<Value, &'static str> {
    let snapshot = state::identity_snapshot(&DeviceStore::for_device()).map_err(|_| UNAVAILABLE)?;
    let saved = snapshot.apps.get("codex").cloned().unwrap_or_default();
    if saved.pending.is_some() {
        return Err("connection_pending");
    }
    if connection.0 != Some(true) {
        return Err(UNAVAILABLE);
    }
    let proxy = saved.mode == Some(state::Mode::Proxy);
    if proxy && (!saved.attached || connection.1 != "proxy" || connection.2 != Some(true)) {
        return Err("proxy_unavailable");
    }
    if !proxy && connection.1 != "direct" {
        return Err(UNAVAILABLE);
    }
    let home = fs::canonicalize(crate::codex_config::get_codex_config_dir())
        .map_err(|_| "config_invalid")?;
    if !home.is_dir() {
        return Err("config_invalid");
    }
    let config = fs::read(home.join("config.toml")).map_err(|_| "config_invalid")?;
    let doc: toml::Table =
        toml::from_str(std::str::from_utf8(&config).map_err(|_| "config_invalid")?)
            .map_err(|_| "config_invalid")?;
    let catalog_path = match doc.get("model_catalog_json") {
        None => None,
        Some(value) => {
            let path = value
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("catalog_invalid")?;
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                home.join(path)
            };
            Some(path)
        }
    };
    let catalog = catalog_path
        .as_ref()
        .map(fs::read)
        .transpose()
        .map_err(|_| "catalog_invalid")?;
    let models = authorized_models(state, &saved)?;
    // Re-read external files too: editors/Codex itself do not share our lock.
    let descriptor = build(
        home.to_str().ok_or("config_invalid")?,
        connection.1,
        &config,
        catalog.as_deref(),
        models,
        now,
    )?;
    if fs::read(home.join("config.toml")).map_err(|_| "config_invalid")? != config {
        return Err("connection_changed");
    }
    if catalog_path
        .as_ref()
        .map(fs::read)
        .transpose()
        .map_err(|_| "catalog_invalid")?
        != catalog
    {
        return Err("connection_changed");
    }
    Ok(descriptor)
}

/// No frontend data or raw errors cross this boundary. Hold the app switch
/// lock for both authoritative samples and the final atomic publication.
pub(crate) async fn publish(state: &AppState) -> Result<(), &'static str> {
    let before = {
        let _guard = state.proxy_service.lock_switch_for_app("codex").await;
        sample_stamp(state)
    };
    let connection = if before.is_ok() {
        super::controller::anywhere_codex_connection(state)
            .await
            .unwrap_or((None, "unavailable", None))
    } else {
        // Invalid local pointers must not reach accessors that repair settings.
        (None, "unavailable", None)
    };
    let _guard = state.proxy_service.lock_switch_for_app("codex").await;
    let now = now_ms();
    let after = sample_stamp(state);
    let mut result = match (before, &after) {
        (Ok(before), Ok(after)) if &before == after => {
            let mut connection = connection;
            if connection.1 == "proxy" {
                connection.2 = Some(state.proxy_service.is_running().await);
            }
            sample_descriptor(state, connection, now)
        }
        (Ok(_), Ok(_)) => Err("connection_changed"),
        _ => Err(UNAVAILABLE),
    };
    if result.is_ok() && sample_stamp(state) != after {
        result = Err("connection_changed");
    }
    let descriptor = result.unwrap_or_else(|code| unavailable(code, now));
    let bytes =
        crate::config::sorted_json_bytes(&descriptor).map_err(|_| "binding_write_failed")?;
    write_binding(
        &crate::config::get_device_config_dir().join("anywhere"),
        &bytes,
    )
}

pub(crate) fn start_publisher(state: AppState) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if !crate::distribution::is_osir() || STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(code) = publish(&state).await {
                log::warn!("Codex local binding: {code}");
            }
        }
    });
}

#[cfg(unix)]
fn write_binding(directory: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    // Walk using pinned directory descriptors: no symlink component can redirect
    // creation/chmod/rename, even if an ancestor is concurrently replaced.
    let mut dir = fs::File::open("/").map_err(|_| "binding_write_failed")?;
    if !directory.is_absolute() {
        return Err("binding_unsafe_path");
    }
    for component in directory.components() {
        let Component::Normal(name) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err("binding_unsafe_path");
        };
        let name = CString::new(name.as_bytes()).map_err(|_| "binding_unsafe_path")?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            let created = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
            if created < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err("binding_write_failed");
            }
            fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err("binding_unsafe_path");
        }
        dir = unsafe { fs::File::from_raw_fd(fd) };
    }
    if unsafe { libc::fchmod(dir.as_raw_fd(), 0o700) } != 0 {
        return Err("binding_write_failed");
    }
    let target = CString::new("codex-binding.json").unwrap();
    let check_target = || -> Result<(), &'static str> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                target.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc == 0 {
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
                return Err("binding_unsafe_path");
            }
        } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
            return Err("binding_write_failed");
        }
        Ok(())
    };
    check_target()?;
    let temp = CString::new(format!(".codex-binding-{}.tmp", uuid::Uuid::new_v4())).unwrap();
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err("binding_write_failed");
    }
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let result = (|| {
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "binding_write_failed")?;
        check_target()?;
        if unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                temp.as_ptr(),
                dir.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            return Err("binding_write_failed");
        }
        dir.sync_all().map_err(|_| "binding_write_failed")
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0);
        }
    }
    result
}

#[cfg(not(unix))]
fn write_binding(directory: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    use std::io::Write;
    let mut path = PathBuf::new();
    for part in directory.components() {
        path.push(part);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("binding_unsafe_path");
        }
    }
    fs::create_dir_all(directory).map_err(|_| "binding_write_failed")?;
    let target = directory.join("codex-binding.json");
    if fs::symlink_metadata(&target).is_ok_and(|m| !m.file_type().is_file()) {
        return Err("binding_unsafe_path");
    }
    let mut file =
        tempfile::NamedTempFile::new_in(directory).map_err(|_| "binding_write_failed")?;
    file.write_all(bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| "binding_write_failed")?;
    file.persist(&target).map_err(|_| "binding_write_failed")?;
    Ok(())
}

#[cfg(test)]
#[path = "anywhere_binding_tests.rs"]
mod tests;
