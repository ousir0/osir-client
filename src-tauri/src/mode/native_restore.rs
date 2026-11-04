//! Explicit connection-selector recovery only. Never capture on a page read or
//! infer that a saved third-party row was the user's original configuration.
//! The private record uses the same local file/backup boundary as the write engine.
use super::{
    AppState, AppType, ConnectionModeView, Mode, ModeState, NativeConfiguration, PendingTarget,
};
use crate::error::AppError;
use crate::live::engine::{self, DeviceStore, LiveFile, Planned};
use crate::live::patch::{Guarded, WholeFile};
use crate::live::patch::{KeyPath, LivePatch};
use crate::mode::operation::{AppWrite, FileChange};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct File {
    path: PathBuf,
    before: Option<Vec<u8>>,
    expected: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    active: bool,
    #[serde(default)]
    official_login: bool,
    #[serde(default)]
    applying: bool,
    files: Vec<File>,
}
fn failure() -> String {
    "原配置恢复记录缺失或配置已变化，未覆盖现有配置".into()
}
fn paths(app: &AppType) -> Vec<PathBuf> {
    match app {
        AppType::Codex => vec![
            crate::codex_config::get_codex_config_path(),
            crate::codex_config::get_codex_auth_path(),
            crate::codex_config::get_codex_model_catalog_path(),
        ],
        AppType::Claude => vec![crate::config::get_claude_settings_path()],
        AppType::Gemini => vec![
            crate::gemini_config::get_gemini_env_path(),
            crate::gemini_config::get_gemini_settings_path(),
        ],
        AppType::ClaudeDesktop => {
            let Ok(lib) = crate::claude_desktop_config::get_config_library_path() else {
                return Vec::new();
            };
            let Some(threep) = lib.parent() else {
                return Vec::new();
            };
            let Some(parent) = threep.parent() else {
                return Vec::new();
            };
            vec![
                parent.join("Claude/claude_desktop_config.json"),
                threep.join("claude_desktop_config.json"),
                lib.join("_meta.json"),
                lib.join(format!("{}.json", crate::claude_desktop_config::PROFILE_ID)),
            ]
        }
        _ => Vec::new(),
    }
}
fn record_path(store: &DeviceStore, app: &AppType) -> PathBuf {
    store.file(&format!("native-connection-restore-{}.json", app.as_str()))
}
fn read(app: &AppType) -> Result<Option<Record>, String> {
    read_at(&DeviceStore::for_device(), app)
}
fn read_at(store: &DeviceStore, app: &AppType) -> Result<Option<Record>, String> {
    let Some(bytes) = engine::read_current(&record_path(store, app)).map_err(|_| failure())? else {
        return Ok(None);
    };
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| failure())?;
    if record.version != 1 || record.files.iter().map(|f| &f.path).ne(paths(app).iter()) {
        return Err(failure());
    }
    Ok(Some(record))
}
pub(super) fn has_record(app: &AppType) -> Result<bool, String> {
    let Some(record) = read(app)? else {
        return Ok(false);
    };
    if record.applying || !matches(&record)? {
        return Err(failure());
    }
    Ok(record.active)
}

