//! Read-only managed connection details. Key identity comes exclusively from the
//! saved provider; only projected metadata enters the account-scoped read cache.
use super::osir::OsirAccountState;
use crate::{
    app_config::AppType,
    database::Database,
    provider::{ManagedBinding, Provider},
    services::osir::{
        account::{KeyMetadata, WireKey},
        auth::AuthManager,
        key_vault::{KeyTool, GATEWAY_ORIGIN},
        managed,
        models::SessionStatus,
        sync::{QueryClass, ReadPolicy, RequestKey},
    },
    store::AppState,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tauri::State;

const STALE: &str = "账户会话已改变，请重新打开连接配置";
const RELOAD: &str = "连接配置已改变或不可用，请刷新后重新打开";
const UNAVAILABLE: &str = "账户密钥信息读取失败，请稍后重试";
const RESOURCE: &str = "connection_key_metadata";
const METADATA_TTL: Duration = Duration::from_secs(180);

#[derive(Clone, PartialEq, Eq)]
struct AccountSnapshot {
    origin: String,
    id: String,
}

/// No binding, vault reference, settings or raw account response can leave IPC.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDetails {
    generation: u64,
    provider_id: String,
    key_id: i64,
    group_id: Option<i64>,
    key_name: Option<String>,
    group_name: Option<String>,
    masked_key: String,
    group_changed: bool,
}

fn tool_kind(tool: &str) -> Result<(AppType, KeyTool), String> {
    match tool {
        "codex" => Ok((AppType::Codex, KeyTool::Codex)),
        "claude" => Ok((AppType::Claude, KeyTool::ClaudeCode)),
        "claude-desktop" => Ok((AppType::ClaudeDesktop, KeyTool::ClaudeDesktop)),
        "opencode" => Ok((AppType::OpenCode, KeyTool::OpenCode)),
        "gemini" => Ok((AppType::Gemini, KeyTool::GeminiCli)),
        _ => Err(RELOAD.into()),
    }
}

fn snapshot(
    status: SessionStatus,
    expected: u64,
    origin: String,
) -> Result<AccountSnapshot, String> {
    if !status.connected || status.generation != expected {
        return Err(STALE.into());
    }
    let id = status
        .profile
        .filter(|profile| profile.id > 0)
        .ok_or(STALE)?
        .id;
    Ok(AccountSnapshot {
        origin,
        id: id.to_string(),
    })
}

fn current_account(auth: &AuthManager, expected: u64) -> Result<AccountSnapshot, String> {
    snapshot(auth.status(), expected, auth.account_origin())
}

fn check_account(
    auth: &AuthManager,
    expected: u64,
    before: &AccountSnapshot,
) -> Result<(), String> {
    if current_account(auth, expected)? != *before {
        return Err(STALE.into());
    }
    Ok(())
}

fn validated_binding(
    provider: &Provider,
    tool: &str,
    provider_id: &str,
    account: &AccountSnapshot,
) -> Result<ManagedBinding, String> {
    let (app, expected_tool) = tool_kind(tool)?;
    managed::validate_managed_provider_shape(&app, provider).map_err(|_| RELOAD)?;
    let managed = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.osir_binding.as_ref())
        .ok_or(RELOAD)?;
    let binding = &managed.binding;
    if provider.id != provider_id
        || binding.provider_id != provider_id
        || binding.tool != expected_tool
        || binding.account_origin != account.origin
        || binding.account_id != account.id
        || binding.gateway_origin != GATEWAY_ORIGIN
        || binding.key_id <= 0
    {
        return Err(RELOAD.into());
    }
    Ok(managed.clone())
}

fn saved_binding(
    db: &Database,
    tool: &str,
    provider_id: &str,
    account: &AccountSnapshot,
    expected: Option<&ManagedBinding>,
) -> Result<ManagedBinding, String> {
    let provider = db
        .get_provider_by_id(provider_id, tool)
        .map_err(|_| RELOAD)?
        .ok_or(RELOAD)?;
    let binding = validated_binding(&provider, tool, provider_id, account)?;
    if expected.is_some_and(|expected| expected != &binding) {
        return Err(RELOAD.into());
    }
    Ok(binding)
}

