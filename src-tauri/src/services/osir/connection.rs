//! Native construction of OSIR provider rows. No model secret enters this row.
use crate::{app_config::AppType, provider::Provider};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRequest {
    pub tool: String,
    pub key_id: i64,
    pub name: String,
    #[serde(default, rename = "hideAggregationName")]
    pub hide_aggregation_name: Option<bool>,
    pub model: String,
    #[serde(default)]
    pub models: Vec<String>,
    pub generation: u64,
}
/// IPC supplies selection and a compare token, never a credential binding.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceConnectionKeyRequest {
    pub tool: String,
    pub provider_id: String,
    pub key_id: i64,
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub models: Vec<String>,
    pub generation: u64,
    pub expected_reference: super::key_vault::KeyReference,
}
impl ReplaceConnectionKeyRequest {
    pub fn validate(&self) -> Result<AppType, String> {
        if self.provider_id.is_empty()
            || self.provider_id.len() > 256
            || self.provider_id.trim() != self.provider_id
            || self.provider_id.chars().any(char::is_control)
        {
            return Err("连接参数无效".into());
        }
        self.selection().validate()
    }
    fn selection(&self) -> ConnectionRequest {
        ConnectionRequest {
            tool: self.tool.clone(),
            key_id: self.key_id,
            name: self.name.clone(),
            hide_aggregation_name: None,
            model: self.model.clone(),
            models: self.models.clone(),
            generation: self.generation,
        }
    }
    pub(crate) fn validate_catalog(
        &self,
        models: &[crate::services::model_fetch::FetchedModel],
    ) -> Result<(), String> {
        validate_catalog_selection(&self.model, &self.models, models)
    }
}

/// Native capability: not deserializable and never accepted by an IPC command.
pub(crate) struct TrustedKeyReplacement {
    request: ReplaceConnectionKeyRequest,
    binding: crate::provider::ManagedBinding,
}
impl TrustedKeyReplacement {
    pub(crate) fn new(
        request: ReplaceConnectionKeyRequest,
        stored: &super::key_vault::KeyMetadata,
    ) -> Result<Self, String> {
        let app = request.validate()?;
        let mut row = provider_row(&request.selection(), request.provider_id.clone())?;
        let binding = crate::provider::ManagedBinding {
            source: crate::provider::AuthBindingSource::ManagedAccount,
            binding: stored.binding.clone(),
            reference: stored.reference.clone(),
            masked_key: stored.masked_key.clone(),
        };
        row.meta.get_or_insert_with(Default::default).osir_binding = Some(binding.clone());
        super::managed::validate_managed_provider_shape(&app, &row)
            .map_err(|_| "连接绑定无效".to_string())?;
        if stored.binding.key_id != request.key_id || stored.reference == request.expected_reference
        {
            return Err("连接绑定无效".into());
        }
        Ok(Self { request, binding })
    }
    pub(crate) fn provider_id(&self) -> &str {
        &self.request.provider_id
    }
    pub(crate) fn apply(&self, app: &AppType, saved: &Provider) -> Result<Provider, String> {
        if self.request.validate()? != *app || saved.id != self.request.provider_id {
            return Err("连接类型已改变，请重新加载".into());
        }
        super::managed::validate_managed_provider_shape(app, saved)
            .map_err(|_| "原连接配置无效".to_string())?;
        let old = saved
            .meta
            .as_ref()
            .and_then(|m| m.osir_binding.as_ref())
            .ok_or("此连接不是账户托管连接")?;
        if old.reference != self.request.expected_reference {
            return Err("连接已改变，请重新加载后换钥".into());
        }
        // Keep latest metadata, MCP, table/path names and non-model fields.
        let mut row = saved.clone();
        row.name = self.request.name.trim().into();
        let selected = provider_row(&self.request.selection(), saved.id.clone())?;
        match app {
            AppType::Codex => {
                let mut doc: toml_edit::DocumentMut = row.settings_config["config"]
                    .as_str()
                    .ok_or("原连接配置无效")?
                    .parse()
                    .map_err(|_| "原连接配置无效")?;
                doc["model"] = toml_edit::value(self.request.model.as_str());
                row.settings_config["config"] = json!(doc.to_string());
                let catalog = row
                    .settings_config
                    .as_object_mut()
                    .ok_or("原连接配置无效")?
                    .entry("modelCatalog")
                    .or_insert_with(|| json!({}));
                catalog.as_object_mut().ok_or("原模型目录无效")?.insert(
                    "models".into(),
                    selected.settings_config["modelCatalog"]["models"].clone(),
                );
            }
            AppType::Claude | AppType::Gemini => {
                let env = row.settings_config["env"]
                    .as_object_mut()
                    .ok_or("原连接配置无效")?;
                let fields: &[&str] = if matches!(app, AppType::Claude) {
                    &[
                        "ANTHROPIC_MODEL",
                        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                        "ANTHROPIC_DEFAULT_SONNET_MODEL",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL",
                        "CLAUDE_CODE_SUBAGENT_MODEL",
                    ]
                } else {
                    &["GEMINI_MODEL"]
                };
                for (index, field) in fields.iter().enumerate() {
                    if index == 0 || env.contains_key(*field) {
                        env.insert((*field).into(), json!(self.request.model));
                    }
                }
            }
            AppType::ClaudeDesktop => {
                row.settings_config["env"]["ANTHROPIC_MODEL"] = json!(self.request.model);
                let meta = row.meta.as_mut().ok_or("原连接配置无效")?;
                let selected_meta = selected.meta.as_ref().ok_or("新连接配置无效")?;
                meta.claude_desktop_mode = selected_meta.claude_desktop_mode.clone();
                meta.claude_desktop_model_routes =
                    selected_meta.claude_desktop_model_routes.clone();
            }
            AppType::OpenCode => {
                row.settings_config["models"] = selected.settings_config["models"].clone();
            }
            _ => return Err("此工具不支持账户换钥".into()),
        }
        row.meta.as_mut().unwrap().osir_binding = Some(self.binding.clone());
        super::managed::validate_managed_provider_shape(app, &row)
            .map_err(|_| "新连接配置无效".to_string())?;
        Ok(row)
    }
}