pub(super) fn local_configuration(app: &AppType) -> Option<NativeConfiguration> {
    let official = || NativeConfiguration {
        source: "official_login",
        status: "available",
        label: "官方登录",
        detail: Some("已检测到本机登录配置，登录是否有效由工具确认"),
    };
    match app {
        AppType::Codex => {
            let live = crate::codex_config::read_codex_live_settings().ok()?;
            if crate::services::ProxyService::config_has_proxy_placeholder(app, &live) {
                return None;
            }
            let config = live
                .get("config")?
                .as_str()?
                .parse::<toml_edit::DocumentMut>()
                .ok()?;
            let selected = config.get("model_provider").and_then(|v| v.as_str());
            if (selected.is_none() || selected == Some("openai"))
                && live.pointer("/auth/tokens").is_some_and(|v| v.is_object())
            {
                return Some(official());
            }
            let selected = selected?;
            config
                .get("model_providers")
                .and_then(|p| p.get(selected))
                .and_then(|p| p.get("base_url"))
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(|_| NativeConfiguration::local("available"))
        }
        AppType::Claude => {
            let value: serde_json::Value =
                crate::config::read_json_file(&crate::config::get_claude_settings_path()).ok()?;
            if crate::services::ProxyService::config_has_proxy_placeholder(app, &value) {
                return None;
            }
            let projection = crate::live::project::claude::ClaudeProjection::of(&value);
            (!projection.env.is_empty() || !projection.top.is_empty())
                .then(|| NativeConfiguration::local("available"))
        }
        AppType::Gemini => {
            let config: serde_json::Value =
                crate::config::read_json_file(&crate::gemini_config::get_gemini_settings_path())
                    .ok()?;
            let env = crate::gemini_config::read_gemini_env().ok()?;
            let live = serde_json::json!({"env": env});
            if crate::services::ProxyService::config_has_proxy_placeholder(app, &live) {
                return None;
            }
            if config
                .pointer("/security/auth/selectedType")
                .and_then(|v| v.as_str())
                == Some("oauth-personal")
                && crate::gemini_config::get_gemini_dir()
                    .join("oauth_creds.json")
                    .is_file()
            {
                return Some(official());
            }
            env.get("GEMINI_API_KEY")
                .filter(|v| !v.is_empty())
                .map(|_| NativeConfiguration::local("available"))
        }
        AppType::ClaudeDesktop => {
            let lib = crate::claude_desktop_config::get_config_library_path().ok()?;
            let config: serde_json::Value =
                crate::config::read_json_file(&lib.parent()?.join("claude_desktop_config.json"))
                    .ok()?;
            let meta: serde_json::Value =
                crate::config::read_json_file(&lib.join("_meta.json")).ok()?;
            if meta.get("appliedId").and_then(|v| v.as_str())
                == Some(crate::claude_desktop_config::PROFILE_ID)
            {
                return None;
            }
            (config.get("deploymentMode").and_then(|v| v.as_str()) == Some("1p"))
                .then(|| NativeConfiguration::local("available"))
        }
        _ => None,
    }
}

pub(super) fn provider_is_live(app: &AppType, row: &crate::provider::Provider) -> bool {
    match app {
        AppType::Claude => {
            let Ok(live) = crate::config::read_json_file::<serde_json::Value>(
                &crate::config::get_claude_settings_path(),
            ) else {
                return false;
            };
            crate::live::project::claude::ClaudeProjection::of(&live)
                == crate::live::project::claude::ClaudeProjection::of(&row.settings_config)
        }
        AppType::Codex => {
            let Ok(live) = crate::codex_config::read_codex_live_settings() else {
                return false;
            };
            let parse = |text: &str| text.parse::<toml_edit::DocumentMut>().ok();
            let Some(actual) = live.get("config").and_then(|v| v.as_str()).and_then(parse) else {
                return false;
            };
            let Some(expected) = row
                .settings_config
                .get("config")
                .and_then(|v| v.as_str())
                .and_then(parse)
            else {
                return false;
            };
            [
                "model_provider",
                "model",
                "model_providers",
                "openai_base_url",
            ]
            .iter()
            .all(|key| {
                actual.get(key).map(ToString::to_string)
                    == expected.get(key).map(ToString::to_string)
            }) && live.pointer("/auth/OPENAI_API_KEY")
                == row.settings_config.pointer("/auth/OPENAI_API_KEY")
        }
        AppType::Gemini => {
            let Ok(env) = crate::gemini_config::read_gemini_env() else {
                return false;
            };
            let Some(expected) = row.settings_config.get("env").and_then(|v| v.as_object()) else {
                return false;
            };
            expected
                .iter()
                .filter(|(key, _)| crate::live::floor::gemini_floor_env(key))
                .all(|(key, value)| value.as_str() == env.get(key).map(String::as_str))
        }
        AppType::ClaudeDesktop => {
            let Ok(lib) = crate::claude_desktop_config::get_config_library_path() else {
                return false;
            };
            let Ok(meta) =
                crate::config::read_json_file::<serde_json::Value>(&lib.join("_meta.json"))
            else {
                return false;
            };
            if meta.get("appliedId").and_then(|v| v.as_str())
                != Some(crate::claude_desktop_config::PROFILE_ID)
            {
                return false;
            }
            let Ok(live) = crate::config::read_json_file::<serde_json::Value>(
                &lib.join(format!("{}.json", crate::claude_desktop_config::PROFILE_ID)),
            ) else {
                return false;
            };
            let Ok(Some(expected)) = crate::claude_desktop_config::direct_target(Some(row)) else {
                return false;
            };
            ["inferenceGatewayBaseUrl", "inferenceGatewayApiKey"]
                .iter()
                .all(|key| live.get(key) == expected.get(key))
        }
        _ => false,
    }
}