fn safe_metadata(wire: WireKey, key_id: i64) -> Result<Value, String> {
    if wire.id != key_id {
        return Err(UNAVAILABLE.into());
    }
    // WireKey deliberately has no Serialize. Project and discard it before
    // returning to SyncCoordinator; raw key/user/group relations never cache.
    serde_json::to_value(wire.metadata()).map_err(|_| UNAVAILABLE.into())
}

fn details(
    metadata: KeyMetadata,
    saved: &ManagedBinding,
    generation: u64,
) -> Result<ConnectionDetails, String> {
    let binding = &saved.binding;
    if metadata.id != binding.key_id {
        return Err(UNAVAILABLE.into());
    }
    let group_changed = metadata.group_id != binding.group_id;
    let group_name = metadata
        .group
        .filter(|group| !group_changed && Some(group.id) == binding.group_id && group.id > 0)
        .map(|group| group.name)
        .filter(|name| !name.trim().is_empty());
    Ok(ConnectionDetails {
        generation,
        provider_id: binding.provider_id.clone(),
        key_id: binding.key_id,
        group_id: binding.group_id,
        key_name: (!metadata.name.trim().is_empty()).then_some(metadata.name),
        group_name,
        masked_key: metadata.masked_key,
        group_changed,
    })
}

#[tauri::command]
pub async fn osir_connection_details(
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
    tool: String,
    provider_id: String,
    generation: u64,
) -> Result<ConnectionDetails, String> {
    tool_kind(&tool)?;
    if provider_id.is_empty()
        || provider_id.len() > 256
        || provider_id.trim() != provider_id
        || provider_id.chars().any(char::is_control)
    {
        return Err(RELOAD.into());
    }
    let before = current_account(&account.auth, generation)?;
    let db = state.db.clone();
    let worker_tool = tool.clone();
    let worker_id = provider_id.clone();
    let worker_account = before.clone();
    let saved = tauri::async_runtime::spawn_blocking(move || {
        saved_binding(&db, &worker_tool, &worker_id, &worker_account, None)
    })
    .await
    .map_err(|_| RELOAD)??;
    check_account(&account.auth, generation, &before)?;
    let session = account.session(generation)?;
    // Key ID is derived solely from the saved DB binding. Multiple providers
    // sharing that key also share one account-scoped metadata request/cache.
    let key_id = saved.binding.key_id;
    let key = RequestKey::new(RESOURCE, json!({"key_id": key_id})).map_err(|_| RELOAD)?;
    let auth = account.auth.clone();
    let fetch_account = before.clone();
    let result = account
        .sync
        .read(
            &session,
            key,
            ReadPolicy {
                ttl: METADATA_TTL,
                class: QueryClass::Standard,
            },
            move || async move {
                check_account(&auth, generation, &fetch_account)?;
                // read_key checks generation and response ID; get_key charges
                // every attempt, including its single authenticated replay.
                let reply = auth
                    .read_key(key_id, generation)
                    .await
                    .map_err(|_| UNAVAILABLE)?;
                if reply.generation != generation {
                    return Err(STALE.into());
                }
                check_account(&auth, generation, &fetch_account)?;
                safe_metadata(reply.data, key_id)
            },
        )
        .await;
    let status = account.reconcile();
    if snapshot(status, generation, account.auth.account_origin())? != before {
        return Err(STALE.into());
    }
    let cached = result.map_err(|_| UNAVAILABLE)?;
    let metadata =
        serde_json::from_value::<KeyMetadata>((*cached).clone()).map_err(|_| UNAVAILABLE)?;
    let response = details(metadata, &saved, generation)?;

    // A replacement/deletion can race the HTTP request or a cache hit. Re-read
    // and compare the complete binding/reference before releasing any result.
    let db = state.db.clone();
    let worker_account = before.clone();
    tauri::async_runtime::spawn_blocking(move || {
        saved_binding(&db, &tool, &provider_id, &worker_account, Some(&saved))
    })
    .await
    .map_err(|_| RELOAD)??;
    check_account(&account.auth, generation, &before)?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::{AuthBindingSource, ProviderMeta},
        services::osir::{account::AccountProfile, key_vault::KeyBinding, models::SessionRecovery},
    };

    fn account() -> AccountSnapshot {
        AccountSnapshot {
            origin: managed::trusted_account_origin().unwrap(),
            id: "7".into(),
        }
    }

    fn provider(tool: &str) -> Provider {
        let (_, key_tool) = tool_kind(tool).unwrap();
        let settings = match tool {
            "claude-desktop" => {
                json!({"env": {"ANTHROPIC_BASE_URL": GATEWAY_ORIGIN, "ANTHROPIC_MODEL": "fixture-model"}})
            }
            "claude" => json!({"env": {"ANTHROPIC_BASE_URL": GATEWAY_ORIGIN}}),
            "gemini" => json!({"env": {"GOOGLE_GEMINI_BASE_URL": GATEWAY_ORIGIN}}),
            "opencode" => json!({"options": {"baseURL": "https://api.osirclaw.com/v1"}}),
            _ => {
                json!({"auth": {}, "config": "model_provider = 'osir'\n[model_providers.osir]\nbase_url = 'https://api.osirclaw.com/v1'\nwire_api = 'responses'\n"})
            }
        };
        let mut provider = Provider::with_id("saved-a".into(), "Saved".into(), settings, None);
        provider.meta = Some(ProviderMeta {
            osir_binding: Some(ManagedBinding {
                source: AuthBindingSource::ManagedAccount,
                binding: KeyBinding {
                    account_origin: account().origin,
                    account_id: account().id,
                    key_id: 21,
                    group_id: Some(9),
                    tool: key_tool,
                    provider_id: provider.id.clone(),
                    gateway_origin: GATEWAY_ORIGIN.into(),
                },
                reference: serde_json::from_value(
                    json!({"version": 1, "id": "6f7e49bc-328e-4b14-8e7b-8a971c56bf25"}),
                )
                .unwrap(),
                masked_key: "synthetic-db-metadata-never-returned".into(),
            }),
            ..Default::default()
        });
        if tool == "claude-desktop" {
            let meta = provider.meta.as_mut().unwrap();
            meta.claude_desktop_mode = Some(crate::provider::ClaudeDesktopMode::Proxy);
            meta.claude_desktop_model_routes.insert(
                crate::claude_desktop_config::DEFAULT_PROXY_ROUTES[0]
                    .route_id
                    .into(),
                crate::provider::ClaudeDesktopModelRoute {
                    model: "fixture-model".into(),
                    label_override: None,
                    supports_1m: Some(false),
                },
            );
        }
        provider
    }

    fn wire(group_id: Option<i64>, nested_id: Option<i64>) -> WireKey {
        serde_json::from_value(json!({
            "id": 21, "name": "Account key", "key": "synthetic-secret-never-returned",
            "group_id": group_id, "group": nested_id.map(|id| json!({
                "id": id, "name": "Cloud group", "credentials": "synthetic-private-relation",
            })),
            "status": "active", "quota": 0, "quota_used": 0, "expires_at": null,
            "user": {"token": "synthetic-private-user"},
        }))
        .unwrap()
    }

    #[test]
    fn only_db_managed_bindings_for_the_current_account_and_tool_are_accepted() {
        for tool in ["codex", "claude", "claude-desktop", "opencode", "gemini"] {
            let binding = validated_binding(&provider(tool), tool, "saved-a", &account()).unwrap();
            assert_eq!(binding.binding.key_id, 21);
        }
        for mismatch in 0..9 {
            let mut provider = provider("claude");
            let managed = provider
                .meta
                .as_mut()
                .unwrap()
                .osir_binding
                .as_mut()
                .unwrap();
            match mismatch {
                0 => managed.binding.account_origin.push_str("/other"),
                1 => managed.binding.account_id = "8".into(),
                2 => managed.binding.provider_id = "other".into(),
                3 => managed.binding.tool = KeyTool::Codex,
                4 => managed.binding.gateway_origin = "https://other.example.invalid".into(),
                5 => managed.source = AuthBindingSource::ProviderConfig,
                6 => managed.binding.key_id = 0,
                7 => provider.meta = None,
                _ => {
                    provider.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"] =
                        json!("synthetic-forbidden")
                }
            }
            assert!(validated_binding(&provider, "claude", "saved-a", &account()).is_err());
        }
        assert!(validated_binding(&provider("claude"), "claude", "other", &account()).is_err());
        assert!(tool_kind("unknown").is_err());
    }

    #[test]
    fn cloud_group_changes_never_relabel_the_local_binding() {
        let saved =
            validated_binding(&provider("claude"), "claude", "saved-a", &account()).unwrap();
        for (group, nested, changed, has_name) in [
            (Some(9), Some(9), false, true),
            (Some(9), Some(10), false, false),
            (Some(10), Some(10), true, false),
            (Some(10), Some(9), true, false),
            (None, None, true, false),
        ] {
            let result = details(wire(group, nested).metadata(), &saved, 4).unwrap();
            assert_eq!(result.group_id, Some(9));
            assert_eq!(result.group_changed, changed);
            assert_eq!(result.group_name.is_some(), has_name);
        }
        let mut ungrouped = saved;
        ungrouped.binding.group_id = None;
        let result = details(wire(Some(9), Some(9)).metadata(), &ungrouped, 4).unwrap();
        assert_eq!(result.group_id, None);
        assert!(result.group_changed);
        assert!(result.group_name.is_none());
    }

    #[test]
    fn cached_metadata_and_ipc_never_include_credentials_or_references() {
        let cached = safe_metadata(wire(Some(9), Some(9)), 21).unwrap();
        let saved =
            validated_binding(&provider("claude"), "claude", "saved-a", &account()).unwrap();
        let result = details(serde_json::from_value(cached.clone()).unwrap(), &saved, 4).unwrap();
        let value = serde_json::to_value(result).unwrap();
        let fields: std::collections::BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            std::collections::BTreeSet::from([
                "generation",
                "providerId",
                "keyId",
                "groupId",
                "keyName",
                "groupName",
                "maskedKey",
                "groupChanged",
            ])
        );
        assert_eq!(value["keyName"], "Account key");
        assert_eq!(value["groupName"], "Cloud group");
        for serialized in [cached.to_string(), value.to_string()] {
            for forbidden in [
                "synthetic-secret",
                "synthetic-private",
                "synthetic-db",
                "reference",
                "account_origin",
                "settings",
                "credentials",
            ] {
                assert!(!serialized.contains(forbidden));
            }
        }
        assert!(safe_metadata(wire(Some(9), Some(9)), 22).is_err());
    }

    #[test]
    fn reread_detects_replacement_removal_and_account_changes() {
        let db = Database::memory().unwrap();
        let provider = provider("claude");
        db.save_provider("claude", &provider).unwrap();
        let saved = saved_binding(&db, "claude", "saved-a", &account(), None).unwrap();
        assert!(saved_binding(&db, "claude", "saved-a", &account(), Some(&saved)).is_ok());
        for mutation in 0..4 {
            let mut replaced = provider.clone();
            let managed = replaced
                .meta
                .as_mut()
                .unwrap()
                .osir_binding
                .as_mut()
                .unwrap();
            match mutation {
                0 => managed.binding.key_id = 22,
                1 => managed.binding.group_id = Some(10),
                2 => {
                    managed.reference = serde_json::from_value(
                        json!({"version": 1, "id": "0e151f87-544b-4345-a397-daf9b647d841"}),
                    )
                    .unwrap()
                }
                _ => replaced.meta = None,
            }
            db.save_provider("claude", &replaced).unwrap();
            assert!(saved_binding(&db, "claude", "saved-a", &account(), Some(&saved)).is_err());
        }
        db.save_provider("claude", &provider).unwrap();
        let mut other_account = account();
        other_account.id = "8".into();
        assert!(saved_binding(&db, "claude", "saved-a", &other_account, None).is_err());
        assert!(saved_binding(&db, "codex", "saved-a", &account(), None).is_err());
        assert!(saved_binding(&db, "claude", "missing", &account(), None).is_err());
    }

    #[test]
    fn account_snapshot_rejects_logout_stale_generations_and_missing_identity() {
        let status = |connected, generation, id: Option<i64>| SessionStatus {
            connected,
            generation,
            recovery: SessionRecovery::Idle,
            can_create_keys: false,
            profile: id.map(|id| AccountProfile {
                id,
                email: String::new(),
                username: String::new(),
                balance: 0.into(),
            }),
        };
        for state in [
            status(false, 4, Some(7)),
            status(true, 5, Some(7)),
            status(true, 4, None),
            status(true, 4, Some(0)),
        ] {
            assert!(snapshot(state, 4, account().origin).is_err());
        }
        assert!(snapshot(status(true, 4, Some(7)), 4, account().origin).unwrap() == account());
        assert!(snapshot(status(true, 4, Some(8)), 4, account().origin).unwrap() != account());
    }
}
