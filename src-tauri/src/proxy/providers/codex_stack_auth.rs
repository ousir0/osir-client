//! Account-pinned authentication for official Codex Stack targets.
//!
//! Integration boundary: invoke only for a validated StackTarget, before the
//! ordinary native-login passthrough check. Never use for an unqualified request.
//! This module deliberately does not import/adopt the current native login or
//! select the manager's default account. Binding is an explicit user operation.

use super::codex_oauth_auth::CodexOAuthManager;
use crate::{error::AppError, provider::Provider};

/// Native-only secret container. No Debug/Clone/Serialize implementation.
pub(crate) struct PinnedOfficialAuth {
    authorization: http::HeaderValue,
    workspace: http::HeaderValue,
}

fn unavailable() -> AppError {
    // Do not propagate manager errors which may include upstream responses.
    AppError::Config(
        "The selected Codex account is unavailable; bind or authorize that account again".into(),
    )
}

pub(crate) fn bound_account(provider: &Provider) -> Result<String, AppError> {
    if !super::is_codex_official_provider(provider) {
        return Err(unavailable());
    }
    provider
        .meta
        .as_ref()
        .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
        .filter(|id| !id.trim().is_empty() && id == id.trim())
        .ok_or_else(unavailable)
}

/// Resolve exactly the stored local account id. A missing/revoked account is an
/// error even if another valid/default account exists. Bundle acquisition uses
/// the existing manager's lifecycle/generation locks, refresh and revocation rules.
pub(crate) async fn resolve(
    manager: &CodexOAuthManager,
    provider: &Provider,
) -> Result<PinnedOfficialAuth, AppError> {
    let id = bound_account(provider)?;
    let bundle = manager
        .get_valid_token_bundle_for_account(&id)
        .await
        .map_err(|_| unavailable())?;
    PinnedOfficialAuth::from_parts(&bundle.access_token, &bundle.chatgpt_account_id)
}

/// Pinned credentials may only reach the fixed official backend.
pub(crate) fn validate_destination(destination: &str) -> Result<(), AppError> {
    let url = url::Url::parse(destination).map_err(|_| unavailable())?;
    if url.scheme() != "https"
        || url.host_str() != Some("chatgpt.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || !(url.path() == "/backend-api/codex" || url.path().starts_with("/backend-api/codex/"))
        || url.fragment().is_some()
    {
        return Err(unavailable());
    }
    Ok(())
}

/// Keep redirects disabled: even a same-origin redirect outside the allowed
/// backend path must not carry a pinned account's credentials.
pub(crate) fn client(proxy: Option<&str>) -> Result<reqwest::Client, AppError> {
    type Cached = Option<(Option<String>, reqwest::Client)>;
    static CLIENT: std::sync::OnceLock<std::sync::Mutex<Cached>> = std::sync::OnceLock::new();
    let mut cached = CLIENT
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| unavailable())?;
    if let Some((previous, client)) = cached.as_ref() {
        if previous.as_deref() == proxy {
            return Ok(client.clone());
        }
    }
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    if let Some(proxy) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| unavailable())?);
    }
    let client = builder.build().map_err(|_| unavailable())?;
    *cached = Some((proxy.map(str::to_string), client.clone()));
    Ok(client)
}

