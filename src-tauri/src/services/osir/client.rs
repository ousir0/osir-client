//! Typed OSIR account API boundary.
//!
//! Account API calls must be made by native code with a scoped desktop session.
//! Keep account API URL configuration separate from model gateway URLs.

use super::{
    models::DesktopAuthCapability,
    request_budget::{AdmissionError, AttemptGuard, RequestBudget, RequestClass},
};
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;
use url::Url;

/// Account endpoints are selected by native code, never arbitrary URLs from IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountResource {
    Profile,
    Keys,
    Groups,
    GroupRates,
    Subscriptions,
    Usage,
    UsageStats,
    UsageSnapshot,
    Announcements,
}

impl AccountResource {
    fn path(self) -> &'static str {
        match self {
            Self::Profile => "user/profile",
            Self::Keys => "keys",
            Self::Groups => "groups/available",
            Self::GroupRates => "groups/rates",
            Self::Subscriptions => "subscriptions",
            Self::Usage => "usage",
            Self::UsageStats => "usage/stats",
            Self::UsageSnapshot => "usage/dashboard/snapshot-v2",
            Self::Announcements => "announcements",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    #[error("账户服务地址配置无效")]
    Configuration,
    #[error("账户会话已失效，请重新授权")]
    Unauthorized,
    #[error("当前账户无权访问此资源")]
    Forbidden,
    #[error("请求过于频繁，请稍后重试")]
    RateLimited { retry_after_seconds: Option<u64> },
    #[error("账户服务暂时不可用")]
    Unavailable,
    #[error("授权请求结果未确认，请勿重放")]
    AuthOutcomeUnknown,
    #[error("账户数据格式不兼容")]
    InvalidResponse,
}

/// Narrow desktop create payload. Extra/custom-key/access-control fields are rejected.
/// Quota 0 means unlimited; absent expiry means no expiry. Native caller must
/// obtain explicit user intent before invoking this mutation.
#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateKeyRequest {
    pub name: String,
    pub group_id: Option<i64>,
    pub quota: serde_json::Number,
    pub expires_in_days: Option<u32>,
}
impl CreateKeyRequest {
    pub fn validate(&self) -> Result<(), AccountError> {
        if self.name.trim().is_empty()
            || self.name.trim() != self.name
            || self.name.len() > 100
            || self.name.chars().any(char::is_control)
            || self.group_id.is_some_and(|id| id <= 0)
            || !self
                .quota
                .as_f64()
                .is_some_and(|q| q.is_finite() && q >= 0.0 && q <= 1_000_000_000.0)
            || self
                .expires_in_days
                .is_some_and(|days| days == 0 || days > 3650)
        {
            return Err(AccountError::Configuration);
        }
        Ok(())
    }
}
/// Safe error category only. Uncertain means reconcile the key list before any
/// new user-confirmed attempt; no idempotency guarantee exists on this route.
#[derive(Debug, thiserror::Error)]
pub enum CreateKeyError {
    #[error("创建结果未确认，请先刷新密钥列表，勿重复创建")]
    Uncertain,
    #[error("创建请求被拒绝")]
    Rejected,
    #[error("创建参数无效")]
    InvalidInput,
}

/// Deliberately not Debug/Serialize; the authenticated client stays native.
#[derive(Clone)]
pub struct AccountClient {
    http: reqwest::Client,
    base: Url,
    // Clones share actual-attempt limits across auth, reads, replays and writes.
    budget: RequestBudget,
}

/// Opt-in operational metadata. Identity is derived by the server from the
/// existing desktop session; no account identifiers or local data are uploaded.
#[derive(Serialize)]
pub(crate) struct ClientActivityMetadata<'a> {
    pub version: &'a str,
    pub channel: &'static str,
    pub platform: &'a str,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DesktopAuthorizationSupport {
    enabled: bool,
    protocol_version: u32,
    #[serde(default)]
    supported_scopes: Vec<String>,
}
impl DesktopAuthorizationSupport {
    pub fn ready(&self) -> bool {
        self.enabled && self.protocol_version == 1
    }
    pub fn invoices(&self) -> bool {
        self.ready()
            && ["invoices:read", "invoices:write"]
                .iter()
                .all(|scope| self.supported_scopes.iter().any(|item| item == scope))
    }
}