impl ConnectionRequest {
    pub fn validate(&self) -> Result<AppType, String> {
        if self.key_id <= 0
            || self.name.trim().is_empty()
            || self.name.len() > 160
            || self.name.chars().any(char::is_control)
            || self.model.is_empty()
            || self.model.len() > 256
            || self.model.trim() != self.model
            || self.model.chars().any(char::is_control)
            || self.models.len() > 1000
            || self.models.iter().any(|id| {
                id.is_empty()
                    || id.len() > 256
                    || id.trim() != id
                    || id.chars().any(char::is_control)
            })
            || (!self.models.is_empty() && !self.models.contains(&self.model))
        {
            return Err("连接参数无效".into());
        }
        match self.tool.as_str() {
            "codex" => Ok(AppType::Codex),
            "claude" => Ok(AppType::Claude),
            "claude-desktop"
                if self.models.len() <= 200
                    && desktop_conversation_model(&self.model)
                    && self
                        .models
                        .iter()
                        .all(|model| desktop_conversation_model(model)) =>
            {
                Ok(AppType::ClaudeDesktop)
            }
            "opencode" => Ok(AppType::OpenCode),
            "gemini" => Ok(AppType::Gemini),
            _ => Err("此工具不支持账户连接".into()),
        }
    }

    pub(crate) fn validate_catalog(
        &self,
        models: &[crate::services::model_fetch::FetchedModel],
    ) -> Result<(), String> {
        validate_catalog_selection(&self.model, &self.models, models)
    }
}

/// The gateway currently publishes IDs only. Exclude known image-output
/// families without excluding conversation models that accept image input.
pub(crate) fn desktop_conversation_model(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    let id = normalized
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    !(id.starts_with("gpt-image-")
        || id.starts_with("dall-e-2")
        || id.starts_with("dall-e-3")
        || matches!(
            id,
            "grok-imagine"
                | "grok-imagine-edit"
                | "gemini-2.5-flash-image"
                | "gemini-3.1-flash-image"
        )
        || id.starts_with("grok-imagine-image"))
}