impl PinnedOfficialAuth {
    fn from_parts(token: &str, workspace: &str) -> Result<Self, AppError> {
        if token.trim().is_empty()
            || workspace.trim().is_empty()
            || token != token.trim()
            || workspace != workspace.trim()
        {
            return Err(unavailable());
        }
        let mut authorization =
            http::HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| unavailable())?;
        authorization.set_sensitive(true);
        let mut workspace = http::HeaderValue::from_str(workspace).map_err(|_| unavailable())?;
        workspace.set_sensitive(true);
        Ok(Self {
            authorization,
            workspace,
        })
    }

    /// Apply to a per-request header copy immediately before the fixed official
    /// upstream send. Never mutate the incoming shared headers or live auth.json.
    /// Run after generic header copying so caller credentials cannot override it.
    pub(crate) fn apply(self, headers: &mut http::HeaderMap) {
        headers.remove(http::header::AUTHORIZATION);
        headers.remove("chatgpt-account-id");
        headers.remove("openai-organization");
        headers.remove("openai-project");
        headers.remove("x-api-key");
        headers.insert(http::header::AUTHORIZATION, self.authorization);
        headers.insert("chatgpt-account-id", self.workspace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct IsolatedHome {
        env: Option<std::ffi::OsString>,
        settings: crate::settings::AppSettings,
        _dir: tempfile::TempDir,
    }
    impl IsolatedHome {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let settings = crate::settings::get_settings();
            let env = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            crate::settings::update_settings(crate::settings::AppSettings {
                codex_config_dir: Some(dir.path().join("codex").to_string_lossy().into_owned()),
                ..Default::default()
            })
            .unwrap();
            assert!(crate::codex_config::get_codex_auth_path().starts_with(dir.path()));
            Self {
                env,
                settings,
                _dir: dir,
            }
        }
    }
    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            let _ = crate::settings::update_settings(self.settings.clone());
            match &self.env {
                Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[test]
    fn pinned_destination_is_fixed_official_origin_and_path() {
        assert!(validate_destination("https://chatgpt.com/backend-api/codex/responses").is_ok());
        for url in [
            "http://chatgpt.com/backend-api/codex/responses",
            "https://chatgpt.com.evil.test/backend-api/codex/responses",
            "https://chatgpt.com:444/backend-api/codex/responses",
            "https://user@chatgpt.com/backend-api/codex/responses",
            "https://chatgpt.com/backend-api/codex-evil/responses",
            "https://chatgpt.com/backend-api/codex/../../other",
        ] {
            assert!(validate_destination(url).is_err());
        }
    }

    fn bound_provider(account: &str) -> Provider {
        let mut provider = Provider::with_id(
            "official-fixture".into(),
            "Official".into(),
            json!({"auth":{}, "config":""}),
            None,
        );
        provider.category = Some("official".into());
        provider.meta = Some(
            serde_json::from_value(json!({"authBinding": {
                "source": "managed_account", "authProvider": "codex_oauth", "accountId": account
            }}))
            .unwrap(),
        );
        provider
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn pinned_account_ignores_default_and_deleted_account_never_falls_back() {
        let _home = IsolatedHome::new();
        let id_a = crate::codex_config::test_codex_id_token("subject-a");
        let id_b = crate::codex_config::test_codex_id_token("subject-b");
        let dir = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(dir.path().to_path_buf());
        manager
            .add_test_account_with_workspace_and_access_token(
                "a",
                "workspace-a",
                "fixture-token-a",
                Some(&id_a),
            )
            .await
            .unwrap();
        manager
            .add_test_account_with_workspace_and_access_token(
                "b",
                "workspace-b",
                "fixture-token-b",
                Some(&id_b),
            )
            .await
            .unwrap();
        assert_eq!(manager.default_account_id().await.as_deref(), Some("a"));
        let provider = bound_provider("b");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Bearer current-native-a".parse().unwrap(),
        );
        resolve(&manager, &provider)
            .await
            .unwrap()
            .apply(&mut headers);
        assert_eq!(
            headers[http::header::AUTHORIZATION],
            "Bearer fixture-token-b"
        );
        assert_eq!(headers["chatgpt-account-id"], "workspace-b");
        manager.remove_account("b").await.unwrap();
        assert!(resolve(&manager, &provider).await.is_err());
        assert!(resolve(&manager, &bound_provider("missing")).await.is_err());
    }

    #[test]
    fn pinned_auth_replaces_current_login_and_workspace() {
        let mut headers = http::HeaderMap::new();
        headers.append(
            http::header::AUTHORIZATION,
            "Bearer wrong-account".parse().unwrap(),
        );
        headers.append(
            http::header::AUTHORIZATION,
            "Bearer duplicate".parse().unwrap(),
        );
        headers.insert("chatgpt-account-id", "wrong-workspace".parse().unwrap());
        headers.insert("openai-project", "wrong-project".parse().unwrap());
        headers.insert("x-api-key", "wrong-key".parse().unwrap());
        headers.insert("openai-organization", "wrong-organization".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        PinnedOfficialAuth::from_parts("fixture-target-token", "fixture-workspace")
            .unwrap()
            .apply(&mut headers);
        assert_eq!(
            headers.get_all(http::header::AUTHORIZATION).iter().count(),
            1
        );
        assert_eq!(
            headers[http::header::AUTHORIZATION],
            "Bearer fixture-target-token"
        );
        assert!(headers[http::header::AUTHORIZATION].is_sensitive());
        assert_eq!(headers["chatgpt-account-id"], "fixture-workspace");
        assert!(!headers.contains_key("x-api-key"));
        assert!(!headers.contains_key("openai-project"));
        assert!(!headers.contains_key("openai-organization"));
        assert!(headers["chatgpt-account-id"].is_sensitive());
        assert_eq!(headers["content-type"], "application/json");
    }

    #[test]
    fn pinned_auth_rejects_invalid_credentials_without_echoing_them() {
        for (token, workspace) in [
            ("", "workspace"),
            ("token", ""),
            ("secret\r\nInjected: value", "workspace"),
            ("token", "workspace\nInjected"),
        ] {
            let error = match PinnedOfficialAuth::from_parts(token, workspace) {
                Ok(_) => panic!("invalid fixture accepted"),
                Err(error) => error,
            };
            assert!(!error.to_string().contains("Injected"));
        }
    }

    #[tokio::test]
    async fn unbound_official_never_uses_default_account() {
        let dir = tempfile::tempdir().unwrap();
        let manager = CodexOAuthManager::new(dir.path().to_path_buf());
        let mut provider = Provider::with_id(
            "native-fixture".into(),
            "Official".into(),
            json!({"auth":{}, "config":""}),
            None,
        );
        provider.category = Some("official".into());
        assert!(resolve(&manager, &provider).await.is_err());
    }
}
