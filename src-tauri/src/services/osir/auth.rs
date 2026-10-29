//! Trusted native deployment configuration. Never construct from IPC/provider URLs.
use super::{client::AccountClient, models::DesktopAuthCapability};

pub use super::session::{AuthManager, LoginAttempt, LoginGrant, SessionError, SessionRead};

#[derive(Clone)]
pub struct AuthConfig {
    pub enabled: bool,
    pub account_api_base: String,
    pub website_origin: String,
    pub scope: String,
}
impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            // Native support; check server readiness before opening the browser.
            enabled: cfg!(any(target_os = "macos", target_os = "windows")),
            // Source and unauthenticated HTTP verified 2026-10-27: account origin;
            // both domains respond to account and model routes without credentials.
            account_api_base: option_env!("OSIR_ACCOUNT_API_BASE")
                .unwrap_or("https://osirclaw.com/api/v1/")
                .into(),
            website_origin: option_env!("OSIR_WEBSITE_ORIGIN")
                .unwrap_or("https://osirclaw.com")
                .into(),
            scope: "account:read keys:read".into(),
        }
    }
}
impl AuthConfig {
    /// Write permission is chosen per login, never inherited from deployment config.
    pub(super) fn login_scope(&self, create_keys: bool) -> &'static str {
        if create_keys {
            "account:read keys:read keys:write"
        } else {
            "account:read keys:read"
        }
    }
    /// Optional scopes are requested only after the server advertises support.
    /// Existing deployments keep their original, compatible login grant.
    pub(super) fn login_scope_with_invoices(&self, create_keys: bool, invoices: bool) -> String {
        let base = self.login_scope(create_keys);
        if invoices {
            format!("{base} invoices:read invoices:write")
        } else {
            base.to_owned()
        }
    }

    pub(super) fn validate(&self) -> Result<AccountClient, super::session::SessionError> {
        use super::session::SessionError;
        let website =
            url::Url::parse(&self.website_origin).map_err(|_| SessionError::Configuration)?;
        if website.scheme() != "https"
            || website.host_str().is_none()
            || !website.username().is_empty()
            || website.password().is_some()
            || website.query().is_some()
            || website.fragment().is_some()
            || website.path() != "/"
            || self.website_origin.trim() != self.website_origin
            || self.website_origin.chars().any(char::is_control)
        {
            return Err(SessionError::Configuration);
        }
        validate_scope(&self.scope)?;
        AccountClient::new(&self.account_api_base).map_err(|_| SessionError::Configuration)
    }
}
pub(super) fn validate_scope(scope: &str) -> Result<(), super::session::SessionError> {
    let scopes: Vec<_> = scope.split(' ').collect();
    if !scopes.contains(&"account:read")
        || scopes.len() > 5
        || scopes.iter().any(|s| {
            !matches!(
                *s,
                "account:read" | "keys:read" | "keys:write" | "invoices:read" | "invoices:write"
            )
        })
        || scopes
            .iter()
            .enumerate()
            .any(|(i, s)| scopes[..i].contains(s))
    {
        return Err(super::session::SessionError::InvalidResponse);
    }
    Ok(())
}
/// Legacy capability remains disabled until commands owns an enabled manager.
pub fn capability() -> DesktopAuthCapability {
    DesktopAuthCapability::default()
}
