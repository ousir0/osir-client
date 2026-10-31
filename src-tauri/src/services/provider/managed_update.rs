//! Validate only against rows freshly read under the application write lock.
use crate::{app_config::AppType, error::AppError, provider::Provider, services::osir::managed};
const MANAGED_BINDING_ERROR: &str = "OSIR managed update rejected; reload the saved provider";
fn invalid() -> AppError {
    AppError::InvalidInput(MANAGED_BINDING_ERROR.into())
}
fn binding(p: &Provider) -> Option<&crate::provider::ManagedBinding> {
    p.meta.as_ref()?.osir_binding.as_ref()
}

pub(super) fn recheck_expected_base(
    app: &AppType,
    saved: Option<&Provider>,
    expected: Option<&Provider>,
) -> Result<(), AppError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let saved = saved.ok_or_else(invalid)?;
    let snapshot = |p: &Provider| -> Result<serde_json::Value, AppError> {
        let mut p = p.clone();
        if binding(&p).is_some() {
            managed::validate_provider(app, &p).map_err(|_| invalid())?;
            p.settings_config =
                managed::sanitize_settings(app, &p, &p.settings_config).map_err(|_| invalid())?;
        }
        serde_json::to_value(p).map_err(|_| invalid())
    };
    if snapshot(saved)? != snapshot(expected)? {
        return Err(AppError::InvalidInput(
            "连接设置已变化，请刷新后重试。".into(),
        ));
    }
    Ok(())
}
/// Compare only against the native row freshly read under the switch lock.
/// expected is a concurrency token; it is never used to load a credential.
pub(super) fn recheck_replacement(
    app: &AppType,
    saved: &Provider,
    expected: &crate::services::osir::key_vault::KeyReference,
) -> Result<(), AppError> {
    managed::validate_managed_provider_shape(app, saved).map_err(|_| invalid())?;
    let old = binding(saved).ok_or_else(invalid)?;
    if &old.reference != expected {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn recheck_update(
    app: &AppType,
    original_id: &str,
    saved: Option<&Provider>,
    target: Option<&Provider>,
    mut incoming: Provider,
) -> Result<Provider, AppError> {
    if original_id != incoming.id && target.and_then(binding).is_some() {
        return Err(invalid());
    }
    match (saved.and_then(binding), binding(&incoming)) {
        (None, None) => return Ok(incoming),
        (Some(old), Some(new))
            if old == new
                && original_id == incoming.id
                && saved.is_some_and(|p| p.id == incoming.id) => {}
        _ => return Err(invalid()),
    }
    let saved = saved.ok_or_else(invalid)?;
    let mut incoming_meta = incoming.meta.clone();
    let mut saved_meta = saved.meta.clone();
    // This single presentation option cannot change credential authority or
    // routing. Every other metadata field, including the binding, stays exact.
    if matches!(app, AppType::Codex) {
        if let Some(meta) = incoming_meta.as_mut() {
            meta.hide_aggregation_name = None;
        }
        if let Some(meta) = saved_meta.as_mut() {
            meta.hide_aggregation_name = None;
        }
    }
    if incoming.category != saved.category
        || serde_json::to_value(incoming_meta).map_err(|_| invalid())?
            != serde_json::to_value(saved_meta).map_err(|_| invalid())?
    {
        return Err(invalid());
    }
    managed::validate_provider(app, &incoming).map_err(|_| invalid())?;
    incoming.settings_config =
        managed::sanitize_settings(app, &incoming, &incoming.settings_config)
            .map_err(|_| invalid())?;
    managed::validate_managed_provider_shape(app, &incoming).map_err(|_| invalid())?;
    let baseline =
        managed::sanitize_settings(app, saved, &saved.settings_config).map_err(|_| invalid())?;
    if non_model_settings(app, &baseline).map_err(|_| invalid())?
        != non_model_settings(app, &incoming.settings_config).map_err(|_| invalid())?
    {
        return Err(invalid());
    }
    Ok(incoming)
}
// Managed edits must use ordinary update; EditorSave can merge shared live fields.
pub(super) fn recheck_editor(
    saved: Option<&Provider>,
    incoming: &Provider,
) -> Result<(), AppError> {
    if saved.and_then(binding).is_some() || binding(incoming).is_some() {
        return Err(invalid());
    }
    Ok(())
}
// Native creation is allowed only without replacing any existing managed identity.
pub(super) fn recheck_add(saved: Option<&Provider>, incoming: &Provider) -> Result<(), AppError> {
    if saved.and_then(binding).is_some() || (saved.is_some() && binding(incoming).is_some()) {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn recheck_endpoint(saved: Option<&Provider>) -> Result<(), AppError> {
    if saved.and_then(binding).is_some() {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn lock_open_code(
    state: &crate::store::AppState,
    app: &AppType,
) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    matches!(app, AppType::OpenCode)
        .then(|| futures::executor::block_on(state.proxy_service.lock_switch_for_app(app.as_str())))
}
pub(crate) fn non_model_settings(
    app: &AppType,
    settings: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut value = settings.clone();
    let invalid = || MANAGED_BINDING_ERROR.to_string();
    let root = value.as_object_mut().ok_or_else(invalid)?;
    match app {
        AppType::Claude | AppType::Gemini => {
            if let Some(env) = root
                .get_mut("env")
                .and_then(serde_json::Value::as_object_mut)
            {
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
                for field in fields {
                    if let Some(model) = env.remove(*field) {
                        if !model.is_string() {
                            return Err(invalid());
                        }
                    }
                }
                if matches!(app, AppType::Claude) {
                    for (field, minimum, maximum) in [
                        ("CLAUDE_CODE_MAX_CONTEXT_TOKENS", 1, 2_000_000),
                        ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", 100_000, 1_000_000),
                    ] {
                        if let Some(value) = env.remove(field) {
                            checked_limit(&value, minimum, maximum, true)?;
                        }
                    }
                }
            }
            if matches!(app, AppType::Gemini) {
                if let Some(config) = root.get_mut("config") {
                    let config = config.as_object_mut().ok_or_else(invalid)?;
                    if let Some(experimental) = config.get_mut("experimental") {
                        let experimental = experimental.as_object_mut().ok_or_else(invalid)?;
                        if let Some(enabled) = experimental.remove("contextManagement") {
                            if !enabled.is_boolean() {
                                return Err(invalid());
                            }
                        }
                        if experimental.is_empty() {
                            config.remove("experimental");
                        }
                    }
                    if let Some(context) = config.get_mut("contextManagement") {
                        let context = context.as_object_mut().ok_or_else(invalid)?;
                        if let Some(history) = context.get_mut("historyWindow") {
                            let history = history.as_object_mut().ok_or_else(invalid)?;
                            if let Some(value) = history.remove("maxTokens") {
                                checked_limit(&value, 1, 2_000_000, false)?;
                            }
                            if history.is_empty() {
                                context.remove("historyWindow");
                            }
                        }
                        if context.is_empty() {
                            config.remove("contextManagement");
                        }
                    }
                    if config.is_empty() {
                        root.remove("config");
                    }
                }
            }
        }
        AppType::Codex => {
            let config = root
                .get("config")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(invalid)?;
            let mut doc: toml::Value = config.parse().map_err(|_| invalid())?;
            let table = doc.as_table_mut().ok_or_else(invalid)?;
            for field in ["model", "model_reasoning_effort"] {
                if let Some(model) = table.remove(field) {
                    if !model.is_str() {
                        return Err(invalid());
                    }
                }
            }
            for field in ["model_context_window", "model_auto_compact_token_limit"] {
                if let Some(value) = table.remove(field) {
                    let value = value
                        .as_integer()
                        .filter(|value| *value > 0)
                        .ok_or_else(invalid)?;
                    if value > 9_007_199_254_740_991 {
                        return Err(invalid());
                    }
                }
            }
            root.insert(
                "config".into(),
                serde_json::to_value(doc).map_err(|_| invalid())?,
            );
            if let Some(catalog) = root.get_mut("modelCatalog") {
                let catalog = catalog.as_object_mut().ok_or_else(invalid)?;
                if let Some(models) = catalog.remove("models") {
                    let mut remainder = serde_json::Map::new();
                    let mut seen = std::collections::HashSet::new();
                    for model in models.as_array().ok_or_else(invalid)? {
                        let mut model = model.as_object().ok_or_else(invalid)?.clone();
                        let id = model
                            .remove("model")
                            .and_then(|v| v.as_str().map(str::to_owned))
                            .ok_or_else(invalid)?;
                        if !seen.insert(id.clone()) {
                            return Err(invalid());
                        }
                        if let Some(name) = model.remove("name") {
                            if !name.is_string() {
                                return Err(invalid());
                            }
                        }
                        for field in ["contextWindow", "context_window"] {
                            if let Some(value) = model.remove(field) {
                                checked_limit(&value, 1, 9_007_199_254_740_991, false)?;
                            }
                        }
                        if model
                            .keys()
                            .any(|key| matches!(key.as_str(), "headers" | "options" | "command"))
                        {
                            return Err(invalid());
                        }
                        if !model.is_empty() {
                            remainder.insert(id, serde_json::Value::Object(model));
                        }
                    }
                    if !remainder.is_empty() {
                        catalog.insert("models".into(), serde_json::Value::Object(remainder));
                    }
                }
            }
        }
        AppType::OpenCode => {
            if let Some(models) = root.remove("models") {
                let mut remainder = serde_json::Map::new();
                for (id, model) in models.as_object().ok_or_else(invalid)? {
                    let mut model = model.as_object().ok_or_else(invalid)?.clone();
                    for field in ["name", "modelID", "id"] {
                        if let Some(value) = model.remove(field) {
                            if !value.is_string() {
                                return Err(invalid());
                            }
                        }
                    }
                    if let Some(limits) = model.get_mut("limit") {
                        let limits = limits.as_object_mut().ok_or_else(invalid)?;
                        for field in ["context", "output"] {
                            if let Some(value) = limits.remove(field) {
                                checked_limit(&value, 1, 9_007_199_254_740_991, false)?;
                            }
                        }
                        if limits.is_empty() {
                            model.remove("limit");
                        }
                    }
                    if model
                        .keys()
                        .any(|key| matches!(key.as_str(), "headers" | "options" | "command"))
                    {
                        return Err(invalid());
                    }
                    if !model.is_empty() {
                        remainder.insert(id.clone(), serde_json::Value::Object(model));
                    }
                }
                if !remainder.is_empty() {
                    root.insert("models".into(), serde_json::Value::Object(remainder));
                }
            }
        }
        _ => return Err(invalid()),
    }
    Ok(value)
}

fn checked_limit(
    value: &serde_json::Value,
    minimum: u64,
    maximum: u64,
    allow_string: bool,
) -> Result<(), String> {
    let number = value.as_u64().or_else(|| {
        allow_string
            .then(|| value.as_str())
            .flatten()
            .filter(|value| !value.is_empty() && value.bytes().all(|ch| ch.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
    });
    match number {
        Some(value) if (minimum..=maximum).contains(&value) => Ok(()),
        _ => Err(MANAGED_BINDING_ERROR.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Provider {
        let mut p = Provider::with_id(
            "fixture".into(),
            "Fixture".into(),
            json!({"env":{"ANTHROPIC_BASE_URL":"https://api.osirclaw.com","ANTHROPIC_MODEL":"model-a"}}),
            None,
        );
        p.meta = Some(serde_json::from_value(json!({"osirBinding":{
            "source":"managed_account", "masked_key":"masked-fixture",
            "reference":{"version":1,"id":"6c0ab1ea-f36b-4d4d-a8af-098ad95b6f02"},
            "binding":{"account_origin":"https://osirclaw.com","account_id":"fixture-account","key_id":1,"group_id":2,"tool":"claude_code","provider_id":"fixture","gateway_origin":"https://api.osirclaw.com"}
        }})).unwrap());
        p
    }
    #[test]
    fn latest_binding_wins_over_stale_command_snapshot() {
        let old = fixture();
        let mut latest = old.clone();
        latest
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .binding
            .key_id = 2;
        assert!(
            recheck_update(&AppType::Claude, &old.id, Some(&latest), None, old.clone()).is_err()
        );
        assert!(recheck_update(&AppType::Claude, &old.id, None, None, old.clone()).is_err());
        let mut manual = old.clone();
        manual.meta = None;
        assert!(recheck_update(
            &AppType::Claude,
            &old.id,
            Some(&latest),
            None,
            manual.clone()
        )
        .is_err());
        assert!(
            recheck_update(&AppType::Claude, &old.id, Some(&manual), None, old.clone()).is_err()
        );
    }
    #[test]
    fn latest_non_model_fields_cannot_be_overwritten_by_stale_edit() {
        let old = fixture();
        let mut latest = old.clone();
        latest.settings_config["env"]["NEW_SETTING"] = json!("new");
        assert!(
            recheck_update(&AppType::Claude, &old.id, Some(&latest), None, old.clone()).is_err()
        );
        let mut edited = old.clone();
        edited.settings_config["env"]["ANTHROPIC_MODEL"] = json!("model-b");
        let safe = recheck_update(&AppType::Claude, &old.id, Some(&old), None, edited).unwrap();
        assert_eq!(safe.settings_config["env"]["ANTHROPIC_MODEL"], "model-b");
    }

    #[test]
    fn context_edits_preserve_model_capabilities_and_reject_invalid_limits() {
        let old = json!({"config":"model = 'a'\n", "modelCatalog":{"models":[{"model":"a","inputModalities":["text","image"]}]}});
        let mut edited = old.clone();
        edited["config"] = json!(
            "model = 'a'\nmodel_context_window = 272000\nmodel_auto_compact_token_limit = 250000\n"
        );
        edited["modelCatalog"]["models"][0]["contextWindow"] = json!(272000);
        assert_eq!(
            non_model_settings(&AppType::Codex, &old).unwrap(),
            non_model_settings(&AppType::Codex, &edited).unwrap()
        );
        let mut changed_capability = edited.clone();
        changed_capability["modelCatalog"]["models"][0]["inputModalities"] = json!(["text"]);
        assert_ne!(
            non_model_settings(&AppType::Codex, &old).unwrap(),
            non_model_settings(&AppType::Codex, &changed_capability).unwrap()
        );
        edited["modelCatalog"]["models"][0]["contextWindow"] = json!(-1);
        assert!(non_model_settings(&AppType::Codex, &edited).is_err());
        for settings in [
            json!({"env":{"CLAUDE_CODE_MAX_CONTEXT_TOKENS":"2000001"}}),
            json!({"env":{"CLAUDE_CODE_AUTO_COMPACT_WINDOW":"99999"}}),
            json!({"env":{"CLAUDE_CODE_MAX_CONTEXT_TOKENS":{"command":"bad"}}}),
        ] {
            assert!(non_model_settings(&AppType::Claude, &settings).is_err());
        }
        assert!(non_model_settings(
            &AppType::Gemini,
            &json!({"config":{"experimental":{"contextManagement":"true"}}})
        )
        .is_err());
        assert!(non_model_settings(
            &AppType::OpenCode,
            &json!({"models":{"a":{"limit":{"context":1.5}}}})
        )
        .is_err());
    }

    #[test]
    fn expected_base_compares_sanitized_latest_model_and_context() {
        let saved = fixture();
        let mut expected = saved.clone();
        expected.settings_config["env"]["ANTHROPIC_API_KEY"] = json!("synthetic-only");
        recheck_expected_base(&AppType::Claude, Some(&saved), Some(&expected)).unwrap();
        let mut latest = saved.clone();
        latest.settings_config["env"]["ANTHROPIC_MODEL"] = json!("new-model");
        assert!(recheck_expected_base(&AppType::Claude, Some(&latest), Some(&expected)).is_err());
        latest = saved.clone();
        latest.settings_config["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"] = json!("100000");
        assert!(recheck_expected_base(&AppType::Claude, Some(&latest), Some(&expected)).is_err());
        assert!(recheck_expected_base(&AppType::Claude, None, Some(&expected)).is_err());
    }
    #[test]
    fn rename_copy_and_managed_target_are_rejected() {
        let p = fixture();
        let mut renamed = p.clone();
        renamed.id = "copy".into();
        assert!(recheck_update(&AppType::Claude, &p.id, Some(&p), None, renamed).is_err());
        let mut manual = p.clone();
        manual.meta = None;
        assert!(recheck_update(
            &AppType::Claude,
            "other",
            Some(&manual),
            Some(&p),
            manual.clone()
        )
        .is_err());
        assert!(recheck_add(Some(&p), &manual).is_err());
        assert!(recheck_add(Some(&manual), &p).is_err());
        assert!(recheck_add(None, &p).is_ok());
    }
    #[test]
    fn every_editor_guard_rejects_managed_source_or_draft() {
        let managed = fixture();
        let mut manual = managed.clone();
        manual.meta = None;
        assert!(recheck_editor(Some(&managed), &manual).is_err());
        assert!(recheck_editor(Some(&manual), &managed).is_err());
        assert!(recheck_editor(None, &managed).is_err());
        assert!(recheck_editor(Some(&manual), &manual).is_ok());
        assert!(recheck_endpoint(Some(&managed)).is_err());
        assert!(recheck_endpoint(Some(&manual)).is_ok());
        assert!(recheck_update(
            &AppType::Claude,
            &manual.id,
            Some(&manual),
            None,
            manual.clone()
        )
        .is_ok());
    }
    #[test]
    fn model_fields_reject_executable_payloads() {
        assert!(non_model_settings(
            &AppType::Claude,
            &json!({"env":{"ANTHROPIC_MODEL":{"exec":"payload"}}})
        )
        .is_err());
        assert!(non_model_settings(
            &AppType::OpenCode,
            &json!({"models":{"model":{"options":{"apiKey":"fixture"}}}})
        )
        .is_err());
        assert!(non_model_settings(&AppType::Codex, &json!({"config":"model = 1"})).is_err());
    }
    #[test]
    fn four_tools_accept_only_model_edits_and_keep_binding() {
        use crate::services::osir::key_vault::KeyTool;
        let cases = [
            (
                AppType::Claude,
                KeyTool::ClaudeCode,
                json!({"env":{"ANTHROPIC_BASE_URL":"https://api.osirclaw.com","ANTHROPIC_MODEL":"a"}}),
                json!({"env":{"ANTHROPIC_BASE_URL":"https://api.osirclaw.com","ANTHROPIC_MODEL":"b"}}),
            ),
            (
                AppType::Gemini,
                KeyTool::GeminiCli,
                json!({"env":{"GOOGLE_GEMINI_BASE_URL":"https://api.osirclaw.com","GEMINI_MODEL":"a"}}),
                json!({"env":{"GOOGLE_GEMINI_BASE_URL":"https://api.osirclaw.com","GEMINI_MODEL":"b"}}),
            ),
            (
                AppType::OpenCode,
                KeyTool::OpenCode,
                json!({"npm":"@ai-sdk/openai-compatible","options":{"baseURL":"https://api.osirclaw.com/v1"},"models":{"a":{"name":"a"}}}),
                json!({"npm":"@ai-sdk/openai-compatible","options":{"baseURL":"https://api.osirclaw.com/v1"},"models":{"b":{"name":"b"}}}),
            ),
            (
                AppType::Codex,
                KeyTool::Codex,
                json!({"auth":{},"config":"model = \"a\"\nmodel_provider = \"osir\"\n[model_providers.osir]\nbase_url = \"https://api.osirclaw.com/v1\"\n"}),
                json!({"auth":{},"config":"model = \"b\"\nmodel_provider = \"osir\"\n[model_providers.osir]\nbase_url = \"https://api.osirclaw.com/v1\"\n"}),
            ),
        ];
        for (app, tool, before, after) in cases {
            let mut saved = fixture();
            saved
                .meta
                .as_mut()
                .unwrap()
                .osir_binding
                .as_mut()
                .unwrap()
                .binding
                .tool = tool;
            saved.settings_config = before;
            let mut incoming = saved.clone();
            incoming.settings_config = after;
            let updated = recheck_update(&app, &saved.id, Some(&saved), None, incoming).unwrap();
            assert_eq!(
                updated.meta.as_ref().unwrap().osir_binding,
                saved.meta.as_ref().unwrap().osir_binding
            );
            let mut attack = updated.clone();
            attack.settings_config["command"] = json!("untrusted hook");
            assert!(recheck_update(&app, &saved.id, Some(&saved), None, attack).is_err());
            let mut attack = updated.clone();
            attack.category = Some("other".into());
            assert!(recheck_update(&app, &saved.id, Some(&saved), None, attack).is_err());
        }
    }
    #[test]
    fn managed_update_sanitizes_secrets_and_rejects_route_or_metadata_changes() {
        let saved = fixture();
        let mut incoming = saved.clone();
        incoming.settings_config["env"]["ANTHROPIC_API_KEY"] = json!("fixture-only-secret");
        let result =
            recheck_update(&AppType::Claude, &saved.id, Some(&saved), None, incoming).unwrap();
        assert!(result.settings_config["env"]
            .get("ANTHROPIC_API_KEY")
            .is_none());
        let mut attack = saved.clone();
        attack.settings_config["env"]["ANTHROPIC_BASE_URL"] = json!("https://untrusted.invalid");
        assert!(recheck_update(&AppType::Claude, &saved.id, Some(&saved), None, attack).is_err());
        let mut attack = saved.clone();
        attack
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .masked_key = "replaced".into();
        assert!(recheck_update(&AppType::Claude, &saved.id, Some(&saved), None, attack).is_err());
    }

    #[test]
    fn managed_codex_update_accepts_only_aggregation_display_metadata() {
        let mut saved = fixture();
        saved
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .binding
            .tool = crate::services::osir::key_vault::KeyTool::Codex;
        saved.settings_config = json!({"auth":{},"config":"model = \"a\"\nmodel_provider = \"osir\"\n[model_providers.osir]\nbase_url = \"https://api.osirclaw.com/v1\"\n"});
        let mut incoming = saved.clone();
        incoming.meta.as_mut().unwrap().hide_aggregation_name = Some(false);
        let result = recheck_update(
            &AppType::Codex,
            &saved.id,
            Some(&saved),
            None,
            incoming.clone(),
        )
        .unwrap();
        assert_eq!(
            result.meta.as_ref().unwrap().hide_aggregation_name,
            Some(false)
        );
        assert_eq!(
            result.meta.as_ref().unwrap().osir_binding,
            saved.meta.as_ref().unwrap().osir_binding
        );
        let settings_semantics = |settings: &serde_json::Value| {
            let mut value = settings.clone();
            let config: toml::Value = settings["config"].as_str().unwrap().parse().unwrap();
            value["config"] = serde_json::to_value(config).unwrap();
            value
        };
        assert_eq!(
            settings_semantics(&result.settings_config),
            settings_semantics(&saved.settings_config)
        );
        incoming.meta.as_mut().unwrap().api_format = Some("anthropic".into());
        assert!(recheck_update(&AppType::Codex, &saved.id, Some(&saved), None, incoming).is_err());
        let mut changed_binding = result;
        changed_binding
            .meta
            .as_mut()
            .unwrap()
            .osir_binding
            .as_mut()
            .unwrap()
            .binding
            .key_id = 99;
        assert!(recheck_update(
            &AppType::Codex,
            &saved.id,
            Some(&saved),
            None,
            changed_binding
        )
        .is_err());
        let claude = fixture();
        let mut changed = claude.clone();
        changed.meta.as_mut().unwrap().hide_aggregation_name = Some(false);
        assert!(
            recheck_update(&AppType::Claude, &claude.id, Some(&claude), None, changed).is_err()
        );
    }
}