pub(super) fn desktop_attached(state: &AppState) -> bool {
    let Ok(lib) = crate::claude_desktop_config::get_config_library_path() else {
        return false;
    };
    let Ok(meta) = crate::config::read_json_file::<serde_json::Value>(&lib.join("_meta.json"))
    else {
        return false;
    };
    if meta.get("appliedId").and_then(|v| v.as_str())
        != Some(crate::claude_desktop_config::PROFILE_ID)
    {
        return false;
    }
    let Ok(profile) = crate::config::read_json_file::<serde_json::Value>(
        &lib.join(format!("{}.json", crate::claude_desktop_config::PROFILE_ID)),
    ) else {
        return false;
    };
    let Ok(expected) = crate::claude_desktop_config::proxy_gateway_base_url_from_db(&state.db)
    else {
        return false;
    };
    profile
        .get("inferenceGatewayBaseUrl")
        .and_then(|v| v.as_str())
        == Some(expected.as_str())
}

/// Avoid current::direct_provider's legacy stale-pointer cleanup on reads.
pub(super) fn direct_read_only(
    state: &AppState,
    app: &AppType,
) -> Result<Option<crate::provider::Provider>, String> {
    let settings = crate::settings::get_settings();
    let local = match app {
        AppType::Codex => settings.current_provider_codex,
        AppType::Claude => settings.current_provider_claude,
        AppType::Gemini => settings.current_provider_gemini,
        AppType::ClaudeDesktop => settings.current_provider_claude_desktop,
        _ => None,
    };
    if let Some(id) = local {
        if let Some(row) = state
            .db
            .get_provider_by_id(&id, app.as_str())
            .map_err(|_| failure())?
        {
            return Ok(Some(row));
        }
    }
    // Same database fallback as mode::current, deliberately without repairing settings.
    let id = crate::database::Database::get_current_provider(&state.db, app.as_str())
        .map_err(|_| failure())?;
    id.map(|id| {
        state
            .db
            .get_provider_by_id(&id, app.as_str())
            .map_err(|_| failure())
    })
    .transpose()
    .map(Option::flatten)
}
fn matches(record: &Record) -> Result<bool, String> {
    record.files.iter().try_fold(true, |ok, file| {
        let bytes = engine::read_current(&file.path).map_err(|_| failure())?;
        Ok(ok && engine::digest(bytes.as_deref()) == file.expected)
    })
}

/// Record publication is a member of the SAME pending operation as the live
/// files. Only the in-memory plan's verified pre/post may advance expected.
/// A conflict disables replanning for this operation; recovery never adopts
/// external bytes as the expected post-image.
pub(crate) fn transaction(
    store: &DeviceStore,
    app: &str,
    plans: &[(&Planned, &dyn LivePatch)],
    target: &PendingTarget,
) -> Result<Option<(LiveFile, Guarded)>, AppError> {
    let Ok(app) = app.parse::<AppType>() else {
        return Ok(None);
    };
    if paths(&app).is_empty() {
        return Ok(None);
    }
    let fail = || AppError::Message(failure());
    let capture = target.extra.get("nativeCapture").and_then(|v| v.as_bool()) == Some(true);
    let mut record = match read_at(store, &app).map_err(|_| fail())? {
        Some(record) => record,
        None if capture => {
            // Authorization was established by the pure selector view. Recheck
            // actual native evidence while holding the live writer lock.
            let source = local_configuration(&app).ok_or_else(fail)?;
            Record {
                version: 1,
                active: false,
                applying: false,
                official_login: source.source == "official_login",
                files: paths(&app)
                    .into_iter()
                    .map(|path| {
                        let before = engine::read_current(&path)?;
                        Ok(File {
                            path,
                            expected: engine::digest(before.as_deref()),
                            before,
                        })
                    })
                    .collect::<Result<Vec<_>, AppError>>()?,
            }
        }
        None => return Ok(None),
    };
    if record.applying || !matches(&record).map_err(|_| fail())? {
        return Err(fail());
    }
    let restoring = target.extra.get("nativeRestore").and_then(|v| v.as_bool()) == Some(true);
    for file in &mut record.files {
        if let Some((plan, _)) = plans.iter().find(|(p, _)| p.file.path == file.path) {
            if plan.pre != file.expected {
                return Err(fail());
            }
            if !restoring {
                // MCP belongs to the user, including updates made while attached.
                // Replay only its known pre/post delta into the native baseline.
                file.before = preserve_mcp(
                    &app,
                    &file.path,
                    file.before.as_deref(),
                    plan.pre_bytes(),
                    plan.planned_bytes(),
                )?;
            }
            file.expected = plan.planned.clone();
        }
    }
    if let Some(mode) = &target.state {
        record.active = mode.attached;
    }
    let record_path = record_path(store, &app);
    let previous = engine::read_current(&record_path)?;
    let bytes = serde_json::to_vec(&record).map_err(|_| fail())?;
    Ok(Some((
        LiveFile::private(record_path),
        Guarded {
            expected_pre: engine::digest(previous.as_deref()),
            then: WholeFile::Write(bytes),
        },
    )))
}

