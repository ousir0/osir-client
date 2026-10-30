//! Role-only managed Claude updates. Persistence, projection and recovery use
//! the existing provider service; credentials never leave the native process.
use super::osir::OsirAccountState;
use crate::{
    app_config::AppType,
    error::AppError,
    provider::{ManagedBinding, Provider},
    services::{
        osir::{connection::desktop_conversation_model, gateway, key_vault::KeyReference, managed},
        ProviderService,
    },
    store::AppState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use tauri::{Manager, State};

const FIELDS: [&str; 4] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
];
const RELOAD: &str = "连接或模型角色已改变，请刷新后重新选择";
const UNAVAILABLE: &str = "所选模型不可用，请重新获取模型后选择";
const UNCONFIRMED: &str = "模型角色保存结果未确认，请刷新连接核对后重试";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeModelRoles {
    #[serde(rename = "ANTHROPIC_MODEL")]
    default: String,
    #[serde(rename = "ANTHROPIC_DEFAULT_HAIKU_MODEL")]
    quick: String,
    #[serde(rename = "ANTHROPIC_DEFAULT_SONNET_MODEL")]
    primary: String,
    #[serde(rename = "ANTHROPIC_DEFAULT_OPUS_MODEL")]
    advanced: String,
}
impl ClaudeModelRoles {
    fn values(&self) -> [&str; 4] {
        [&self.default, &self.quick, &self.primary, &self.advanced]
    }
    fn valid(&self) -> bool {
        self.values()
            .iter()
            .all(|id| id.len() <= 256 && id.trim() == *id && !id.chars().any(char::is_control))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaudeModelRolesRequest {
    provider_id: String,
    expected_reference: KeyReference,
    expected_roles: ClaudeModelRoles,
    roles: ClaudeModelRoles,
}
fn invalid(message: &str) -> AppError {
    AppError::InvalidInput(message.into())
}
fn binding(provider: &Provider) -> Result<&ManagedBinding, AppError> {
    managed::validate_managed_provider_shape(&AppType::Claude, provider)?;
    provider
        .meta
        .as_ref()
        .and_then(|meta| meta.osir_binding.as_ref())
        .ok_or_else(|| invalid(RELOAD))
}
fn read_roles(provider: &Provider) -> Result<ClaudeModelRoles, AppError> {
    let env = provider
        .settings_config
        .get("env")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(RELOAD))?;
    let read = |field: &str| -> Result<String, AppError> {
        match env.get(field) {
            None => Ok(String::new()),
            Some(Value::String(value)) => Ok(value.clone()),
            _ => Err(invalid(RELOAD)),
        }
    };
    Ok(ClaudeModelRoles {
        default: read(FIELDS[0])?,
        quick: read(if env.contains_key(FIELDS[1]) {
            FIELDS[1]
        } else {
            "ANTHROPIC_SMALL_FAST_MODEL"
        })?,
        primary: read(FIELDS[2])?,
        advanced: read(FIELDS[3])?,
    })
}
fn check_expected(provider: &Provider, request: &ClaudeModelRolesRequest) -> Result<(), AppError> {
    if provider.id != request.provider_id
        || binding(provider)?.reference != request.expected_reference
        || read_roles(provider)? != request.expected_roles
    {
        return Err(invalid(RELOAD));
    }
    Ok(())
}
fn prepare(
    latest: &Provider,
    catalog_binding: &ManagedBinding,
    request: &ClaudeModelRolesRequest,
    catalog: &HashSet<String>,
) -> Result<Provider, AppError> {
    check_expected(latest, request)?;
    if binding(latest)? != catalog_binding || !request.roles.valid() {
        return Err(invalid(RELOAD));
    }
    let mut next = latest.clone();
    let env = next
        .settings_config
        .get_mut("env")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid(RELOAD))?;
    for ((field, value), old) in FIELDS
        .iter()
        .zip(request.roles.values())
        .zip(request.expected_roles.values())
    {
        // Missing roles follow the existing normalization. Clearing an explicit
        // override is not presented as native-default behavior.
        if value.is_empty() {
            if !old.is_empty() {
                return Err(invalid(UNAVAILABLE));
            }
        } else {
            env.insert((*field).into(), Value::String(value.into()));
        }
    }
    crate::services::provider::normalize_claude_models_in_value(&mut next.settings_config);
    if read_roles(&next)?
        .values()
        .iter()
        .any(|id| !id.is_empty() && (!desktop_conversation_model(id) || !catalog.contains(*id)))
    {
        return Err(invalid(UNAVAILABLE));
    }
    managed::validate_managed_provider_shape(&AppType::Claude, &next)?;
    Ok(next)
}

