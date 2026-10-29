//! Read-only key details boundary. DesktopRouteAllowed in the real user API
//! permits key reads/creation, but explicitly denies PUT/DELETE /keys/:id.
//! Do not infer update permission from keys:write or add a token/HTTP bypass.
//! Main-thread integration: declare/re-export this module in commands/mod.rs
//! and register commands::osir_key_management in generate_handler!.
use super::osir::OsirAccountState;
use crate::database::Database;
use crate::provider::{AuthBindingSource, Provider};
use crate::services::osir::{
    auth::AuthManager,
    key_vault::{KeyTool, GATEWAY_ORIGIN},
    models::SessionStatus,
};
use crate::store::AppState;
use serde::Serialize;
use tauri::State;

const STALE: &str = "账户会话已改变，请重新打开密钥详情";
const LOCAL_READ_FAILED: &str = "本机已保存连接读取失败，请手动重试";
const MANAGEMENT_UNAVAILABLE: &str = "当前桌面授权不支持修改密钥名称或启停，请前往 OSIR 网站管理。";

#[derive(Clone, PartialEq, Eq)]
struct AccountSnapshot {
    origin: String,
    id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalKeyUsage {
    tool: KeyTool,
    provider_id: String,
    provider_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyManagementCapabilities {
    rename: bool,
    change_status: bool,
    reason: &'static str,
}

impl KeyManagementCapabilities {
    fn current() -> Self {
        // keys:write grants POST creation only, never name/status mutation.
        Self {
            rename: false,
            change_status: false,
            reason: MANAGEMENT_UNAVAILABLE,
        }
    }
}

/// Explicit IPC allowlist: no settings, binding, reference, credential or key.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyManagementDetails {
    generation: u64,
    key_id: i64,
    source: &'static str,
    local_usages: Vec<LocalKeyUsage>,
    capabilities: KeyManagementCapabilities,
}

fn snapshot(
    status: SessionStatus,
    expected: u64,
    origin: String,
) -> Result<AccountSnapshot, String> {
    if !status.connected || status.generation != expected {
        return Err(STALE.into());
    }
    let profile = status
        .profile
        .filter(|profile| profile.id > 0)
        .ok_or(STALE)?;
    Ok(AccountSnapshot {
        origin,
        id: profile.id.to_string(),
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

fn location(
    provider: &Provider,
    tool: KeyTool,
    account: &AccountSnapshot,
    key_id: i64,
) -> Option<LocalKeyUsage> {
    let managed = provider.meta.as_ref()?.osir_binding.as_ref()?;
    let binding = &managed.binding;
    if managed.source != AuthBindingSource::ManagedAccount
        || provider.id.is_empty()
        || binding.provider_id != provider.id
        || binding.account_origin != account.origin
        || binding.account_id != account.id
        || binding.key_id != key_id
        || binding.tool != tool
        || binding.gateway_origin != GATEWAY_ORIGIN
    {
        return None;
    }
    // Saved references only: no vault load, live file scan or activation claim.
    // Group, name, endpoint and masked suffix are never identity substitutes.
    Some(LocalKeyUsage {
        tool,
        provider_id: provider.id.clone(),
        provider_name: provider.name.clone(),
    })
}

fn saved_locations(
    db: &Database,
    auth: &AuthManager,
    expected: u64,
    account: &AccountSnapshot,
    key_id: i64,
) -> Result<Vec<LocalKeyUsage>, String> {
    let mut result = Vec::new();
    for (app, tool) in [
        ("codex", KeyTool::Codex),
        ("claude", KeyTool::ClaudeCode),
        ("claude-desktop", KeyTool::ClaudeDesktop),
        ("opencode", KeyTool::OpenCode),
        ("gemini", KeyTool::GeminiCli),
    ] {
        check_account(auth, expected, account)?;
        let providers = db.get_all_providers(app).map_err(|_| LOCAL_READ_FAILED)?;
        check_account(auth, expected, account)?;
        result.extend(
            providers
                .values()
                .filter_map(|p| location(p, tool, account, key_id)),
        );
    }
    Ok(result)
}

/// Local inspection only; no operation selector and no remote mutation path.
#[tauri::command]
pub async fn osir_key_management(
    state: State<'_, AppState>,
    account: State<'_, OsirAccountState>,
    generation: u64,
    key_id: i64,
) -> Result<KeyManagementDetails, String> {
    if key_id <= 0 {
        return Err("密钥标识无效".into());
    }
    let before = current_account(&account.auth, generation)?;
    let db = state.db.clone();
    let auth = account.auth.clone();
    let worker_account = before.clone();
    let local_usages = tauri::async_runtime::spawn_blocking(move || {
        saved_locations(&db, &auth, generation, &worker_account, key_id)
    })
    .await
    .map_err(|_| LOCAL_READ_FAILED)??;
    check_account(&account.auth, generation, &before)?;
    Ok(KeyManagementDetails {
        generation,
        key_id,
        source: "saved_provider_metadata",
        local_usages,
        capabilities: KeyManagementCapabilities::current(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ManagedBinding, ProviderMeta};
    use crate::services::osir::{
        account::AccountProfile, key_vault::KeyBinding, models::SessionRecovery,
    };
    use serde_json::json;

    fn account() -> AccountSnapshot {
        AccountSnapshot {
            origin: "https://account.example.test".into(),
            id: "7".into(),
        }
    }

    fn provider(tool: KeyTool) -> Provider {
        let mut provider = Provider::with_id(
            "saved-a".into(),
            "本机连接".into(),
            json!({"api_key": "NEVER_RENDER_SETTINGS"}),
            None,
        );
        provider.meta = Some(ProviderMeta {
            osir_binding: Some(ManagedBinding {
                source: AuthBindingSource::ManagedAccount,
                binding: KeyBinding {
                    account_origin: account().origin,
                    account_id: account().id,
                    key_id: 21,
                    group_id: Some(9),
                    tool,
                    provider_id: provider.id.clone(),
                    gateway_origin: GATEWAY_ORIGIN.into(),
                },
                reference: serde_json::from_value(
                    json!({"version":1,"id":"6f7e49bc-328e-4b14-8e7b-8a971c56bf25"}),
                )
                .unwrap(),
                masked_key: "NEVER_RENDER_METADATA_SECRET".into(),
            }),
            ..Default::default()
        });
        provider
    }

    #[test]
    fn projects_all_four_tools_with_exact_key_identity() {
        for tool in [
            KeyTool::Codex,
            KeyTool::ClaudeCode,
            KeyTool::ClaudeDesktop,
            KeyTool::OpenCode,
            KeyTool::GeminiCli,
        ] {
            let item = location(&provider(tool), tool, &account(), 21).unwrap();
            assert!(item.tool == tool);
            assert_eq!(item.provider_id, "saved-a");
        }
    }

    #[test]
    fn rejects_cross_account_key_tool_provider_gateway_and_manual_sources() {
        for mismatch in 0..8 {
            let mut p = provider(KeyTool::Codex);
            let m = p.meta.as_mut().unwrap().osir_binding.as_mut().unwrap();
            match mismatch {
                0 => m.binding.account_origin.push_str("/other"),
                1 => m.binding.account_id = "8".into(),
                2 => m.binding.key_id = 22,
                3 => m.binding.tool = KeyTool::OpenCode,
                4 => m.binding.provider_id = "another".into(),
                5 => m.binding.gateway_origin = "https://other.example.test".into(),
                6 => m.source = AuthBindingSource::ProviderConfig,
                _ => p.id.clear(),
            }
            assert!(location(&p, KeyTool::Codex, &account(), 21).is_none());
        }
    }

    #[test]
    fn same_group_or_suffix_is_not_a_key_match_and_unmanaged_is_absent() {
        let p = provider(KeyTool::Codex);
        assert!(location(&p, KeyTool::Codex, &account(), 22).is_none());
        let mut unmanaged = p;
        unmanaged.meta = None;
        assert!(location(&unmanaged, KeyTool::Codex, &account(), 21).is_none());
    }

    #[test]
    fn ipc_is_an_allowlist_without_credentials_or_mutation_permissions() {
        let details = KeyManagementDetails {
            generation: 4,
            key_id: 21,
            source: "saved_provider_metadata",
            local_usages: vec![
                location(&provider(KeyTool::Codex), KeyTool::Codex, &account(), 21).unwrap(),
            ],
            capabilities: KeyManagementCapabilities::current(),
        };
        let value = serde_json::to_value(details).unwrap();
        assert_eq!(
            value["localUsages"][0],
            json!({"tool":"codex","providerId":"saved-a","providerName":"本机连接"})
        );
        assert_eq!(value["capabilities"]["rename"], false);
        assert_eq!(value["capabilities"]["changeStatus"], false);
        let wire = value.to_string();
        for forbidden in [
            "NEVER_RENDER",
            "settings",
            "reference",
            "masked_key",
            "account_origin",
            "gateway_origin",
        ] {
            assert!(!wire.contains(forbidden));
        }
    }

    #[test]
    fn snapshot_requires_connected_generation_and_native_account_identity() {
        let status = |connected, generation, profile| SessionStatus {
            connected,
            generation,
            profile,
            can_create_keys: true,
            recovery: SessionRecovery::Idle,
        };
        let profile = || AccountProfile {
            id: 7,
            email: String::new(),
            username: String::new(),
            balance: 0.into(),
        };
        assert!(snapshot(status(false, 4, Some(profile())), 4, account().origin).is_err());
        assert!(snapshot(status(true, 5, Some(profile())), 4, account().origin).is_err());
        assert!(snapshot(status(true, 4, None), 4, account().origin).is_err());
        assert!(
            snapshot(status(true, 4, Some(profile())), 4, account().origin).unwrap() == account()
        );
    }
}