/// Merge only changes to MCP sections, preserving native route/auth/model bytes.
fn preserve_mcp(
    app: &AppType,
    path: &std::path::Path,
    original: Option<&[u8]>,
    pre: Option<&[u8]>,
    post: Option<&[u8]>,
) -> Result<Option<Vec<u8>>, AppError> {
    if app == &AppType::Codex && path == crate::codex_config::get_codex_config_path() {
        use crate::live::patch::toml::{parse, TomlPatch};
        let pre = parse(path, pre)?;
        let post = parse(path, post)?;
        let mut patch = TomlPatch::default();
        for segments in [&["mcp_servers"][..], &["mcp", "servers"][..]] {
            let at = |doc: &toml_edit::DocumentMut| -> Option<toml_edit::Item> {
                let mut item = doc.get(segments[0])?;
                for key in &segments[1..] {
                    item = item.get(*key)?;
                }
                Some(item.clone())
            };
            let old = at(&pre);
            let new = at(&post);
            // Item/Table Display emits only the immediate table body. For an
            // implicit parent such as [mcp_servers.keep], that body is empty,
            // even when nested servers were added/edited/deleted. A document
            // traverses the entire subtree, including standard child tables.
            let subtree_text = |item: &toml_edit::Item| {
                let mut doc = toml_edit::DocumentMut::new();
                doc["subtree"] = item.clone();
                doc.to_string()
            };
            if old.as_ref().map(subtree_text) == new.as_ref().map(subtree_text) {
                continue;
            }
            match new {
                Some(item) => patch.set.push((KeyPath::new(segments), item)),
                None => patch.remove.push(KeyPath::new(segments)),
            }
        }
        if patch.set.is_empty() && patch.remove.is_empty() {
            return Ok(original.map(<[u8]>::to_vec));
        }
        return Ok(Some(patch.apply(path, original)?));
    }
    if (app == &AppType::Gemini && path == crate::gemini_config::get_gemini_settings_path())
        || app == &AppType::Claude
        || app == &AppType::ClaudeDesktop
    {
        let parse = |bytes: Option<&[u8]>| -> Result<serde_json::Value, AppError> {
            bytes
                .map(|b| serde_json::from_slice(b).map_err(|_| AppError::Message(failure())))
                .unwrap_or_else(|| Ok(serde_json::json!({})))
        };
        let pre = parse(pre)?;
        let post = parse(post)?;
        if pre.get("mcpServers") != post.get("mcpServers") {
            let mut patch = crate::live::patch::json::JsonPatch::default();
            match post.get("mcpServers") {
                Some(value) => patch
                    .set
                    .push((KeyPath::new(&["mcpServers"]), value.clone())),
                None => patch.remove.push(KeyPath::new(&["mcpServers"])),
            }
            return Ok(Some(patch.apply(path, original)?));
        }
    }
    Ok(original.map(<[u8]>::to_vec))
}