#[tauri::command]
pub async fn osir_update_claude_model_roles(
    app: tauri::AppHandle,
    account: State<'_, OsirAccountState>,
    request: ClaudeModelRolesRequest,
) -> Result<ClaudeModelRoles, String> {
    if request.provider_id.is_empty()
        || request.provider_id.len() > 256
        || request.provider_id.chars().any(char::is_control)
        || !request.roles.valid()
        || !request.expected_roles.valid()
    {
        return Err(UNAVAILABLE.into());
    }
    let snapshot_app = app.clone();
    let provider_id = request.provider_id.clone();
    let expected_reference = request.expected_reference.clone();
    let expected_roles = request.expected_roles.clone();
    let (snapshot, secret) = tokio::task::spawn_blocking(move || {
        let state = snapshot_app.state::<AppState>();
        let provider = state
            .db
            .get_provider_by_id(&provider_id, "claude")
            .map_err(|_| RELOAD.to_string())?
            .ok_or_else(|| RELOAD.to_string())?;
        let grant = binding(&provider).map_err(|_| RELOAD.to_string())?;
        if grant.reference != expected_reference
            || read_roles(&provider).map_err(|_| RELOAD.to_string())? != expected_roles
        {
            return Err(RELOAD.into());
        }
        let secret = managed::vault()
            .map_err(|_| "连接凭据不可用".to_string())?
            .load(&grant.reference, &grant.binding)
            .map_err(|_| "连接凭据不可用".to_string())?
            .ok_or_else(|| "连接凭据不可用".to_string())?;
        Ok::<_, String>((provider, secret))
    })
    .await
    .map_err(|_| RELOAD.to_string())??;
    check_expected(&snapshot, &request).map_err(|_| RELOAD.to_string())?;
    let grant = binding(&snapshot).map_err(|_| RELOAD.to_string())?.clone();
    // Reuse the native catalog/cache and request budget. No renderer catalog
    // authority, additional cache, or new credential API.
    let models = gateway::models_for_binding(&secret, &grant, &account.auth.request_budget())
        .await
        .map_err(|_| "模型目录暂时不可用，请重试获取".to_string())?;
    let catalog: HashSet<String> = models
        .into_iter()
        .filter(|model| desktop_conversation_model(&model.id))
        .map(|model| model.id)
        .collect();
    if catalog.is_empty() {
        return Err(UNAVAILABLE.into());
    }
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut expected_saved = None;
        let saved = ProviderService::update_osir_claude_models_checked(
            state.inner(),
            &request.provider_id,
            |latest| {
                let next = prepare(latest, &grant, &request, &catalog)?;
                expected_saved = Some(read_roles(&next)?);
                Ok(next)
            },
        )
        .map_err(|_| UNCONFIRMED.to_string())?;
        let roles = read_roles(&saved).map_err(|_| UNCONFIRMED.to_string())?;
        if Some(&roles) != expected_saved.as_ref()
            || binding(&saved).map_err(|_| UNCONFIRMED.to_string())? != &grant
        {
            return Err(UNCONFIRMED.into());
        }
        Ok(roles)
    })
    .await
    .map_err(|_| UNCONFIRMED.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Provider {
        let mut provider = Provider::with_id(
            "fixture".into(),
            "Fixture".into(),
            json!({
                "env": {"ANTHROPIC_BASE_URL": "https://api.osirclaw.com", "ANTHROPIC_MODEL": "model-a", "USER_SETTING": "keep"},
                "mcpServers": {"fixture": {"command": "fixture"}}
            }),
            None,
        );
        provider.meta = Some(serde_json::from_value(json!({"osirBinding": {
            "source": "managed_account", "masked_key": "****",
            "reference": {"version": 1, "id": "6c0ab1ea-f36b-4d4d-a8af-098ad95b6f02"},
            "binding": {"account_origin": "https://osirclaw.com", "account_id": "fixture", "key_id": 1, "group_id": 2, "tool": "claude_code", "provider_id": "fixture", "gateway_origin": "https://api.osirclaw.com"}
        }})).unwrap());
        provider
    }
    fn request(provider: &Provider) -> ClaudeModelRolesRequest {
        let expected_roles = read_roles(provider).unwrap();
        let mut roles = expected_roles.clone();
        roles.quick = "model-b".into();
        ClaudeModelRolesRequest {
            provider_id: provider.id.clone(),
            expected_reference: binding(provider).unwrap().reference.clone(),
            expected_roles,
            roles,
        }
    }
    fn catalog() -> HashSet<String> {
        ["model-a", "model-b"]
            .map(String::from)
            .into_iter()
            .collect()
    }
    #[test]
    fn role_only_merge_preserves_binding_mcp_and_latest_unrelated_settings() {
        let provider = fixture();
        let request = request(&provider);
        let mut latest = provider.clone();
        latest.settings_config["env"]["NEW_SETTING"] = json!("new");
        let next = prepare(&latest, binding(&provider).unwrap(), &request, &catalog()).unwrap();
        assert_eq!(next.settings_config["env"][FIELDS[1]], "model-b");
        assert_eq!(next.settings_config["env"][FIELDS[2]], "model-a");
        assert_eq!(next.settings_config["env"]["NEW_SETTING"], "new");
        assert_eq!(next.settings_config["env"]["USER_SETTING"], "keep");
        assert_eq!(
            next.settings_config["mcpServers"],
            latest.settings_config["mcpServers"]
        );
        assert_eq!(
            serde_json::to_value(&next.meta).unwrap(),
            serde_json::to_value(&latest.meta).unwrap()
        );
        assert!(!next.settings_config.to_string().contains("AUTH_TOKEN"));
    }
    #[test]
    fn stale_roles_binding_rotation_and_manual_replacement_are_rejected() {
        let provider = fixture();
        let request = request(&provider);
        let grant = binding(&provider).unwrap();
        let mut latest = provider.clone();
        latest.settings_config["env"][FIELDS[0]] = json!("model-b");
        assert!(prepare(&latest, grant, &request, &catalog()).is_err());
        latest = provider.clone();
        latest
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .binding
            .key_id = 2;
        assert!(prepare(&latest, grant, &request, &catalog()).is_err());
        latest = provider.clone();
        latest
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .reference = serde_json::from_value(
            json!({"version":1, "id":"b4f186ab-e652-4611-9230-a44e92695dcf"}),
        )
        .unwrap();
        assert!(prepare(&latest, grant, &request, &catalog()).is_err());
        latest.meta = None;
        assert!(prepare(&latest, grant, &request, &catalog()).is_err());
    }
    #[test]
    fn unavailable_empty_and_invalid_models_fail_without_mutating_source() {
        let provider = fixture();
        let before = provider.settings_config.clone();
        let mut request = request(&provider);
        for model in ["withdrawn", " model-b"] {
            request.roles.quick = model.into();
            assert!(prepare(&provider, binding(&provider).unwrap(), &request, &catalog()).is_err());
        }
        request.roles.quick = "model-b".into();
        request.roles.quick.push(char::from(10));
        assert!(prepare(&provider, binding(&provider).unwrap(), &request, &catalog()).is_err());
        request.roles.quick = "model-b".into();
        assert!(prepare(
            &provider,
            binding(&provider).unwrap(),
            &request,
            &HashSet::new()
        )
        .is_err());
        request.roles.default.clear();
        assert!(prepare(&provider, binding(&provider).unwrap(), &request, &catalog()).is_err());
        assert_eq!(provider.settings_config, before);
    }

    #[test]
    fn image_output_roles_are_rejected_but_image_input_conversation_roles_are_kept() {
        let provider = fixture();
        let grant = binding(&provider).unwrap();
        let mut request = request(&provider);
        request.roles.quick = "gpt-image-2".into();
        let mut available = catalog();
        available.insert("gpt-image-2".into());
        assert!(prepare(&provider, grant, &request, &available).is_err());

        request.roles.quick = "vision-image-input-model".into();
        available.insert("vision-image-input-model".into());
        let next = prepare(&provider, grant, &request, &available).unwrap();
        assert_eq!(
            next.settings_config["env"][FIELDS[1]],
            "vision-image-input-model"
        );
    }
    #[test]
    fn role_payload_rejects_credentials_and_unknown_fields() {
        let mut payload = serde_json::to_value(read_roles(&fixture()).unwrap()).unwrap();
        payload["ANTHROPIC_AUTH_TOKEN"] = json!("fixture-only");
        assert!(serde_json::from_value::<ClaudeModelRoles>(payload).is_err());
    }
}