impl AccountClient {
    pub(super) async fn record_client_activity(
        &self,
        metadata: &ClientActivityMetadata<'_>,
        access_token: &str,
    ) -> Result<(), AccountError> {
        if access_token.is_empty() {
            return Err(AccountError::Unauthorized);
        }
        let url = self
            .base
            .join("osir-client/activity")
            .map_err(|_| AccountError::Configuration)?;
        #[derive(serde::Deserialize)]
        struct Accepted {
            accepted: bool,
        }
        let reply: Accepted = self
            .send_and_read(
                self.http
                    .post(url)
                    .bearer_auth(access_token)
                    .json(metadata)
                    .timeout(Duration::from_secs(5)),
                RequestClass::Standard,
            )
            .await?;
        if !reply.accepted {
            return Err(AccountError::InvalidResponse);
        }
        Ok(())
    }

    /// Configuration must come from trusted native deployment configuration.
    pub fn new(base: &str) -> Result<Self, AccountError> {
        if base.trim() != base || base.chars().any(char::is_control) {
            return Err(AccountError::Configuration);
        }
        let base = Url::parse(base).map_err(|_| AccountError::Configuration)?;
        if base.scheme() != "https"
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/api/v1/"
        {
            return Err(AccountError::Configuration);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // Each wire attempt must pass our budget; never replay POSTs below it.
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| AccountError::Configuration)?;
        Ok(Self {
            http,
            base,
            budget: RequestBudget::default(),
        })
    }

    /// Shares the existing account budget, including cooldown and in-flight state.
    pub fn request_budget(&self) -> RequestBudget {
        self.budget.clone()
    }

    /// Public readiness only; no website cookies or account credentials.
    pub(super) async fn desktop_authorization_ready(
        &self,
    ) -> Result<DesktopAuthorizationSupport, AccountError> {
        let url = self
            .base
            .join("auth/desktop/capabilities")
            .map_err(|_| AccountError::Configuration)?;
        let readiness: DesktopAuthorizationSupport = self
            .send_and_read(
                self.http.get(url).timeout(Duration::from_secs(8)),
                RequestClass::Standard,
            )
            .await?;
        Ok(readiness)
    }

    /// No automatic replay: refresh and retry policy belong to the coordinator.
    /// T may contain secrets; callers must project a safe DTO before IPC/cache.
    pub async fn get<T: DeserializeOwned>(
        &self,
        resource: AccountResource,
        access_token: &str,
        query: &[(String, String)],
    ) -> Result<T, AccountError> {
        if access_token.is_empty() {
            return Err(AccountError::Unauthorized);
        }
        let url = self
            .base
            .join(resource.path())
            .map_err(|_| AccountError::Configuration)?;
        self.send_and_read(
            self.http.get(url).bearer_auth(access_token).query(query),
            resource_class(resource),
        )
        .await
    }

    /// Exact user-owned key endpoint. The result stays native, never in shared cache.
    pub(super) async fn get_key<T: DeserializeOwned>(
        &self,
        id: i64,
        access_token: &str,
    ) -> Result<T, AccountError> {
        if id <= 0 || access_token.is_empty() {
            return Err(AccountError::Configuration);
        }
        let url = self
            .base
            .join(&format!("keys/{id}"))
            .map_err(|_| AccountError::Configuration)?;
        self.send_and_read(
            self.http.get(url).bearer_auth(access_token),
            RequestClass::Standard,
        )
        .await
    }

    /// Explicit mutation, no generic URL or automatic retry surface.
    pub(super) async fn mark_announcement_read(
        &self,
        id: i64,
        access_token: &str,
    ) -> Result<serde_json::Value, AccountError> {
        if id <= 0 || access_token.is_empty() {
            return Err(AccountError::Configuration);
        }
        let url = self
            .base
            .join(&format!("announcements/{id}/read"))
            .map_err(|_| AccountError::Configuration)?;
        self.send_and_read(
            self.http
                .post(url)
                .bearer_auth(access_token)
                .json(&serde_json::json!({})),
            RequestClass::Standard,
        )
        .await
    }