/// None means no new-selector evidence; the caller may inspect legacy direct
/// restore evidence, but must not manufacture an official seed as a fallback.
pub(super) fn view(
    app: &AppType,
    mode: &ModeState,
    attached_live: bool,
) -> Result<Option<ConnectionModeView>, String> {
    let Some(record) = read(app)? else {
        return Ok(None);
    };
    let valid = !record.applying && matches(&record)?;
    let available = record.files.iter().any(|f| f.before.is_some());
    Ok(Some(ConnectionModeView {
        mode: if !valid {
            "unavailable"
        } else if record.active {
            if mode.is_proxy() && mode.attached && attached_live {
                "api_key"
            } else {
                "unavailable"
            }
        } else if !mode.is_proxy() && !mode.attached {
            "native"
        } else {
            "unavailable"
        },
        can_restore_native: valid && available,
        reason: (!valid).then_some("配置已另行修改，无法安全恢复原配置"),
        native_configuration: Some(if record.official_login {
            NativeConfiguration {
                source: "official_login",
                status: "available",
                label: "官方登录",
                detail: Some("已保留本机登录配置，登录是否有效由工具确认"),
            }
        } else {
            NativeConfiguration::local(if available {
                "available"
            } else {
                "unavailable"
            })
        }),
    }))
}

pub(super) fn restore(state: &AppState, app: &AppType, mode: &ModeState) -> Result<bool, String> {
    restore_to(
        state,
        app,
        &ModeState {
            mode: Some(Mode::Direct),
            attached: false,
            proxy_route: mode.proxy_route.clone(),
            contract: None,
        },
    )
}

pub(super) fn restore_to(
    state: &AppState,
    app: &AppType,
    mode: &ModeState,
) -> Result<bool, String> {
    let Some(record) = read(app)? else {
        return Ok(false);
    };
    if !matches(&record)? {
        return Err(failure());
    }
    // Missing originals are not a license to turn an empty file into a login.
    if !record.files.iter().any(|f| f.before.is_some()) {
        return Err(failure());
    }
    let patches: Vec<_> = record
        .files
        .iter()
        .map(|file| Guarded {
            expected_pre: file.expected.clone(),
            then: match &file.before {
                Some(bytes) => WholeFile::Write(bytes.clone()),
                None => WholeFile::Delete,
            },
        })
        .collect();
    let changes: Vec<_> = record
        .files
        .iter()
        .zip(&patches)
        .map(|(file, patch)| FileChange {
            file: LiveFile::private(&file.path),
            patch,
        })
        .collect();
    // Use the normal validated, backed-up, recoverable multi-file write engine.
    // Expected is advanced only by the paired internal transaction, including MCP.
    AppWrite::begin(&state.db, app.as_str())
        .map_err(|_| failure())?
        .run(
            "native-restore",
            &changes,
            PendingTarget {
                state: Some(mode.clone()),
                extra: [("nativeRestore".into(), serde_json::Value::Bool(true))]
                    .into_iter()
                    .collect(),
                ..PendingTarget::default()
            },
        )
        .map_err(|_| failure())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn u1_native_restore_nested_mcp_delta_keeps_add_edit_and_remove() {
        // Pure in-memory fixtures: no reads/writes of native configuration.
        let path = crate::codex_config::get_codex_config_path();
        for (pre, post) in [
            (
                "[mcp_servers.keep]\ncommand = \"native\"\n[mcp_servers.edited]\ncommand = \"old\"\n[mcp_servers.removed]\ncommand = \"old\"\n",
                "[mcp_servers.keep]\ncommand = \"native\"\n[mcp_servers.edited]\ncommand = \"new\"\n[mcp_servers.added]\ncommand = \"fixture\"\n",
            ),
            (
                "mcp_servers = { keep = {command = \"native\"}, edited = {command = \"old\"}, removed = {command = \"old\"} }\n",
                "mcp_servers = { keep = {command = \"native\"}, edited = {command = \"new\"}, added = {command = \"fixture\"} }\n",
            ),
        ] {
            let original = format!("model = \"native-model\"\n{pre}");
            let live_pre = format!("model = \"api-model\"\n{pre}");
            let live_post = format!("model = \"api-next\"\n{post}");
            let merged = preserve_mcp(&AppType::Codex, &path, Some(original.as_bytes()),
                Some(live_pre.as_bytes()), Some(live_post.as_bytes())).unwrap().unwrap();
            let doc: toml::Table = toml::from_str(std::str::from_utf8(&merged).unwrap()).unwrap();
            assert_eq!(doc["model"].as_str(), Some("native-model"));
            let servers = doc["mcp_servers"].as_table().unwrap();
            assert_eq!(servers["keep"]["command"].as_str(), Some("native"));
            assert_eq!(servers["added"]["command"].as_str(), Some("fixture"));
            assert_eq!(servers["edited"]["command"].as_str(), Some("new"));
            assert!(!servers.contains_key("removed"));
        }
    }
}