fn validate_catalog_selection(
    selected: &str,
    requested: &[String],
    catalog: &[crate::services::model_fetch::FetchedModel],
) -> Result<(), String> {
    if !catalog.iter().any(|model| model.id == selected)
        || requested
            .iter()
            .any(|id| !catalog.iter().any(|model| &model.id == id))
    {
        return Err("所选模型不在密钥的可用目录中，请重新读取并选择".into());
    }
    Ok(())
}

pub fn provider_row(request: &ConnectionRequest, id: String) -> Result<Provider, String> {
    let tool = request.validate()?;
    let origin = super::key_vault::GATEWAY_ORIGIN;
    let model = &request.model;
    // Omitted/empty catalogs preserve the existing single-model behavior.
    let models = if request.models.is_empty() {
        std::slice::from_ref(model)
    } else {
        request.models.as_slice()
    };
    let config = match tool {
        AppType::Codex => {
            // Serialize TOML instead of interpolating a model supplied by IPC.
            let mut table = toml_edit::DocumentMut::new();
            table["model_provider"] = toml_edit::value("osir");
            table["model"] = toml_edit::value(model.as_str());
            table["model_providers"]["osir"]["name"] = toml_edit::value(request.name.trim());
            table["model_providers"]["osir"]["base_url"] = toml_edit::value(format!("{origin}/v1"));
            table["model_providers"]["osir"]["wire_api"] = toml_edit::value("responses");
            table["model_providers"]["osir"]["requires_openai_auth"] = toml_edit::value(true);
            json!({"config":table.to_string(),"auth":{},"modelCatalog":{"models":models.iter().map(|id| json!({"model":id})).collect::<Vec<_>>()}})
        }
        AppType::Claude | AppType::ClaudeDesktop => {
            json!({"env":{"ANTHROPIC_BASE_URL":origin,"ANTHROPIC_MODEL":model}})
        }
        AppType::Gemini => json!({"env":{"GOOGLE_GEMINI_BASE_URL":origin,"GEMINI_MODEL":model}}),
        AppType::OpenCode => {
            let catalog: serde_json::Map<String, serde_json::Value> = models
                .iter()
                .map(|id| (id.clone(), json!({"name":id})))
                .collect();
            json!({"npm":"@ai-sdk/openai-compatible","options":{"baseURL":format!("{origin}/v1")},"models":catalog})
        }
        _ => return Err("此工具不支持账户连接".into()),
    };
    let mut provider = Provider::with_id(
        id,
        request.name.trim().into(),
        config,
        Some("https://osirclaw.com".into()),
    );
    provider.category = Some("custom".into());
    if tool == AppType::ClaudeDesktop {
        // Every selected upstream model gets a Desktop-safe role. The default
        // stays first; only the loopback gateway token reaches Desktop files.
        let mut selected = vec![model.clone()];
        let mut others: Vec<_> = models.iter().filter(|id| *id != model).cloned().collect();
        others.sort();
        others.dedup();
        selected.extend(others);
        let role = crate::claude_desktop_config::DEFAULT_PROXY_ROUTES[0].route_id;
        let meta = provider.meta.get_or_insert_with(Default::default);
        meta.api_format = Some("anthropic".into());
        meta.claude_desktop_mode = Some(crate::provider::ClaudeDesktopMode::Proxy);
        meta.claude_desktop_model_routes = selected
            .into_iter()
            .enumerate()
            .map(|(index, model)| {
                let route = if index == 0 {
                    role.to_owned()
                } else {
                    format!("{role}-r{}", index + 1)
                };
                (
                    route,
                    crate::provider::ClaudeDesktopModelRoute {
                        label_override: Some(model.clone()),
                        model,
                        supports_1m: Some(false),
                    },
                )
            })
            .collect();
    }
    if tool == AppType::Codex {
        provider
            .meta
            .get_or_insert_with(Default::default)
            .hide_aggregation_name = Some(request.hide_aggregation_name.unwrap_or(true));
    }
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::{AuthBindingSource, ManagedBinding},
        services::osir::key_vault::{
            KeyBinding, KeyMetadata, KeyReference, KeyTool, GATEWAY_ORIGIN,
        },
    };

    fn reference(id: &str) -> KeyReference {
        serde_json::from_value(json!({"version":1,"id":id})).unwrap()
    }
    fn replacement_request(tool: &str) -> ReplaceConnectionKeyRequest {
        ReplaceConnectionKeyRequest {
            tool: tool.into(),
            provider_id: "fixture".into(),
            key_id: 2,
            name: " Renamed OSIR ".into(),
            model: "model-2".into(),
            models: vec!["model-2".into(), "model-3".into()],
            generation: 2,
            expected_reference: reference("6c0ab1ea-f36b-4d4d-a8af-098ad95b6f02"),
        }
    }
    fn replacement_fixture(tool: &str) -> (AppType, Provider, KeyMetadata) {
        let req = request(tool);
        let app = req.validate().unwrap();
        let mut saved = provider_row(&req, "fixture".into()).unwrap();
        let binding = KeyBinding {
            account_origin: "https://osirclaw.com".into(),
            account_id: "old-account".into(),
            key_id: 1,
            group_id: Some(3),
            provider_id: saved.id.clone(),
            gateway_origin: GATEWAY_ORIGIN.into(),
            tool: match app {
                AppType::Codex => KeyTool::Codex,
                AppType::Claude => KeyTool::ClaudeCode,
                AppType::ClaudeDesktop => KeyTool::ClaudeDesktop,
                AppType::Gemini => KeyTool::GeminiCli,
                AppType::OpenCode => KeyTool::OpenCode,
                _ => unreachable!(),
            },
        };
        let old = ManagedBinding {
            source: AuthBindingSource::ManagedAccount,
            binding: binding.clone(),
            reference: replacement_request(tool).expected_reference,
            masked_key: "••••old1".into(),
        };
        saved.meta.get_or_insert_with(Default::default).osir_binding = Some(old);
        saved.meta.as_mut().unwrap().common_config_enabled = Some(false);
        if matches!(app, AppType::Codex) {
            saved.meta.as_mut().unwrap().hide_aggregation_name = Some(false);
        }
        saved.notes = Some("Latest user notes".into());
        saved.sort_index = Some(7);
        saved.created_at = Some(12345);
        saved.in_failover_queue = true;
        saved.settings_config["mcpServers"] = json!({"user-owned":{"command":"fixture-mcp"}});
        saved.settings_config["userSetting"] = json!({"preserve":true});
        match app {
            AppType::Codex => {
                let mut doc: toml_edit::DocumentMut = saved.settings_config["config"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap();
                let providers = doc["model_providers"]
                    .as_inline_table_mut()
                    .expect("generated Codex providers use an inline table");
                let route = providers.remove("osir").unwrap();
                providers.insert("existing_osir_route", route);
                doc["model_provider"] = toml_edit::value("existing_osir_route");
                doc["model_catalog_json"] = toml_edit::value("/fixture/original-catalog.json");
                doc["model_reasoning_effort"] = toml_edit::value("high");
                doc["mcp_servers"]["user"]["command"] = toml_edit::value("fixture-mcp");
                saved.settings_config["config"] = json!(doc.to_string());
                saved.settings_config["modelCatalog"]["userLabel"] = json!("preserve");
            }
            AppType::Claude => {
                saved.settings_config["env"]["USER_SETTING"] = json!("preserve");
                saved.settings_config["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"] = json!("old-haiku");
                saved.settings_config["env"]["CLAUDE_CODE_SUBAGENT_MODEL"] = json!("old-subagent");
            }
            AppType::Gemini => saved.settings_config["env"]["USER_SETTING"] = json!("preserve"),
            AppType::ClaudeDesktop => {
                saved.settings_config["env"]["USER_SETTING"] = json!("preserve")
            }
            AppType::OpenCode => saved.settings_config["options"]["timeout"] = json!(1234),
            _ => unreachable!(),
        }
        let mut new_binding = binding;
        new_binding.account_id = "new-account".into();
        new_binding.key_id = 2;
        new_binding.group_id = None;
        let metadata = KeyMetadata {
            binding: new_binding,
            reference: reference("550e8400-e29b-41d4-a716-446655440000"),
            masked_key: "••••new2".into(),
        };
        (app, saved, metadata)
    }

    #[test]
    fn replace_key_four_tools_keep_latest_identity_routes_user_fields_and_new_catalog() {
        for tool in ["codex", "claude", "gemini", "opencode"] {
            let (app, saved, metadata) = replacement_fixture(tool);
            let before = serde_json::to_value(&saved).unwrap();
            let replacement =
                TrustedKeyReplacement::new(replacement_request(tool), &metadata).unwrap();
            let updated = replacement.apply(&app, &saved).unwrap();
            assert_eq!(replacement.provider_id(), saved.id);
            assert_eq!(updated.id, saved.id);
            assert_eq!(updated.name, "Renamed OSIR");
            assert_eq!(serde_json::to_value(&saved).unwrap(), before);
            let binding = updated
                .meta
                .as_ref()
                .unwrap()
                .osir_binding
                .as_ref()
                .unwrap();
            assert_eq!(binding.binding, metadata.binding);
            assert_eq!(binding.reference, metadata.reference);
            assert_eq!(binding.masked_key, metadata.masked_key);
            // Every field outside name, settings and binding must remain the latest value.
            let mut expected_outer = before.clone();
            let mut actual_outer = serde_json::to_value(&updated).unwrap();
            for outer in [&mut expected_outer, &mut actual_outer] {
                outer.as_object_mut().unwrap().remove("name");
                outer.as_object_mut().unwrap().remove("settingsConfig");
                outer["meta"].as_object_mut().unwrap().remove("osirBinding");
            }
            assert_eq!(actual_outer, expected_outer);
            assert_eq!(
                updated.settings_config["mcpServers"],
                saved.settings_config["mcpServers"]
            );
            assert_eq!(
                updated.settings_config["userSetting"],
                saved.settings_config["userSetting"]
            );
            match app {
                AppType::Codex => {
                    let mut old: toml::Value = saved.settings_config["config"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap();
                    let mut new: toml::Value = updated.settings_config["config"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap();
                    assert_eq!(new["model"].as_str(), Some("model-2"));
                    assert_eq!(new["model_provider"].as_str(), Some("existing_osir_route"));
                    assert_eq!(
                        new["model_catalog_json"].as_str(),
                        Some("/fixture/original-catalog.json")
                    );
                    old.as_table_mut().unwrap().remove("model");
                    new.as_table_mut().unwrap().remove("model");
                    assert_eq!(new, old);
                    assert_eq!(
                        updated.settings_config["modelCatalog"],
                        json!({"userLabel":"preserve","models":[{"model":"model-2"},{"model":"model-3"}]})
                    );
                }
                AppType::Claude | AppType::Gemini => {
                    let mut expected = saved.settings_config.clone();
                    let fields: &[&str] = if matches!(app, AppType::Claude) {
                        &[
                            "ANTHROPIC_MODEL",
                            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                            "CLAUDE_CODE_SUBAGENT_MODEL",
                        ]
                    } else {
                        &["GEMINI_MODEL"]
                    };
                    for field in fields {
                        expected["env"][*field] = json!("model-2");
                    }
                    assert_eq!(updated.settings_config, expected);
                }
                AppType::OpenCode => {
                    let mut expected = saved.settings_config.clone();
                    expected["models"] =
                        json!({"model-2":{"name":"model-2"},"model-3":{"name":"model-3"}});
                    assert_eq!(updated.settings_config, expected);
                }
                _ => unreachable!(),
            }
            let wire = serde_json::to_string(&updated).unwrap();
            for field in [
                "OPENAI_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "GEMINI_API_KEY",
                "apiKey",
            ] {
                assert!(!wire.contains(field));
            }
        }
    }

    #[test]
    fn replace_key_rejects_stale_reference_wrong_row_tool_and_reuse_without_mutation() {
        for tool in ["codex", "claude", "gemini", "opencode"] {
            let (app, saved, metadata) = replacement_fixture(tool);
            let replacement =
                TrustedKeyReplacement::new(replacement_request(tool), &metadata).unwrap();
            let mut stale = saved.clone();
            stale
                .meta
                .as_mut()
                .unwrap()
                .osir_binding
                .as_mut()
                .unwrap()
                .reference = metadata.reference.clone();
            let before = serde_json::to_value(&stale).unwrap();
            assert!(replacement.apply(&app, &stale).is_err());
            assert_eq!(serde_json::to_value(&stale).unwrap(), before);
            let mut wrong_row = saved.clone();
            wrong_row.id = "other-provider".into();
            assert!(replacement.apply(&app, &wrong_row).is_err());
            let other_app = if matches!(app, AppType::Codex) {
                AppType::Claude
            } else {
                AppType::Codex
            };
            assert!(replacement.apply(&other_app, &saved).is_err());
            let mut competing_metadata = metadata.clone();
            competing_metadata.reference = reference("550e8400-e29b-41d4-a716-446655440001");
            let competing =
                TrustedKeyReplacement::new(replacement_request(tool), &competing_metadata).unwrap();
            let updated = replacement.apply(&app, &saved).unwrap();
            let committed = serde_json::to_value(&updated).unwrap();
            assert!(competing.apply(&app, &updated).is_err());
            assert_eq!(serde_json::to_value(&updated).unwrap(), committed);
            assert!(replacement.apply(&app, &updated).is_err());
            let mut manual = saved;
            manual.meta.as_mut().unwrap().osir_binding = None;
            assert!(replacement.apply(&app, &manual).is_err());
            let mut reused = metadata;
            reused.reference = replacement_request(tool).expected_reference;
            assert!(TrustedKeyReplacement::new(replacement_request(tool), &reused).is_err());
        }
    }

    #[test]
    fn replace_key_requires_all_selected_models_in_new_native_catalog() {
        use crate::services::model_fetch::FetchedModel;
        let catalog = ["model-2", "model-3"].map(|id| FetchedModel {
            id: id.into(),
            owned_by: None,
        });
        for tool in ["codex", "claude", "gemini", "opencode"] {
            let mut req = replacement_request(tool);
            assert!(req.validate_catalog(&catalog).is_ok());
            assert!(req.validate_catalog(&catalog[..1]).is_err());
            assert!(req.validate_catalog(&[]).is_err());
            req.models.clear();
            assert!(req.validate_catalog(&catalog[..1]).is_ok());
            req.model = "model-from-old-key-only".into();
            let error = req.validate_catalog(&catalog).unwrap_err();
            assert!(!error.contains(&req.model));
            req.models = vec!["model-2".into()];
            assert!(req.validate().is_err());
        }
    }

    #[test]
    fn new_connection_models_must_belong_to_native_key_catalog() {
        use crate::services::model_fetch::FetchedModel;
        let catalog = ["model-1", "model-2"].map(|id| FetchedModel {
            id: id.into(),
            owned_by: None,
        });
        let mut req = request("codex");
        req.models = vec!["model-1".into(), "model-2".into()];
        assert!(req.validate_catalog(&catalog).is_ok());
        assert!(req.validate_catalog(&catalog[..1]).is_err());
        req.model = "model-from-another-key".into();
        assert!(req.validate_catalog(&catalog).is_err());
        req.model = "model-1".into();
        req.models = vec!["model-1".into(), "unavailable".into()];
        assert!(req.validate_catalog(&catalog).is_err());
    }

    #[test]
    fn replace_key_ipc_accepts_optional_catalog_but_not_binding_authority() {
        let input = json!({"tool":"codex","provider_id":"fixture","key_id":2,
            "name":"Renamed","model":"model-2","generation":2,
            "expected_reference":replacement_request("codex").expected_reference});
        let req: ReplaceConnectionKeyRequest = serde_json::from_value(input.clone()).unwrap();
        assert!(req.models.is_empty());
        assert!(req.validate().is_ok());
        for field in ["account_id", "group_id", "binding", "reference", "secret"] {
            let mut untrusted = input.clone();
            untrusted[field] = json!("untrusted");
            assert!(serde_json::from_value::<ReplaceConnectionKeyRequest>(untrusted).is_err());
        }
        let mut missing_compare = input;
        missing_compare
            .as_object_mut()
            .unwrap()
            .remove("expected_reference");
        assert!(serde_json::from_value::<ReplaceConnectionKeyRequest>(missing_compare).is_err());
        let (_, _, metadata) = replacement_fixture("codex");
        for field in ["key_id", "provider_id", "tool"] {
            let mut wrong = metadata.clone();
            match field {
                "key_id" => wrong.binding.key_id = 99,
                "provider_id" => wrong.binding.provider_id = "other-provider".into(),
                _ => wrong.binding.tool = KeyTool::GeminiCli,
            }
            assert!(TrustedKeyReplacement::new(replacement_request("codex"), &wrong).is_err());
        }
    }

    fn request(tool: &str) -> ConnectionRequest {
        ConnectionRequest {
            tool: tool.into(),
            key_id: 1,
            name: "OSIR".into(),
            hide_aggregation_name: None,
            model: "model-1".into(),
            models: Vec::new(),
            generation: 2,
        }
    }
    #[test]
    fn codex_aggregation_name_preference_defaults_hidden_and_is_display_only() {
        let input =
            json!({"tool":"codex","key_id":1,"name":"用户连接","model":"model-1","generation":2});
        let request: ConnectionRequest = serde_json::from_value(input.clone()).unwrap();
        let default = provider_row(&request, "stable-id".into()).unwrap();
        assert_eq!(
            default.meta.as_ref().unwrap().hide_aggregation_name,
            Some(true)
        );
        let mut explicit = input.clone();
        explicit["hideAggregationName"] = json!(false);
        let request: ConnectionRequest = serde_json::from_value(explicit.clone()).unwrap();
        let visible = provider_row(&request, "stable-id".into()).unwrap();
        assert_eq!(
            visible.meta.as_ref().unwrap().hide_aggregation_name,
            Some(false)
        );
        assert_eq!(visible.id, default.id);
        assert_eq!(visible.name, "用户连接");
        assert_eq!(visible.settings_config, default.settings_config);
        explicit["reference"] = json!("untrusted");
        assert!(serde_json::from_value::<ConnectionRequest>(explicit).is_err());
        for tool in ["claude", "gemini", "opencode"] {
            let mut request = super::tests::request(tool);
            request.hide_aggregation_name = Some(false);
            let row = provider_row(&request, "stable-id".into()).unwrap();
            assert_eq!(
                row.meta
                    .as_ref()
                    .and_then(|meta| meta.hide_aggregation_name),
                None
            );
        }
    }
    #[test]
    fn claude_desktop_rows_publish_every_selected_model_and_reject_oversized_catalogs() {
        let mut req = request("claude-desktop");
        req.model = "claude-sonnet-5-5".into();
        req.models = vec![
            "other-model".into(),
            req.model.clone(),
            "third-model".into(),
        ];
        let provider = provider_row(&req, "desktop-row".into()).unwrap();
        assert_eq!(req.validate().unwrap(), AppType::ClaudeDesktop);
        assert_eq!(
            provider.settings_config["env"]["ANTHROPIC_BASE_URL"],
            GATEWAY_ORIGIN
        );
        let routes = crate::claude_desktop_config::proxy_model_routes(&provider).unwrap();
        assert_eq!(routes.len(), 3);
        for model in &req.models {
            assert!(routes.iter().any(|route| &route.upstream_model == model));
        }
        assert!(routes
            .iter()
            .all(|route| crate::claude_desktop_config::is_claude_safe_model_id(&route.route_id)));
        assert_eq!(
            provider.meta.as_ref().unwrap().claude_desktop_mode,
            Some(crate::provider::ClaudeDesktopMode::Proxy)
        );
        assert!(!serde_json::to_string(&provider)
            .unwrap()
            .contains("ANTHROPIC_AUTH_TOKEN"));
        req.models = (0..201).map(|i| format!("model-{i}")).collect();
        req.model = req.models[0].clone();
        assert!(req.validate().is_err());
    }

    #[test]
    fn claude_desktop_rejects_image_output_models_but_keeps_vision_conversation_models() {
        for model in [
            "gpt-image-2",
            "vendor/gpt-image-1:latest",
            "dall-e-3",
            "grok-imagine-edit",
            "gemini-3.1-flash-image",
        ] {
            let mut req = request("claude-desktop");
            req.model = model.into();
            req.models = vec![model.into()];
            assert!(req.validate().is_err(), "{model}");
        }
        for model in [
            "claude-sonnet-5",
            "gpt-5.4",
            "gemini-3.1-pro",
            "vision-image-input-model",
        ] {
            assert!(desktop_conversation_model(model));
        }
    }

    #[test]
    fn claude_desktop_key_replacement_preserves_user_fields_and_rebuilds_all_model_routes() {
        let (app, saved, metadata) = replacement_fixture("claude-desktop");
        let updated = TrustedKeyReplacement::new(replacement_request("claude-desktop"), &metadata)
            .unwrap()
            .apply(&app, &saved)
            .unwrap();
        assert_eq!(updated.id, saved.id);
        assert_eq!(updated.settings_config["env"]["USER_SETTING"], "preserve");
        assert_eq!(
            updated.settings_config["mcpServers"],
            saved.settings_config["mcpServers"]
        );
        assert_eq!(
            updated.settings_config["userSetting"],
            saved.settings_config["userSetting"]
        );
        assert_eq!(
            updated
                .meta
                .as_ref()
                .unwrap()
                .osir_binding
                .as_ref()
                .unwrap()
                .binding
                .tool,
            KeyTool::ClaudeDesktop
        );
        let routes = crate::claude_desktop_config::proxy_model_routes(&updated).unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].upstream_model, "model-2");
        assert!(routes.iter().any(|route| route.upstream_model == "model-3"));
        super::super::managed::validate_managed_provider_shape(&app, &updated).unwrap();
        assert!(
            super::super::managed::validate_managed_provider_shape(&AppType::Claude, &updated)
                .is_err()
        );
    }

    #[test]
    fn four_tool_rows_have_fixed_endpoints_and_no_credentials() {
        for tool in ["codex", "claude", "opencode", "gemini"] {
            let row = provider_row(&request(tool), "fixture".into()).unwrap();
            let wire = serde_json::to_string(&row).unwrap();
            assert!(wire.contains("api.osirclaw.com"));
            assert!(!wire.contains("OPENAI_API_KEY"));
            assert!(!wire.contains("GEMINI_API_KEY"));
            assert!(!wire.contains("ANTHROPIC_AUTH_TOKEN"));
            assert!(!wire.contains("apiKey"));
        }
    }
    #[test]
    fn untrusted_model_cannot_inject_toml_settings() {
        let mut req = request("codex");
        req.model = "quoted\"model".into();
        let row = provider_row(&req, "fixture".into()).unwrap();
        let config: toml_edit::DocumentMut = row.settings_config["config"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(config["model"].as_str(), Some(req.model.as_str()));
        req.model = "bad\nmodel".into();
        assert!(req.validate().is_err());
        assert!(request("unknown").validate().is_err());
    }

    #[test]
    fn multiple_model_catalogs_are_validated_and_contain_no_credentials() {
        for tool in ["codex", "opencode", "claude", "gemini"] {
            let mut req = request(tool);
            req.models = vec!["model-1".into(), "model-2".into()];
            let row = provider_row(&req, "fixture".into()).unwrap();
            match tool {
                "codex" => {
                    assert_eq!(
                        row.settings_config["modelCatalog"]["models"],
                        json!([{"model":"model-1"},{"model":"model-2"}])
                    );
                    let config: toml_edit::DocumentMut = row.settings_config["config"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap();
                    assert_eq!(config["model"].as_str(), Some("model-1"));
                }
                "opencode" => assert_eq!(
                    row.settings_config["models"],
                    json!({"model-1":{"name":"model-1"},"model-2":{"name":"model-2"}})
                ),
                _ => assert_eq!(
                    row.settings_config,
                    provider_row(&request(tool), "fixture".into())
                        .unwrap()
                        .settings_config
                ),
            }
            let wire = serde_json::to_string(&row).unwrap();
            for forbidden in [
                "apiKey",
                "OPENAI_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "GEMINI_API_KEY",
            ] {
                assert!(!wire.contains(forbidden));
            }
            for invalid in [
                vec!["other".into()],
                vec!["model-1".into(), "".into()],
                vec!["model-1".into(), "bad\nmodel".into()],
                vec!["model-1".into(), " model-2".into()],
                vec!["model-1".into(), "x".repeat(257)],
                vec!["model-1".into(); 1001],
            ] {
                req.models = invalid;
                assert!(req.validate().is_err());
            }
            req.models = vec!["model-1".into(); 1000];
            assert!(req.validate().is_ok());
        }
        let req: ConnectionRequest = serde_json::from_value(
            json!({"tool":"codex","key_id":1,"name":"OSIR","model":"model-1","generation":2}),
        )
        .unwrap();
        assert!(req.models.is_empty());
        assert!(req.validate().is_ok());
    }
}