    /// Exactly one POST. Transport, server, or successful-body decoding failure
    /// may follow a committed create and therefore have an uncertain outcome.
    pub(super) async fn create_key<T: DeserializeOwned>(
        &self,
        request: &CreateKeyRequest,
        access_token: &str,
    ) -> Result<T, CreateKeyError> {
        request
            .validate()
            .map_err(|_| CreateKeyError::InvalidInput)?;
        if access_token.is_empty() {
            return Err(CreateKeyError::Rejected);
        }
        let url = self
            .base
            .join("keys")
            .map_err(|_| CreateKeyError::InvalidInput)?;
        // Admission denial is known not sent; transport/server failures remain
        // uncertain and must never cause automatic duplicate creation.
        let request = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(request)
            .build()
            .map_err(|_| CreateKeyError::InvalidInput)?;
        let guard = self
            .budget
            .admit(RequestClass::Standard)
            .await
            .map_err(|_| CreateKeyError::Rejected)?;
        let response = self
            .http
            .execute(request)
            .await
            .map_err(|_| CreateKeyError::Uncertain)?;
        observe_headers(&guard, &response);
        let status = response.status();
        if status.is_client_error() && status.as_u16() != 408 {
            return Err(CreateKeyError::Rejected);
        }
        if !status.is_success() {
            return Err(CreateKeyError::Uncertain);
        }
        let data = read_response(response)
            .await
            .map_err(|_| CreateKeyError::Uncertain)?;
        guard.finish_success();
        Ok(data)
    }

    /// Never replay a consumed code or rotating refresh after a timeout.
    pub(super) async fn auth_post<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        endpoint: AuthEndpoint,
        body: &B,
    ) -> Result<T, AccountError> {
        let url = self
            .base
            .join(endpoint.path())
            .map_err(|_| AccountError::Configuration)?;
        let request = self
            .http
            .post(url)
            .json(body)
            .build()
            .map_err(|_| AccountError::Configuration)?;
        let guard = self
            .budget
            .admit(RequestClass::Standard)
            .await
            .map_err(admission_error)?;
        let response = self
            .http
            .execute(request)
            .await
            .map_err(|_| AccountError::AuthOutcomeUnknown)?;
        observe_headers(&guard, &response);
        let successful_status = response.status().is_success();
        let data = read_response(response).await.map_err(|error| {
            if successful_status {
                AccountError::AuthOutcomeUnknown
            } else {
                error
            }
        })?;
        guard.finish_success();
        Ok(data)
    }
    /// Every invocation is one charged HTTP attempt. Session-level GET replay
    /// enters this same path again. POSTs are never retried by this boundary.
    async fn send_and_read<T: DeserializeOwned>(
        &self,
        builder: reqwest::RequestBuilder,
        class: RequestClass,
    ) -> Result<T, AccountError> {
        let request = builder.build().map_err(|_| AccountError::Configuration)?;
        let guard = self.budget.admit(class).await.map_err(admission_error)?;
        let response = self
            .http
            .execute(request)
            .await
            .map_err(|_| AccountError::Unavailable)?;
        observe_headers(&guard, &response);
        let data = read_response(response).await?;
        guard.finish_success();
        Ok(data)
    }
    pub(super) fn origin(&self) -> String {
        self.base.origin().ascii_serialization()
    }
}

