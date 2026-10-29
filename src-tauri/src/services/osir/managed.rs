//! Native materialization boundary. Returned providers MUST NOT be persisted,
//! logged or returned over IPC. Resolve only after common-config/live merging.
use std::sync::OnceLock;

use serde_json::{json, Value};

use super::{
    auth::AuthConfig,
    credentials::CredentialBackend,
    key_vault::{KeyTool, KeyVault, GATEWAY_ORIGIN},
};
pub use crate::provider::ManagedBinding;
use crate::{
    app_config::AppType,
    error::AppError,
    provider::{AuthBindingSource, Provider},
};

static IDENTITY: OnceLock<String> = OnceLock::new();
fn invalid() -> AppError {
    AppError::Config("OSIR managed credential configuration is invalid or unavailable".into())
}

/// Call from native setup with app.config().identifier, never from IPC input.
/// Does not depend on account login and must not be reset on logout.
pub fn initialize(identity: &str) -> Result<(), AppError> {
    KeyVault::new(&trusted_account_origin()?, identity).map_err(|_| invalid())?;
    if let Some(current) = IDENTITY.get() {
        return if current == identity {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    IDENTITY.set(identity.to_owned()).map_err(|_| invalid())
}
pub fn trusted_account_origin() -> Result<String, AppError> {
    Ok(AuthConfig::default()
        .validate()
        .map_err(|_| invalid())?
        .origin())
}
pub fn vault() -> Result<KeyVault, AppError> {
    KeyVault::new(
        &trusted_account_origin()?,
        IDENTITY.get().ok_or_else(invalid)?,
    )
    .map_err(|_| invalid())
}
pub fn gateway(app: &AppType) -> Result<&'static str, AppError> {
    match app {
        AppType::Codex | AppType::OpenCode => Ok("https://api.osirclaw.com/v1"),
        AppType::Claude | AppType::ClaudeDesktop | AppType::Gemini => Ok(GATEWAY_ORIGIN),
        _ => Err(invalid()),
    }
}
fn tool(app: &AppType) -> Result<KeyTool, AppError> {
    match app {
        AppType::Codex => Ok(KeyTool::Codex),
        AppType::Claude => Ok(KeyTool::ClaudeCode),
        AppType::ClaudeDesktop => Ok(KeyTool::ClaudeDesktop),
        AppType::OpenCode => Ok(KeyTool::OpenCode),
        AppType::Gemini => Ok(KeyTool::GeminiCli),
        _ => Err(invalid()),
    }
}
fn binding(provider: &Provider) -> Option<&ManagedBinding> {
    provider.meta.as_ref()?.osir_binding.as_ref()
}
/// Safe metadata/config validation, also usable before creating a vault record.
/// Account/key/group fields are checked against the immutable vault record on load.
pub fn validate_provider(app: &AppType, provider: &Provider) -> Result<(), AppError> {
    let Some(managed) = binding(provider) else {
        return Ok(());
    };
    let b = &managed.binding;
    if managed.source != AuthBindingSource::ManagedAccount
        || b.tool != tool(app)?
        || b.provider_id != provider.id
        || b.account_origin != trusted_account_origin()?
        || b.gateway_origin != GATEWAY_ORIGIN
        || b.key_id <= 0
        || b.group_id.is_some_and(|id| id <= 0)
        || b.account_id.trim().is_empty()
        || provider.category.as_deref() == Some("official")
    {
        return Err(invalid());
    }
    let meta = provider.meta.as_ref().ok_or_else(invalid)?;
    if !meta.custom_endpoints.is_empty()
        || meta.endpoint_auto_select == Some(true)
        || meta.is_full_url == Some(true)
        || meta.provider_type.is_some()
        || meta.auth_binding.is_some()
        || meta.github_account_id.is_some()
        || meta
            .local_proxy_request_overrides
            .as_ref()
            .is_some_and(|v| !v.is_empty())
    {
        return Err(invalid());
    }
    if matches!(app, AppType::ClaudeDesktop) {
        let routes = &meta.claude_desktop_model_routes;
        let selected = provider.settings_config["env"]["ANTHROPIC_MODEL"]
            .as_str()
            .filter(|model| !model.is_empty())
            .ok_or_else(invalid)?;
        // Account credentials may only be materialized for native forwarding,
        // never projected into a direct Desktop profile or alternate protocol.
        if meta.claude_desktop_mode != Some(crate::provider::ClaudeDesktopMode::Proxy)
            || meta
                .api_format
                .as_deref()
                .is_some_and(|format| format != "anthropic")
            || routes.is_empty()
            || routes.len() > 200
            || !routes.values().any(|route| route.model == selected)
            || routes.iter().any(|(id, route)| {
                !crate::claude_desktop_config::is_claude_safe_model_id(id)
                    || route.model.is_empty()
                    || route.model.len() > 256
                    || route.model.trim() != route.model
                    || route.model.chars().any(char::is_control)
                    || !super::connection::desktop_conversation_model(&route.model)
            })
        {
            return Err(invalid());
        }
    }
    validate_routes(&provider.settings_config, gateway(app)?)?;
    if matches!(app, AppType::Codex) {
        codex_config(&provider.settings_config)?;
    }
    if matches!(app, AppType::OpenCode) {
        // Arbitrary npm implementations can redirect a materialized secret.
        if let Some(npm) = provider.settings_config.get("npm") {
            if !matches!(
                npm.as_str(),
                Some("@ai-sdk/openai-compatible" | "@ai-sdk/openai")
            ) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
/// Caller must also compare binding equality with the existing DB row.
/// This helper never grants authority to create or replace a managed binding.
pub fn validate_managed_provider_shape(app: &AppType, provider: &Provider) -> Result<(), AppError> {
    validate_provider(app, provider)?;
    if binding(provider).is_none() {
        return Ok(());
    }
    let safe = sanitize_settings(app, provider, &provider.settings_config)?;
    let mut original = provider.settings_config.clone();
    if matches!(app, AppType::Codex) {
        original["config"] = safe["config"].clone();
        let mut parsed = serde_json::to_value(codex_config(&provider.settings_config)?)
            .map_err(|_| invalid())?;
        let before = parsed.clone();
        scrub(&mut parsed);
        if parsed != before {
            return Err(invalid());
        }
    }
    if original != safe {
        return Err(invalid());
    }
    Ok(())
}
fn normalized(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn validate_routes(value: &Value, expected: &str) -> Result<(), AppError> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                // MCP remains user-owned; no managed credential is injected there.
                if matches!(key.as_str(), "mcp" | "mcpServers" | "mcp_servers") {
                    continue;
                }
                let k = normalized(key);
                if matches!(key.as_str(), "env" | "auth" | "options" | "settings")
                    && !value.is_object()
                {
                    return Err(invalid());
                }
                if k.contains("baseurl") || k == "apiendpoint" || k == "modelsurl" {
                    if value.as_str() != Some(expected) {
                        return Err(invalid());
                    }
                }
                if (k.ends_with("headers")
                    || matches!(
                        k.as_str(),
                        "headers"
                            | "httpheaders"
                            | "envhttpheaders"
                            | "customheaders"
                            | "apikeyhelper"
                            | "envkey"
                            | "credential"
                            | "credentials"
                            | "authcommand"
                            | "proxy"
                            | "proxyurl"
                            | "fetch"
                    ))
                    && !value.is_null()
                    && value != &json!({})
                {
                    return Err(invalid());
                }
                validate_routes(value, expected)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                validate_routes(item, expected)?;
            }
        }
        _ => (),
    }
    Ok(())
}
fn codex_config(settings: &Value) -> Result<toml::Value, AppError> {
    let doc: toml::Value = settings
        .get("config")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let id = doc
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .ok_or_else(invalid)?;
    if doc
        .get("model_providers")
        .and_then(|v| v.get(id))
        .and_then(toml::Value::as_table)
        .is_none()
    {
        return Err(invalid());
    }
    validate_routes(
        &serde_json::to_value(&doc).map_err(|_| invalid())?,
        gateway(&AppType::Codex)?,
    )?;
    Ok(doc)
}
/// Transient clone only. A saved tool grant remains valid after account logout.
pub fn resolve_provider(app_type: &AppType, provider: &Provider) -> Result<Provider, AppError> {
    if binding(provider).is_none() {
        return Ok(provider.clone());
    }
    #[cfg(test)]
    if let Some(result) = test_support::resolve(app_type, provider) {
        return result;
    }
    let vault = vault().map_err(|_| {
        log::warn!("OSIR managed credential resolution failed: vault_unavailable");
        invalid()
    })?;
    resolve_with_vault(app_type, provider, &vault)
}

/// Scoped to the calling test thread; never changes native identity or keychain.
#[cfg(test)]
pub(crate) mod test_support {
    use super::super::{
        credentials::CredentialError,
        key_vault::{KeyBinding, NativeKeySecret},
    };
    use super::*;
    use crate::provider::ProviderMeta;
    use std::cell::RefCell;
    use std::{
        collections::HashMap,
        marker::PhantomData,
        rc::Rc,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Default)]
    struct Memory(Arc<Mutex<HashMap<(String, String), Vec<u8>>>>);
    impl CredentialBackend for Memory {
        fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            self.0
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), bytes.to_vec());
            Ok(())
        }
        fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned())
        }
        fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
            self.0
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }
    /// Current-thread only. Nested fixtures must be dropped in LIFO order.
    pub(crate) struct Fixture {
        pub(crate) provider: Provider,
        memory: Memory,
        previous: Option<Resolver>,
        _thread: PhantomData<Rc<()>>,
    }
    impl Fixture {
        pub(crate) fn new(app: &AppType, settings: Value, secret: &str) -> Self {
            Self::with_metadata(app, settings, ProviderMeta::default(), secret)
        }
        pub(crate) fn desktop(models: &[&str], secret: &str) -> Self {
            let row = super::super::connection::provider_row(
                &super::super::connection::ConnectionRequest {
                    tool: "claude-desktop".into(),
                    key_id: 1,
                    name: "Desktop fixture".into(),
                    hide_aggregation_name: None,
                    model: models[0].into(),
                    models: models.iter().map(|model| (*model).into()).collect(),
                    generation: 1,
                },
                "temporary-fixture".into(),
            )
            .unwrap();
            Self::with_metadata(
                &AppType::ClaudeDesktop,
                row.settings_config,
                row.meta.unwrap(),
                secret,
            )
        }
        fn with_metadata(app: &AppType, settings: Value, meta: ProviderMeta, secret: &str) -> Self {
            let origin = trusted_account_origin().unwrap();
            let memory = Memory::default();
            let vault =
                KeyVault::with_backend(&origin, "com.osir.fixture", memory.clone()).unwrap();
            let binding = KeyBinding {
                account_origin: origin,
                account_id: "fixture-account".into(),
                key_id: 1,
                group_id: Some(2),
                tool: tool(app).unwrap(),
                provider_id: uuid::Uuid::new_v4().to_string(),
                gateway_origin: GATEWAY_ORIGIN.into(),
            };
            let stored = vault
                .store(&binding, &NativeKeySecret::new(secret.into()).unwrap())
                .unwrap();
            let mut provider = Provider::with_id(
                binding.provider_id.clone(),
                "Managed fixture".into(),
                settings,
                None,
            );
            provider.meta = Some(ProviderMeta {
                osir_binding: Some(ManagedBinding {
                    source: AuthBindingSource::ManagedAccount,
                    binding,
                    reference: stored.reference,
                    masked_key: stored.masked_key,
                }),
                ..meta
            });
            validate_managed_provider_shape(app, &provider).unwrap();
            let previous = FIXTURE.with(|slot| {
                slot.replace(Some(Box::new(move |app, provider| {
                    resolve_with_vault(app, provider, &vault)
                })))
            });
            Self {
                provider,
                memory,
                previous,
                _thread: PhantomData,
            }
        }
        pub(crate) fn clear_credentials(&self) {
            self.memory.0.lock().unwrap().clear();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            FIXTURE.with(|slot| *slot.borrow_mut() = self.previous.take());
        }
    }
    type Resolver = Box<dyn Fn(&AppType, &Provider) -> Result<Provider, AppError>>;
    thread_local! {
        static FIXTURE: RefCell<Option<Resolver>> = RefCell::new(None);
    }
    pub(super) fn resolve(
        app: &AppType,
        provider: &Provider,
    ) -> Option<Result<Provider, AppError>> {
        FIXTURE.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|resolver| resolver(app, provider))
        })
    }
    pub(crate) fn with_vault<B: CredentialBackend + 'static, R>(
        vault: KeyVault<B>,
        run: impl FnOnce() -> R,
    ) -> R {
        struct Restore(Option<Resolver>);
        impl Drop for Restore {
            fn drop(&mut self) {
                FIXTURE.with(|slot| *slot.borrow_mut() = self.0.take());
            }
        }
        let previous = FIXTURE.with(|slot| {
            slot.replace(Some(Box::new(move |app, provider| {
                resolve_with_vault(app, provider, &vault)
            })))
        });
        let _restore = Restore(previous);
        run()
    }

    #[test]
    fn managed_fixture_is_thread_local_and_restores_after_unwind() {
        let app = AppType::Claude;
        assert!(FIXTURE.with(|slot| slot.borrow().is_none()));
        {
            let outer = Fixture::new(&app, json!({"env": {}}), "outer-fixture-secret");
            std::thread::spawn(|| {
                assert!(FIXTURE.with(|slot| slot.borrow().is_none()));
            })
            .join()
            .unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let inner = Fixture::new(&app, json!({"env": {}}), "inner-fixture-secret");
                assert_eq!(
                    resolve_provider(&app, &inner.provider)
                        .unwrap()
                        .settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
                    "inner-fixture-secret"
                );
                assert!(resolve_provider(&app, &outer.provider).is_err());
                panic!("fixture unwind");
            }));
            assert!(result.is_err());
            assert_eq!(
                resolve_provider(&app, &outer.provider)
                    .unwrap()
                    .settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
                "outer-fixture-secret"
            );
            outer.clear_credentials();
            assert!(resolve_provider(&app, &outer.provider).is_err());
        }
        assert!(FIXTURE.with(|slot| slot.borrow().is_none()));
    }
}
fn resolve_with_vault<B: CredentialBackend>(
    app: &AppType,
    provider: &Provider,
    vault: &KeyVault<B>,
) -> Result<Provider, AppError> {
    validate_provider(app, provider).map_err(|_| {
        log::warn!("OSIR managed credential resolution failed: provider_validation");
        invalid()
    })?;
    let managed = binding(provider).ok_or_else(invalid)?;
    let secret = vault
        .load(&managed.reference, &managed.binding)
        .map_err(|error| {
            log::warn!("OSIR managed credential resolution failed: {error:?}");
            invalid()
        })?
        .ok_or_else(|| {
            log::warn!("OSIR managed credential resolution failed: key_missing");
            invalid()
        })?;
    let mut result = provider.clone();
    result.settings_config = sanitize_settings(app, provider, &provider.settings_config)?;
    let s = &mut result.settings_config;
    match app {
        AppType::Codex => {
            let mut doc = codex_config(s)?;
            let id = doc["model_provider"]
                .as_str()
                .ok_or_else(invalid)?
                .to_owned();
            let route = doc
                .get_mut("model_providers")
                .and_then(|v| v.get_mut(&id))
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(invalid)?;
            route.insert("base_url".into(), toml::Value::String(gateway(app)?.into()));
            route.insert("requires_openai_auth".into(), toml::Value::Boolean(true));
            s["config"] = Value::String(toml::to_string(&doc).map_err(|_| invalid())?);
            s["auth"] = json!({"OPENAI_API_KEY": secret.expose()});
        }
        AppType::Claude | AppType::ClaudeDesktop => {
            s["env"]["ANTHROPIC_BASE_URL"] = json!(gateway(app)?);
            let field = result
                .meta
                .as_ref()
                .and_then(|m| m.api_key_field.as_deref())
                .unwrap_or("ANTHROPIC_AUTH_TOKEN");
            if !matches!(field, "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY") {
                return Err(invalid());
            }
            s["env"][field] = json!(secret.expose());
        }
        AppType::Gemini => {
            s["env"]["GOOGLE_GEMINI_BASE_URL"] = json!(gateway(app)?);
            s["env"]["GEMINI_API_KEY"] = json!(secret.expose());
        }
        AppType::OpenCode => {
            let field = if s.get("settings").is_some() {
                "settings"
            } else {
                "options"
            };
            s[field]["baseURL"] = json!(gateway(app)?);
            s[field]["apiKey"] = json!(secret.expose());
        }
        _ => return Err(invalid()),
    }
    Ok(result)
}
/// Strip managed authentication before live/editor data leaves native code or is
/// backfilled into storage. Non-managed providers retain legacy behavior.
/// Malformed Codex TOML fails closed; never return the original string on error.
pub fn sanitize_settings(
    app: &AppType,
    provider: &Provider,
    settings: &Value,
) -> Result<Value, AppError> {
    if binding(provider).is_none() {
        return Ok(settings.clone());
    }
    let mut safe = settings.clone();
    if !safe.is_object() {
        return Err(invalid());
    }
    if matches!(app, AppType::Codex) {
        if let Some(config) = safe.get("config") {
            let doc: toml::Value = config
                .as_str()
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())?;
            let mut value = serde_json::to_value(doc).map_err(|_| invalid())?;
            scrub(&mut value);
            let doc: toml::Value = serde_json::from_value(value).map_err(|_| invalid())?;
            safe["config"] = json!(toml::to_string(&doc).map_err(|_| invalid())?);
        }
    }
    scrub(&mut safe);
    if matches!(app, AppType::Codex) {
        safe["auth"] = json!({});
    }
    Ok(safe)
}
fn scrub(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| {
                let k = normalized(key);
                !(k.contains("apikey")
                    || k.ends_with("headers")
                    || k.ends_with("authtoken")
                    || k.ends_with("bearertoken")
                    || matches!(
                        k.as_str(),
                        "token" | "accesstoken" | "refreshtoken" | "idtoken"
                    )
                    || k.contains("secret")
                    || k.contains("password")
                    || k.contains("credential")
                    || matches!(
                        k.as_str(),
                        "auth"
                            | "authorization"
                            | "headers"
                            | "httpheaders"
                            | "envhttpheaders"
                            | "customheaders"
                            | "envkey"
                            | "authcommand"
                    ))
            });
            for (key, value) in map.iter_mut() {
                if !matches!(key.as_str(), "mcp" | "mcpServers" | "mcp_servers") {
                    scrub(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub(item);
            }
        }
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        credentials::CredentialError,
        key_vault::{KeyBinding, NativeKeySecret},
    };
    use super::*;
    use crate::provider::ProviderMeta;
    use std::{collections::HashMap, sync::Mutex};
    #[derive(Default)]
    struct Memory(Mutex<HashMap<(String, String), Vec<u8>>>);
    impl CredentialBackend for Memory {
        fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            self.0
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), bytes.to_vec());
            Ok(())
        }
        fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned())
        }
        fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
            self.0
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }
    fn fixture(app: &AppType, settings: Value) -> (KeyVault<Memory>, Provider) {
        let origin = trusted_account_origin().unwrap();
        let vault = KeyVault::with_backend(&origin, "com.osir.fixture", Memory::default()).unwrap();
        let b = KeyBinding {
            account_origin: origin,
            account_id: "fixture-account".into(),
            key_id: 1,
            group_id: Some(2),
            tool: tool(app).unwrap(),
            provider_id: "fixture-provider".into(),
            gateway_origin: GATEWAY_ORIGIN.into(),
        };
        let stored = vault
            .store(
                &b,
                &NativeKeySecret::new("fixture-managed-secret".into()).unwrap(),
            )
            .unwrap();
        let mut p = Provider::with_id(b.provider_id.clone(), "Fixture".into(), settings, None);
        p.meta = Some(ProviderMeta {
            osir_binding: Some(ManagedBinding {
                source: AuthBindingSource::ManagedAccount,
                binding: b,
                reference: stored.reference,
                masked_key: stored.masked_key,
            }),
            ..Default::default()
        });
        (vault, p)
    }
    #[test]
    fn managed_materialization_is_transient_for_all_tools_and_native_opencode() {
        for (app, settings, pointer) in [
            (
                AppType::Claude,
                json!({"env":{"ANTHROPIC_BASE_URL":GATEWAY_ORIGIN}}),
                "/env/ANTHROPIC_AUTH_TOKEN",
            ),
            (
                AppType::Gemini,
                json!({"env":{"GOOGLE_GEMINI_BASE_URL":GATEWAY_ORIGIN}}),
                "/env/GEMINI_API_KEY",
            ),
            (
                AppType::OpenCode,
                json!({"npm":"@ai-sdk/openai-compatible","options":{"baseURL":"https://api.osirclaw.com/v1"},"models":{"m":{}}}),
                "/options/apiKey",
            ),
            (
                AppType::OpenCode,
                json!({"settings":{"baseURL":"https://api.osirclaw.com/v1"},"models":{"m":{"modelID":"m"}}}),
                "/settings/apiKey",
            ),
            (
                AppType::Codex,
                json!({"auth":{},"config":"model_provider = 'osir'
model = 'm'
[model_providers.osir]
base_url = 'https://api.osirclaw.com/v1'
wire_api = 'responses'
"}),
                "/auth/OPENAI_API_KEY",
            ),
        ] {
            let (vault, p) = fixture(&app, settings);
            validate_managed_provider_shape(&app, &p).unwrap();
            let materialized = resolve_with_vault(&app, &p, &vault).unwrap();
            assert_eq!(
                materialized
                    .settings_config
                    .pointer(pointer)
                    .and_then(Value::as_str),
                Some("fixture-managed-secret")
            );
            assert!(!serde_json::to_string(&p)
                .unwrap()
                .contains("fixture-managed-secret"));
            let safe = sanitize_settings(&app, &p, &materialized.settings_config).unwrap();
            assert!(!safe.to_string().contains("fixture-managed-secret"));
        }
    }
    #[test]
    fn binding_tampering_and_wrong_tool_fail_closed_without_echo() {
        let (vault, original) = fixture(&AppType::Claude, json!({"env":{}}));
        for field in [
            "provider", "account", "key", "group", "gateway", "source", "origin",
        ] {
            let mut p = original.clone();
            let m = p.meta.as_mut().unwrap().osir_binding.as_mut().unwrap();
            match field {
                "provider" => p.id = "other".into(),
                "account" => m.binding.account_id = "other".into(),
                "key" => m.binding.key_id += 1,
                "group" => m.binding.group_id = None,
                "gateway" => m.binding.gateway_origin = "https://evil.invalid".into(),
                "origin" => m.binding.account_origin = "https://evil.invalid".into(),
                _ => m.source = AuthBindingSource::ProviderConfig,
            }
            assert!(resolve_with_vault(&AppType::Claude, &p, &vault).is_err());
        }
        assert!(resolve_with_vault(&AppType::Gemini, &original, &vault).is_err());
    }
    #[test]
    fn claude_desktop_managed_auth_is_transient_separate_from_cli_and_proxy_only() {
        let grant = test_support::Fixture::desktop(
            &["claude-sonnet-5-5", "other-model"],
            "desktop-fixture-secret",
        );
        let saved = serde_json::to_value(&grant.provider).unwrap();
        validate_managed_provider_shape(&AppType::ClaudeDesktop, &grant.provider).unwrap();
        let resolved = resolve_provider(&AppType::ClaudeDesktop, &grant.provider).unwrap();
        assert_eq!(
            resolved.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
            "desktop-fixture-secret"
        );
        let auth = crate::proxy::providers::get_adapter(&AppType::ClaudeDesktop)
            .unwrap()
            .extract_auth(&resolved)
            .unwrap();
        assert_eq!(auth.api_key, "desktop-fixture-secret");
        assert_eq!(serde_json::to_value(&grant.provider).unwrap(), saved);
        assert!(!saved.to_string().contains("desktop-fixture-secret"));
        assert!(resolve_provider(&AppType::Claude, &grant.provider).is_err());
        assert!(crate::claude_desktop_config::direct_target(Some(&grant.provider)).is_err());
        let routes = crate::claude_desktop_config::proxy_model_routes(&grant.provider).unwrap();
        assert_eq!(routes[0].upstream_model, "claude-sonnet-5-5");
        for mutation in 0..4 {
            let mut row = grant.provider.clone();
            let meta = row.meta.as_mut().unwrap();
            match mutation {
                0 => meta.claude_desktop_mode = Some(crate::provider::ClaudeDesktopMode::Direct),
                1 => meta.api_format = Some("openai_chat".into()),
                2 => meta.claude_desktop_model_routes.clear(),
                _ => meta.osir_binding.as_mut().unwrap().binding.tool = KeyTool::ClaudeCode,
            }
            assert!(validate_managed_provider_shape(&AppType::ClaudeDesktop, &row).is_err());
        }
        grant.clear_credentials();
        assert!(resolve_provider(&AppType::ClaudeDesktop, &grant.provider).is_err());
    }
    #[test]
    fn edited_routes_and_credential_overrides_are_rejected() {
        for settings in [
            json!({"env":{"ANTHROPIC_BASE_URL":"https://evil.invalid"}}),
            json!({"headers":{"Authorization":"fixture-managed-secret"}}),
            json!({"apiKeyHelper":"command"}),
        ] {
            let (vault, p) = fixture(&AppType::Claude, settings);
            assert!(resolve_with_vault(&AppType::Claude, &p, &vault).is_err());
        }
        let (_, mut p) = fixture(
            &AppType::Claude,
            json!({"env":{"ANTHROPIC_AUTH_TOKEN":"fixture-managed-secret"}}),
        );
        assert!(validate_managed_provider_shape(&AppType::Claude, &p).is_err());
        p.settings_config = json!({"env":{"ANTHROPIC_MODEL":"new-model"},"modelCatalog":{"defaultModel":"new-model"}});
        validate_managed_provider_shape(&AppType::Claude, &p).unwrap();
    }
    #[test]
    fn sanitizer_preserves_mcp_models_and_token_limits_and_rejects_bad_toml() {
        let (_, p) = fixture(&AppType::Claude, json!({}));
        let value = json!({"max_tokens":4096,"mcpServers":{"local":{"command":"fixture-command","env":{"LOCAL_TOKEN":"user-owned"}}},"env":{"ANTHROPIC_AUTH_TOKEN":"fixture-managed-secret","ANTHROPIC_MODEL":"m"}});
        let safe = sanitize_settings(&AppType::Claude, &p, &value).unwrap();
        assert_eq!(safe["max_tokens"], 4096);
        assert_eq!(safe["mcpServers"], value["mcpServers"]);
        assert_eq!(safe["env"]["ANTHROPIC_MODEL"], "m");
        assert!(!safe.to_string().contains("fixture-managed-secret"));
        assert!(sanitize_settings(&AppType::Codex, &p, &json!({"config":"[bad"})).is_err());
    }
    #[test]
    fn ordinary_provider_and_sanitization_do_not_require_initialization() {
        let p = Provider::with_id(
            "ordinary".into(),
            "ordinary".into(),
            json!({"apiKey":"legacy"}),
            None,
        );
        assert_eq!(
            resolve_provider(&AppType::OpenCode, &p)
                .unwrap()
                .settings_config,
            p.settings_config
        );
        assert_eq!(
            sanitize_settings(&AppType::OpenCode, &p, &p.settings_config).unwrap(),
            p.settings_config
        );
    }
}
