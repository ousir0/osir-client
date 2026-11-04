//! DSH credentials are local-only. Provider rows carry opaque references so
//! neither renderer reads nor cloud synchronization materialize model keys.
use crate::{
    app_config::AppType, database::Database, error::AppError, provider::Provider, store::AppState,
};
use indexmap::IndexMap;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
const SERVICE: &str = "osir.dsh.model-key.v1";
fn unavailable() -> AppError {
    AppError::Config("DeepSeek Harness 连接暂不可用，请重新读取配置".into())
}
fn reference(provider: &Provider) -> Option<&str> {
    provider
        .settings_config
        .get("_osirDshKey")
        .and_then(Value::as_str)
}

/// A caller cannot attach another row's credential reference or change its ID.
/// This runs under the same switch lock as persistence and projection.
pub(crate) fn check_reference(
    saved: Option<&Provider>,
    incoming: &Provider,
) -> Result<(), AppError> {
    if let Some(key) = reference(incoming) {
        if !saved.is_some_and(|old| old.id == incoming.id && reference(old) == Some(key)) {
            return Err(unavailable());
        }
    }
    Ok(())
}
pub(crate) fn materialize(db: &Database, provider: &Provider) -> Result<Provider, AppError> {
    if provider
        .meta
        .as_ref()
        .is_some_and(|m| m.osir_binding.is_some())
    {
        return super::managed::resolve_provider(&AppType::Dsh, provider);
    }
    let mut row = provider.clone();
    // New manual input takes precedence; saved references are checked by the
    // write boundary before this function is called.
    if row
        .settings_config
        .pointer("/options/apiKey")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        if let Some(reference) = reference(provider) {
            let conn = db.conn.lock().map_err(|_| unavailable())?;
            let secret = conn
                .query_row(
                    "SELECT payload FROM osir_model_credentials WHERE service=?1 AND account=?2",
                    params![SERVICE, reference],
                    |r| r.get::<_, Option<Vec<u8>>>(0),
                )
                .optional()
                .map_err(|_| unavailable())?
                .flatten();
            row.settings_config["options"]["apiKey"] = json!(String::from_utf8(
                secret.ok_or_else(unavailable)?
            )
            .map_err(|_| unavailable())?);
        }
    }
    Ok(row)
}
pub(crate) fn prepare_row(db: &Database, provider: &Provider) -> Result<Provider, AppError> {
    let mut row = provider.clone();
    let key = row
        .settings_config
        .get_mut("options")
        .and_then(Value::as_object_mut)
        .and_then(|o| o.remove("apiKey"));
    if let Some(secret) = key
        .as_ref()
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
    {
        let reference = uuid::Uuid::new_v4().to_string();
        db.conn
            .lock()
            .map_err(|_| unavailable())?
            .execute(
                "INSERT INTO osir_model_credentials(service,account,payload) VALUES(?1,?2,?3)",
                params![SERVICE, reference, secret.as_bytes()],
            )
            .map_err(|_| unavailable())?;
        row.settings_config["_osirDshKey"] = json!(reference);
        // Immutable refs preserve the old connection if saving the row fails.
    }
    Ok(row)
}
pub(crate) fn save_row(db: &Database, provider: &Provider) -> Result<(), AppError> {
    db.save_provider("dsh", &prepare_row(db, provider)?)
}
/// Saved credentials may only be used with the saved route. An edited endpoint
/// requires an explicit replacement key, never an implicit credential transfer.
pub(crate) fn model_request_key(
    db: &Database,
    id: &str,
    endpoint: &str,
    api: &str,
) -> Result<String, AppError> {
    let saved = db.get_provider_by_id(id, "dsh")?.ok_or_else(unavailable)?;
    if saved
        .settings_config
        .pointer("/options/baseURL")
        .and_then(Value::as_str)
        != Some(endpoint)
        || saved.settings_config.get("api").and_then(Value::as_str) != Some(api)
    {
        return Err(AppError::Config(
            "服务地址或协议已修改，请填写密钥后获取模型。".into(),
        ));
    }
    materialize(db, &saved)?
        .settings_config
        .pointer("/options/apiKey")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(unavailable)
}
pub(crate) fn list(state: &AppState) -> Result<IndexMap<String, Provider>, AppError> {
    let _guard = futures::executor::block_on(state.proxy_service.lock_switch_for_app("dsh"));
    let native = crate::dsh_config::get_providers()?;
    let mut rows = state.db.get_all_providers("dsh")?;
    let selection = crate::dsh_config::current_selection().ok().flatten();
    for row in rows.values_mut() {
        if row
            .settings_config
            .pointer("/options/apiKey")
            .and_then(Value::as_str)
            .is_some_and(|key| !key.is_empty())
        {
            save_row(&state.db, row)?;
            *row = state
                .db
                .get_provider_by_id(&row.id, "dsh")?
                .ok_or_else(unavailable)?;
        }
    }
    for (id, config) in &native {
        // Import a previously unseen native connection once. Reads never
        // replace saved edits or account bindings with externally changed data.
        if rows.contains_key(id) {
            continue;
        }
        let mut row = Provider::with_id(
            id.clone(),
            config["name"].as_str().unwrap_or(id).into(),
            config.clone(),
            None,
        );
        if id == "deepseek-official" {
            row.category = Some("official".into());
        }
        if let Some((selected, model)) = &selection {
            if selected == id {
                row.settings_config["defaultModel"] = json!(model);
            }
        }
        save_row(&state.db, &row)?;
        rows.insert(
            id.clone(),
            state
                .db
                .get_provider_by_id(id, "dsh")?
                .ok_or_else(unavailable)?,
        );
    }
    for (id, row) in &mut rows {
        row.meta
            .get_or_insert_with(Default::default)
            .live_config_managed = Some(native.contains_key(id));
    }
    Ok(rows)
}
pub(crate) fn current_provider(state: &AppState) -> Result<String, AppError> {
    let _guard = futures::executor::block_on(state.proxy_service.lock_switch_for_app("dsh"));
    let Some((id, model)) = crate::dsh_config::current_selection()? else {
        return Ok(String::new());
    };
    let Some(saved) = state.db.get_provider_by_id(&id, "dsh")? else {
        return Ok(String::new());
    };
    let resolved = materialize(&state.db, &saved)?;
    let native = crate::dsh_config::get_providers()?;
    let Some(native) = native.get(&id) else {
        return Ok(String::new());
    };
    let config = &resolved.settings_config;
    if native["api"] == config["api"]
        && native.pointer("/options/baseURL") == config.pointer("/options/baseURL")
        && native.pointer("/options/apiKey") == config.pointer("/options/apiKey")
        && native["models"].get(&model).is_some()
        && config["models"].get(&model).is_some()
        && config["defaultModel"]
            .as_str()
            .is_none_or(|selected| selected == model)
    {
        Ok(id)
    } else {
        Ok(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: &str) -> Provider {
        Provider::with_id(
            id.into(),
            id.into(),
            json!({"api":"openai-completions","options":{"baseURL":"http://127.0.0.1:9/v1","apiKey":"synthetic-key"},"models":{"fixture":{}},"defaultModel":"fixture"}),
            None,
        )
    }
    #[test]
    fn dsh_local_credentials_never_enter_provider_rows_and_restore_is_immediate() {
        let db = Database::memory().unwrap();
        save_row(&db, &row("fixture")).unwrap();
        let saved = db.get_provider_by_id("fixture", "dsh").unwrap().unwrap();
        assert!(!serde_json::to_string(&saved)
            .unwrap()
            .contains("synthetic-key"));
        assert_eq!(
            materialize(&db, &saved).unwrap().settings_config["options"]["apiKey"],
            "synthetic-key"
        );
        db.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE osir_model_credentials SET payload=?1 WHERE service=?2",
                params![b"restored-key".as_slice(), SERVICE],
            )
            .unwrap();
        assert_eq!(
            materialize(&db, &saved).unwrap().settings_config["options"]["apiKey"],
            "restored-key"
        );
    }
    #[test]
    fn dsh_saved_model_lookup_rejects_changed_endpoint_or_protocol() {
        let db = Database::memory().unwrap();
        save_row(&db, &row("fixture")).unwrap();
        assert_eq!(
            model_request_key(
                &db,
                "fixture",
                "http://127.0.0.1:9/v1",
                "openai-completions"
            )
            .unwrap(),
            "synthetic-key"
        );
        assert!(model_request_key(
            &db,
            "fixture",
            "https://other.example/v1",
            "openai-completions"
        )
        .is_err());
        assert!(model_request_key(
            &db,
            "fixture",
            "http://127.0.0.1:9/v1",
            "anthropic-messages"
        )
        .is_err());
    }
    #[test]
    fn dsh_credential_reference_cannot_be_forged_or_moved_to_another_provider() {
        let db = Database::memory().unwrap();
        save_row(&db, &row("first")).unwrap();
        let saved = db.get_provider_by_id("first", "dsh").unwrap().unwrap();
        assert!(check_reference(Some(&saved), &saved).is_ok());
        assert!(check_reference(None, &saved).is_err());
        let mut attack = saved.clone();
        attack.id = "other".into();
        assert!(check_reference(Some(&saved), &attack).is_err());
        attack = saved.clone();
        attack.settings_config["_osirDshKey"] = json!("another-ref");
        assert!(check_reference(Some(&saved), &attack).is_err());
    }
}