fn resource_class(resource: AccountResource) -> RequestClass {
    match resource {
        AccountResource::UsageStats | AccountResource::UsageSnapshot => RequestClass::Heavy,
        _ => RequestClass::Standard,
    }
}
fn admission_error(error: AdmissionError) -> AccountError {
    match error {
        AdmissionError::Cooldown { retry_after } | AdmissionError::RateLimited { retry_after } => {
            AccountError::RateLimited {
                retry_after_seconds: Some(
                    retry_after
                        .as_secs()
                        .saturating_add(u64::from(retry_after.subsec_nanos() != 0)),
                ),
            }
        }
        AdmissionError::Busy | AdmissionError::HeavyBusy => AccountError::Unavailable,
    }
}
fn observe_headers(guard: &AttemptGuard, response: &reqwest::Response) {
    guard.observe_response(
        response.status().as_u16(),
        retry_after(response.headers(), chrono::Utc::now()),
    );
}
/// Retry-After supports delta seconds and HTTP dates. Header contents never
/// appear in errors/logging. Clamp before passing to the account-wide budget.
fn retry_after(
    headers: &reqwest::header::HeaderMap,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let text = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if text.is_empty() || text.len() > 128 {
        return None;
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return Some(Duration::from_secs(
            text.parse::<u64>().unwrap_or(u64::MAX).min(300),
        ));
    }
    let date = chrono::DateTime::parse_from_rfc2822(text)
        .map(|d| d.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            ["%A, %d-%b-%y %H:%M:%S GMT", "%a %b %e %H:%M:%S %Y"]
                .iter()
                .find_map(|format| {
                    chrono::NaiveDateTime::parse_from_str(text, format)
                        .ok()
                        .map(|date| date.and_utc())
                })
        })?;
    Some(
        date.signed_duration_since(now)
            .to_std()
            .unwrap_or(Duration::ZERO)
            .min(Duration::from_secs(300)),
    )
}

#[derive(Clone, Copy)]
pub(super) enum AuthEndpoint {
    Exchange,
    Refresh,
    Logout,
}
impl AuthEndpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Exchange => "auth/desktop/token",
            Self::Refresh => "auth/refresh",
            Self::Logout => "auth/logout",
        }
    }
}
async fn read_response<T: DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, AccountError> {
    match response.status().as_u16() {
        401 => return Err(AccountError::Unauthorized),
        403 => return Err(AccountError::Forbidden),
        429 => {
            return Err(AccountError::RateLimited {
                retry_after_seconds: retry_after(response.headers(), chrono::Utc::now())
                    .map(|d| d.as_secs().saturating_add(u64::from(d.subsec_nanos() != 0))),
            })
        }
        200..=299 => {}
        _ => return Err(AccountError::Unavailable),
    }
    const MAX_BODY: usize = 2 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(AccountError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AccountError::Unavailable)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err(AccountError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    decode(&bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AccountError> {
    #[derive(serde::Deserialize)]
    struct Envelope<T> {
        code: i64,
        data: T,
    }
    let envelope: Envelope<T> =
        serde_json::from_slice(bytes).map_err(|_| AccountError::InvalidResponse)?;
    if envelope.code != 0 {
        return Err(AccountError::Unavailable);
    }
    Ok(envelope.data)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in super::super) fn local_tls_client(base: &str, certificate: &[u8]) -> AccountClient {
        let mut client = AccountClient::new(base).unwrap();
        client.http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .add_root_certificate(reqwest::Certificate::from_pem(certificate).unwrap())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        client
    }

    // Local loopback fixture only. Trusted production URL validation is unchanged.
    pub(in super::super) fn response_fixture(
        response: &'static str,
    ) -> (AccountClient, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0u8; 8192];
            let _ = socket.read(&mut request).unwrap();
            socket.write_all(response.as_bytes()).unwrap();
        });
        let client = AccountClient {
            http: reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            base: Url::parse(&format!("http://{address}/api/v1/")).unwrap(),
            budget: RequestBudget::default(),
        };
        (client, worker)
    }
    #[tokio::test]
    async fn response_headers_apply_account_cooldown_even_for_create_error() {
        let (client, worker) = response_fixture("HTTP/1.1 503 Service Unavailable\r\nRetry-After: 30\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let request = CreateKeyRequest {
            name: "fixture".into(),
            group_id: None,
            quota: serde_json::Number::from(0),
            expires_in_days: None,
        };
        assert!(matches!(
            client
                .create_key::<serde_json::Value>(&request, "fixture")
                .await,
            Err(CreateKeyError::Uncertain)
        ));
        worker.join().unwrap();
        assert_eq!(client.budget.failed_attempts(), 1);
        assert!(matches!(
            client
                .clone()
                .auth_post::<serde_json::Value, _>(AuthEndpoint::Refresh, &serde_json::json!({}))
                .await,
            Err(AccountError::RateLimited { .. })
        ));
    }
    #[tokio::test]
    async fn guard_counts_body_decode_failure_and_releases_slot_after_validation() {
        let (client, worker) =
            response_fixture("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        assert!(matches!(
            client
                .get::<serde_json::Value>(AccountResource::Profile, "fixture", &[])
                .await,
            Err(AccountError::InvalidResponse)
        ));
        worker.join().unwrap();
        assert_eq!(client.budget.failed_attempts(), 1);
        let guards: Vec<_> = (0..4)
            .map(|_| client.budget.try_admit(RequestClass::Standard).unwrap())
            .collect();
        assert!(matches!(
            client.budget.try_admit(RequestClass::Standard),
            Err(AdmissionError::Busy)
        ));
        drop(guards);
    }
    #[tokio::test]
    async fn successful_decode_marks_attempt_successful() {
        let (client, worker) = response_fixture("HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{\"code\":0,\"data\":42}");
        assert_eq!(
            client
                .get::<u64>(AccountResource::Profile, "fixture", &[])
                .await
                .unwrap(),
            42
        );
        worker.join().unwrap();
        assert_eq!(client.budget.failed_attempts(), 0);
    }
    #[test]
    fn retry_after_seconds_dates_and_invalid_values_are_safe_and_bounded() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-27T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for (value, expected) in [
            ("12", Some(12)),
            ("99999999999999999999999999", Some(300)),
            ("Tue, 27 Oct 2026 00:00:30 GMT", Some(30)),
            ("Tue, 27 Oct 2026 00:10:00 GMT", Some(300)),
            ("Mon, 26 Oct 2026 00:00:00 GMT", Some(0)),
            ("-1", None),
            ("secret-response", None),
        ] {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::RETRY_AFTER, value.parse().unwrap());
            assert_eq!(retry_after(&headers, now).map(|d| d.as_secs()), expected);
        }
    }
    #[tokio::test]
    async fn all_endpoints_and_clones_obey_shared_cooldown_before_network() {
        let client = AccountClient::new("https://example.invalid/api/v1/").unwrap();
        client
            .budget
            .observe_response(429, Some(Duration::from_secs(60)));
        let clone = client.clone();
        assert!(matches!(
            clone
                .get::<serde_json::Value>(AccountResource::Profile, "fixture", &[])
                .await,
            Err(AccountError::RateLimited { .. })
        ));
        assert!(matches!(
            client.get_key::<serde_json::Value>(7, "fixture").await,
            Err(AccountError::RateLimited { .. })
        ));
        assert!(matches!(
            clone.mark_announcement_read(7, "fixture").await,
            Err(AccountError::RateLimited { .. })
        ));
        for endpoint in [
            AuthEndpoint::Exchange,
            AuthEndpoint::Refresh,
            AuthEndpoint::Logout,
        ] {
            assert!(matches!(
                client
                    .auth_post::<serde_json::Value, _>(endpoint, &serde_json::json!({}))
                    .await,
                Err(AccountError::RateLimited { .. })
            ));
        }
        let create = CreateKeyRequest {
            name: "fixture".into(),
            group_id: None,
            quota: serde_json::Number::from(0),
            expires_in_days: None,
        };
        assert!(matches!(
            clone
                .create_key::<serde_json::Value>(&create, "fixture")
                .await,
            Err(CreateKeyError::Rejected)
        ));
        // Local denial made no HTTP attempt, therefore no failed guard exists.
        assert_eq!(client.budget.failed_attempts(), 0);
    }
    #[test]
    fn only_aggregate_usage_resources_use_heavy_slots() {
        assert_eq!(
            resource_class(AccountResource::UsageStats),
            RequestClass::Heavy
        );
        assert_eq!(
            resource_class(AccountResource::UsageSnapshot),
            RequestClass::Heavy
        );
        assert_eq!(
            resource_class(AccountResource::Usage),
            RequestClass::Standard
        );
        assert!(matches!(
            admission_error(AdmissionError::Cooldown {
                retry_after: Duration::from_millis(1)
            }),
            AccountError::RateLimited {
                retry_after_seconds: Some(1)
            }
        ));
    }
    #[test]
    fn create_key_payload_is_closed_and_validated() {
        let valid =
            serde_json::json!({"name":"desktop", "group_id":7,"quota":0,"expires_in_days":30});
        let request: CreateKeyRequest = serde_json::from_value(valid.clone()).unwrap();
        assert!(request.validate().is_ok());
        for field in [
            "custom_key",
            "key",
            "ip_whitelist",
            "rate_limit_5h",
            "access_token",
        ] {
            let mut extra = valid.clone();
            extra[field] = serde_json::json!("forbidden");
            assert!(serde_json::from_value::<CreateKeyRequest>(extra).is_err());
        }
        for (field, value) in [
            ("name", serde_json::json!(" ")),
            ("group_id", serde_json::json!(0)),
            ("quota", serde_json::json!(-1)),
            ("expires_in_days", serde_json::json!(0)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<CreateKeyRequest>(invalid)
                .unwrap()
                .validate()
                .is_err());
        }
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(wire.as_object().unwrap().len(), 4);
    }
    #[test]
    fn resource_ipc_uses_closed_snake_case_enum() {
        assert_eq!(
            serde_json::to_string(&AccountResource::GroupRates).unwrap(),
            "\"group_rates\""
        );
        assert_eq!(
            serde_json::from_str::<AccountResource>("\"usage_snapshot\"").unwrap(),
            AccountResource::UsageSnapshot
        );
        assert!(serde_json::from_str::<AccountResource>("\"https://untrusted.example/\"").is_err());
    }

    #[test]
    fn rejects_untrusted_account_url_shapes() {
        for url in [
            "http://example.com/api/v1/",
            "https://user:secret@example.com/api/v1/",
            "https://example.com/api/v1/?token=secret",
            "https://example.com/v1/",
            "https://example.com/api/v1/#fragment",
        ] {
            assert!(matches!(
                AccountClient::new(url),
                Err(AccountError::Configuration)
            ));
        }
    }

    #[test]
    fn invoice_support_requires_both_advertised_scopes_and_compatible_protocol() {
        for (value, ready, invoices) in [
            (
                serde_json::json!({"enabled":true,"protocolVersion":1}),
                true,
                false,
            ),
            (
                serde_json::json!({"enabled":true,"protocolVersion":1,"supportedScopes":["invoices:read"]}),
                true,
                false,
            ),
            (
                serde_json::json!({"enabled":true,"protocolVersion":1,"supportedScopes":["invoices:read","invoices:write","future:unknown"]}),
                true,
                true,
            ),
            (
                serde_json::json!({"enabled":false,"protocolVersion":1,"supportedScopes":["invoices:read","invoices:write"]}),
                false,
                false,
            ),
            (
                serde_json::json!({"enabled":true,"protocolVersion":2,"supportedScopes":["invoices:read","invoices:write"]}),
                false,
                false,
            ),
        ] {
            let support: DesktopAuthorizationSupport = serde_json::from_value(value).unwrap();
            assert_eq!(support.ready(), ready);
            assert_eq!(support.invoices(), invoices);
        }
    }

    #[test]
    fn envelope_errors_never_include_server_content() {
        let error = decode::<serde_json::Value>(br#"{"code":403,"message":"Bearer secret-value"}"#)
            .unwrap_err();
        assert!(!error.to_string().contains("secret"));
        assert!(decode::<serde_json::Value>(br#"{"code":0}"#).is_err());
        assert_eq!(decode::<u64>(br#"{"code":0,"data":42}"#).unwrap(), 42);
    }
}

pub fn require_session(_capability: &DesktopAuthCapability) -> Result<(), String> {
    Err("OSIR 桌面账户授权接口尚未接通".to_string())
}

impl AccountClient {
    pub(crate) async fn get_image_contract(
        &self,
        key_id: i64,
        access_token: &str,
    ) -> Result<serde_json::Value, AccountError> {
        if key_id <= 0 || access_token.is_empty() {
            return Err(AccountError::Configuration);
        }
        let url = self
            .base
            .join(&format!("keys/{key_id}/image-contract"))
            .map_err(|_| AccountError::Configuration)?;
        self.send_and_read(
            self.http.get(url).bearer_auth(access_token),
            RequestClass::Standard,
        )
        .await
    }
}
