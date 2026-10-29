//! Native Manager compatibility transport, NOT a desktop account session.
//! Sources: Manager app/opencodex.rs; baseline-check-20261025
//! handler/codex_install_handler.go and CodexManagerConnectView.vue.
//! Register in osir/mod.rs; add aes-gcm 0.10 and rsa 0.9 (pem, sha2).
//! Call explicitly: begin -> open_browser -> wait -> exchange -> snapshot.
//! Retain AuthorizedConnect for an explicit exchange retry with the same keys.
//! Saves vault references and provider rows; never switches live configuration.
//! No OpenCodex process, automatic retry or account login.
use super::{
    connection::{provider_row, ConnectionRequest},
    key_vault::{KeyBinding, KeyTool, NativeKeySecret, GATEWAY_ORIGIN},
};
use crate::provider::Provider;
use tauri::Manager;
#[path = "../../commands/osir_recovery.rs"]
mod recovery;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use rsa::{
    pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    rand_core::OsRng,
    Oaep, RsaPrivateKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{oneshot, OwnedSemaphorePermit, Semaphore},
    time::{timeout, Instant},
};
use url::Url;

const CONNECT: &str = "https://osirclaw.com/codex-manager/connect";
const EXCHANGE: &str = "https://api.osirclaw.com/api/v1/codex-install/desktop/exchange";
const MAX_BODY: usize = 2 * 1024 * 1024;
static FLOW: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Default)]
pub struct LegacyConnectState {
    inner: tokio::sync::Mutex<LegacyAttemptState>,
    cancel: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}
#[derive(Default)]
struct LegacyAttemptState {
    authorized: Option<AuthorizedConnect>,
    bundle: Option<NativeLegacyBundle>,
}
impl LegacyConnectState {
    /// Native save integration only; never register this as an IPC command.
    pub async fn take_bundle(&self) -> Option<NativeLegacyBundle> {
        self.inner.lock().await.bundle.take()
    }
}

/// New authorization only. Uncertain outcomes require the explicit retry command
/// with the original durable encryption identity; never silently start over.
#[tauri::command]
pub async fn osir_connect_legacy(
    app: tauri::AppHandle,
    state: tauri::State<'_, LegacyConnectState>,
) -> std::result::Result<LegacyConnectSnapshot, String> {
    run_connect(app, &state, false)
        .await
        .map_err(|e| e.to_string())
}

async fn run_connect(
    app: tauri::AppHandle,
    state: &LegacyConnectState,
    resume: bool,
) -> Result<LegacyConnectSnapshot> {
    let mut inner = state
        .inner
        .try_lock()
        .map_err(|_| LegacyConnectError::Busy)?;
    let disk = read_exchange_marker(&app)?;
    if !resume && disk != "clear" {
        return Err(LegacyConnectError::RecoveryRequired);
    }
    if resume && inner.bundle.is_none() && inner.authorized.is_none() {
        if disk != "pending" {
            return Err(LegacyConnectError::RecoveryRequired);
        }
        *inner = restore_exchange(app.clone()).await?;
    }
    // A previous received-bundle write may have failed. Retry that durable
    // boundary before importing, while the original replay identity is retained.
    if inner.bundle.is_some() && inner.authorized.is_some() {
        persist_exchange(app.clone(), &inner).await?;
        inner.authorized = None;
    }
    if let Some(bundle) = inner.bundle.as_ref() {
        import_bundle(app.clone(), bundle.prepare_codex()?).await?;
        let snapshot = bundle.snapshot();
        if super::legacy_snapshot::save(&app, &snapshot).is_err() {
            log::warn!("Legacy providers imported; subscription snapshot persistence failed");
        }
        finish_exchange(app.clone()).await?;
        inner.bundle = None;
        inner.authorized = None;
        return Ok(snapshot);
    }
    if inner.authorized.is_none() {
        let (sender, mut receiver) = oneshot::channel();
        *state
            .cancel
            .lock()
            .map_err(|_| LegacyConnectError::Callback)? = Some(sender);
        // Register cancellation before RSA generation or browser launch.
        // Keep cancellation responsive even if the OS browser launcher stalls.
        let outcome = async {
            let pending = tokio::select! {
                biased;
                _ = &mut receiver => return Err(LegacyConnectError::Cancelled),
                result = PendingConnect::begin() => result?,
            };
            tokio::select! {
                biased;
                _ = &mut receiver => return Err(LegacyConnectError::Cancelled),
                result = timeout(Duration::from_secs(15), pending.open_browser()) => {
                    result.map_err(|_| LegacyConnectError::Browser)??;
                }
            }
            pending.wait(receiver).await
        }
        .await;
        state
            .cancel
            .lock()
            .map_err(|_| LegacyConnectError::Callback)?
            .take();
        inner.authorized = Some(outcome?);
    }
    persist_exchange(app.clone(), &inner).await?;
    let result = inner
        .authorized
        .as_ref()
        .ok_or(LegacyConnectError::Cancelled)?
        .exchange()
        .await;
    match result {
        Ok(bundle) => {
            inner.bundle = Some(bundle);
            // Persist the received secrets before dropping the replay identity.
            persist_exchange(app.clone(), &inner).await?;
            inner.authorized = None;
            let bundle = inner
                .bundle
                .as_ref()
                .ok_or(LegacyConnectError::InvalidBundle)?;
            import_bundle(app.clone(), bundle.prepare_codex()?).await?;
            let snapshot = bundle.snapshot();
            if super::legacy_snapshot::save(&app, &snapshot).is_err() {
                log::warn!("Legacy providers imported; subscription snapshot persistence failed");
            }
            finish_exchange(app.clone()).await?;
            inner.bundle = None;
            Ok(snapshot)
        }
        Err(error) => {
            // No HTTP failure proves that the cloud mutation did not occur.
            // Keep both the replay identity and durable pending marker.
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn osir_cancel_legacy_connect(
    state: tauri::State<'_, LegacyConnectState>,
) -> std::result::Result<(), String> {
    if let Some(sender) = state
        .cancel
        .lock()
        .map_err(|_| LegacyConnectError::Callback.to_string())?
        .take()
    {
        let _ = sender.send(());
        return Ok(());
    }
    // Never queue behind a newer flow, or erase an in-flight exchange's replay
    // identity. Import is a commit boundary and cannot safely be interrupted.
    let inner = state
        .inner
        .try_lock()
        .map_err(|_| "正在确认兑换或保存结果，请稍后重试".to_string())?;
    if inner.authorized.is_some() || inner.bundle.is_some() {
        return Err(LegacyConnectError::RecoveryRequired.to_string());
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum LegacyConnectError {
    #[error("已有连接正在进行")]
    Busy,
    #[error("无法启动本机授权回调")]
    Callback,
    #[error("无法打开授权浏览器")]
    Browser,
    #[error("连接已取消或超时，请重新授权")]
    Cancelled,
    #[error("网站未批准此次连接")]
    Denied,
    #[error("连接加密配置无效，未保存本地连接")]
    InvalidBundle,
    #[error("授权兑换结果未确认，请重试当前连接；不要重复创建密钥")]
    Uncertain,
    #[error("旧版连接服务不可用或授权已失效")]
    Unavailable,
    #[error("无法建立可信原生密钥绑定，未保存本地连接")]
    BindingUnavailable,
    #[error("本地保存未确认，请检查供应商列表并重试当前连接；不要重新创建密钥")]
    ImportUnconfirmed,
    #[error("上次云端连接结果需核对；请显式恢复原兑换，或核对网站密钥及本地列表后解除阻止。请勿重新授权")]
    RecoveryRequired,
}
type Result<T> = std::result::Result<T, LegacyConnectError>;

// Native types deliberately have no Serialize or Debug implementations.
pub struct PendingConnect {
    listener: TcpListener,
    state: String,
    verifier: String,
    redirect: Url,
    browser: Url,
    private: RsaPrivateKey,
    deadline: Instant,
    permit: OwnedSemaphorePermit,
}
pub struct AuthorizedConnect {
    code: String,
    state: String,
    verifier: String,
    redirect: String,
    private: RsaPrivateKey,
    public: String,
    idempotency: String,
    deadline: Instant,
    _permit: OwnedSemaphorePermit,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LegacyAccountSnapshot {
    #[serde(alias = "userId")]
    pub user_id: i64,
    #[serde(default, alias = "displayName")]
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub balance: f64,
    #[serde(default)]
    pub subscriptions: Vec<LegacySubscriptionSnapshot>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LegacySubscriptionSnapshot {
    pub id: i64,
    pub group_name: Option<String>,
    pub status: String,
    pub expires_at: Option<String>,
    pub days_remaining: i32,
    pub monthly_used_usd: f64,
    pub monthly_limit_usd: f64,
    pub monthly_remaining_usd: f64,
}
/// Safe projection only. Never translate this to SessionRead::Authorized.
#[derive(Serialize)]
pub struct LegacyConnectSnapshot {
    pub kind: &'static str,
    pub account: LegacyAccountSnapshot,
    pub providers: Vec<LegacyProviderSummary>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LegacyProviderSummary {
    pub platform: String,
    pub provider: String,
    pub key_id: i64,
    pub models: Vec<String>,
    pub recommended_model: String,
}
#[derive(Deserialize)]
struct EncryptedBundle {
    wrapped_key: String,
    iv: String,
    ciphertext: String,
}
#[derive(Deserialize)]
struct ExchangeResponse {
    encrypted_bundle: EncryptedBundle,
    providers: Vec<LegacyProviderSummary>,
}
#[derive(Deserialize, Serialize)]
struct LegacyRoute {
    platform: String,
    provider: String,
    key_id: i64,
    #[serde(alias = "apiKey")]
    api_key: String,
    adapter: String,
    #[serde(alias = "baseUrl")]
    base_url: String,
    models: Vec<String>,
    #[serde(alias = "recommendedModel")]
    recommended_model: String,
}
#[derive(Deserialize, Serialize)]
pub struct NativeLegacyBundle {
    config_schema: u32,
    account: LegacyAccountSnapshot,
    providers: Vec<LegacyRoute>,
}
/// Journal vault storage, attach ManagedBinding from vault metadata, then save
/// through ProviderService. This plan is neither persisted nor activated here.
pub struct PreparedConnection {
    pub provider: Provider,
    pub binding: KeyBinding,
    pub secret: NativeKeySecret,
}

fn nonce() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}
impl PendingConnect {
    pub async fn begin() -> Result<Self> {
        let permit = FLOW
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| LegacyConnectError::Busy)?;
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|_| LegacyConnectError::Callback)?;
        let port = listener
            .local_addr()
            .map_err(|_| LegacyConnectError::Callback)?
            .port();
        let redirect = Url::parse(&format!("http://127.0.0.1:{port}/oauth/callback"))
            .map_err(|_| LegacyConnectError::Callback)?;
        let state = nonce();
        let verifier = nonce();
        let private = tokio::task::spawn_blocking(|| RsaPrivateKey::new(&mut OsRng, 3072))
            .await
            .map_err(|_| LegacyConnectError::InvalidBundle)?
            .map_err(|_| LegacyConnectError::InvalidBundle)?;
        let mut browser = Url::parse(CONNECT).map_err(|_| LegacyConnectError::Browser)?;
        browser
            .query_pairs_mut()
            .append_pair("state", &state)
            .append_pair("redirect_uri", redirect.as_str())
            .append_pair(
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            )
            .append_pair("code_challenge_method", "S256");
        Ok(Self {
            listener,
            state,
            verifier,
            redirect,
            browser,
            private,
            deadline: Instant::now() + Duration::from_secs(300),
            permit,
        })
    }
    /// On macOS, explicitly prefer Chrome, falling back only when absent.
    pub async fn open_browser(&self) -> Result<()> {
        let url = self.browser.to_string();
        tokio::task::spawn_blocking(move || {
            #[cfg(target_os = "macos")]
            {
                let installed = std::process::Command::new("/usr/bin/open")
                    .args(["-Ra", "Google Chrome"])
                    .output()
                    .map_err(|_| LegacyConnectError::Browser)?
                    .status
                    .success();
                let mut command = std::process::Command::new("/usr/bin/open");
                if installed {
                    command.args(["-a", "Google Chrome"]);
                }
                if command
                    .arg(&url)
                    .output()
                    .map_err(|_| LegacyConnectError::Browser)?
                    .status
                    .success()
                {
                    Ok(())
                } else {
                    Err(LegacyConnectError::Browser)
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                #[cfg(target_os = "windows")]
                let mut command = {
                    let mut c = std::process::Command::new("rundll32.exe");
                    c.arg("url.dll,FileProtocolHandler");
                    c
                };
                #[cfg(not(target_os = "windows"))]
                let mut command = std::process::Command::new("xdg-open");
                if command
                    .arg(&url)
                    .output()
                    .map_err(|_| LegacyConnectError::Browser)?
                    .status
                    .success()
                {
                    Ok(())
                } else {
                    Err(LegacyConnectError::Browser)
                }
            }
        })
        .await
        .map_err(|_| LegacyConnectError::Browser)?
    }
    pub async fn wait(self, mut cancelled: oneshot::Receiver<()>) -> Result<AuthorizedConnect> {
        let code = tokio::select! {
            biased;
            _ = &mut cancelled => return Err(LegacyConnectError::Cancelled),
            _ = tokio::time::sleep_until(self.deadline) => return Err(LegacyConnectError::Cancelled),
            result = self.receive() => result?,
        };
        let public = self
            .private
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|_| LegacyConnectError::InvalidBundle)?;
        Ok(AuthorizedConnect {
            code,
            state: self.state,
            verifier: self.verifier,
            redirect: self.redirect.to_string(),
            private: self.private,
            public,
            idempotency: uuid::Uuid::new_v4().to_string(),
            deadline: Instant::now() + Duration::from_secs(600),
            _permit: self.permit,
        })
    }
    async fn receive(&self) -> Result<String> {
        loop {
            let (mut stream, peer) = self
                .listener
                .accept()
                .await
                .map_err(|_| LegacyConnectError::Callback)?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let head = timeout(Duration::from_secs(2), async {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let n = stream.read(&mut buf).await.map_err(|_| ())?;
                    if n == 0 || head.len() + n > 8192 {
                        return Err(());
                    }
                    head.extend_from_slice(&buf[..n]);
                    if head.windows(4).any(|w| w == b"\r\n\r\n") {
                        return String::from_utf8(head).map_err(|_| ());
                    }
                }
            })
            .await;
            let Ok(Ok(head)) = head else {
                continue;
            };
            let result = parse_callback(&head, &self.redirect, &self.state);
            let status = if result.is_some() {
                "200 OK"
            } else {
                "400 Bad Request"
            };
            let body = "Return to OSIR Client. Connection will be confirmed in the app.";
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            let _ = timeout(
                Duration::from_secs(1),
                stream.write_all(response.as_bytes()),
            )
            .await;
            if let Some(result) = result {
                return result;
            }
        }
    }
}
fn parse_callback(head: &str, redirect: &Url, state: &str) -> Option<Result<String>> {
    let mut lines = head.split("\r\n");
    let parts: Vec<_> = lines.next()?.split_whitespace().collect();
    if parts.len() != 3
        || parts[0] != "GET"
        || parts[2] != "HTTP/1.1"
        || !parts[1].starts_with("/oauth/callback?")
    {
        return None;
    }
    let host = format!("127.0.0.1:{}", redirect.port()?);
    let hosts: Vec<_> = lines
        .filter_map(|l| l.split_once(':'))
        .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.trim())
        .collect();
    if hosts != [host.as_str()] {
        return None;
    }
    let callback = redirect.join(parts[1]).ok()?;
    if callback.path() != "/oauth/callback" || callback.fragment().is_some() {
        return None;
    }
    let mut params = std::collections::BTreeMap::new();
    for (name, value) in callback.query_pairs() {
        if params
            .insert(name.into_owned(), value.into_owned())
            .is_some()
        {
            return None;
        }
    }
    if params.get("state")?.as_str() != state {
        return None;
    }
    match (params.get("code"), params.get("error")) {
        (Some(code), None)
            if !code.is_empty() && code.len() <= 4096 && !code.chars().any(char::is_control) =>
        {
            Some(Ok(code.clone()))
        }
        (None, Some(_)) => Some(Err(LegacyConnectError::Denied)),
        _ => None,
    }
}
impl AuthorizedConnect {
    /// Explicit retry reuses this object's RSA key and idempotency key. Network
    /// failures may have created cloud keys. Never silently start a new flow.
    pub async fn exchange(&self) -> Result<NativeLegacyBundle> {
        if Instant::now() >= self.deadline {
            return Err(LegacyConnectError::Cancelled);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| LegacyConnectError::Unavailable)?;
        let mut response = client
            .post(EXCHANGE)
            .header("Cache-Control", "no-store")
            .json(&serde_json::json!({
                "authorization_code": self.code, "state": self.state,
                "redirect_uri": self.redirect, "code_verifier": self.verifier,
                "client_public_key": self.public, "idempotency_key": self.idempotency,
                "installer_version": concat!("osir-client-", env!("CARGO_PKG_VERSION")),
                "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            }))
            .send()
            .await
            .map_err(|_| LegacyConnectError::Uncertain)?;
        if !response.status().is_success() {
            return Err(
                if response.status().is_server_error() || response.status().as_u16() == 429 {
                    LegacyConnectError::Uncertain
                } else {
                    LegacyConnectError::Unavailable
                },
            );
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| LegacyConnectError::Uncertain)?
        {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(LegacyConnectError::InvalidBundle);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| LegacyConnectError::InvalidBundle)?;
        let response: ExchangeResponse =
            serde_json::from_value(value.get("data").cloned().unwrap_or(value))
                .map_err(|_| LegacyConnectError::InvalidBundle)?;
        let private = self.private.clone();
        tokio::task::spawn_blocking(move || decrypt(private, response))
            .await
            .map_err(|_| LegacyConnectError::InvalidBundle)?
    }
}
fn decrypt(private: RsaPrivateKey, response: ExchangeResponse) -> Result<NativeLegacyBundle> {
    let decode = |s: &str| {
        STANDARD
            .decode(s)
            .map_err(|_| LegacyConnectError::InvalidBundle)
    };
    let encrypted = response.encrypted_bundle;
    let mut key = private
        .decrypt(Oaep::new::<Sha256>(), &decode(&encrypted.wrapped_key)?)
        .map_err(|_| LegacyConnectError::InvalidBundle)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| LegacyConnectError::InvalidBundle);
    key.fill(0);
    let cipher = cipher?;
    let iv = decode(&encrypted.iv)?;
    if iv.len() != 12 {
        return Err(LegacyConnectError::InvalidBundle);
    }
    let mut plaintext = cipher
        .decrypt(
            Nonce::from_slice(&iv),
            decode(&encrypted.ciphertext)?.as_ref(),
        )
        .map_err(|_| LegacyConnectError::InvalidBundle)?;
    let bundle = serde_json::from_slice::<NativeLegacyBundle>(&plaintext);
    plaintext.fill(0);
    let bundle = bundle.map_err(|_| LegacyConnectError::InvalidBundle)?;
    bundle.validate(&response.providers)?;
    Ok(bundle)
}
fn valid_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.trim() == s && !s.chars().any(char::is_control)
}
impl NativeLegacyBundle {
    fn summaries(&self) -> Vec<LegacyProviderSummary> {
        self.providers
            .iter()
            .map(|p| LegacyProviderSummary {
                platform: p.platform.clone(),
                provider: p.provider.clone(),
                key_id: p.key_id,
                models: p.models.clone(),
                recommended_model: p.recommended_model.clone(),
            })
            .collect()
    }
    fn validate(&self, summaries: &[LegacyProviderSummary]) -> Result<()> {
        if self.config_schema != 1
            || self.account.user_id <= 0
            || !self.account.balance.is_finite()
            || self.providers.is_empty()
            || self.providers.len() > 32
        {
            return Err(LegacyConnectError::InvalidBundle);
        }
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for p in &self.providers {
            let models: BTreeSet<_> = p.models.iter().collect();
            if !valid_text(&p.provider, 96)
                || !valid_text(&p.platform, 96)
                || !ids.insert(&p.provider)
                || !keys.insert(p.key_id)
                || p.key_id <= 0
                || p.adapter != "openai-responses"
                || p.base_url != format!("{GATEWAY_ORIGIN}/v1")
                || p.models.is_empty()
                || p.models.len() > 256
                || models.len() != p.models.len()
                || p.models.iter().any(|m| !valid_text(m, 256))
                || !p.models.contains(&p.recommended_model)
                || !valid_text(&p.api_key, 16384)
            {
                return Err(LegacyConnectError::InvalidBundle);
            }
        }
        let mut actual = self.summaries();
        let mut expected = summaries.to_vec();
        for list in [&mut actual, &mut expected] {
            for p in list.iter_mut() {
                p.models.sort();
            }
            list.sort_by(|a, b| a.provider.cmp(&b.provider));
        }
        if actual != expected {
            return Err(LegacyConnectError::InvalidBundle);
        }
        Ok(())
    }
    pub fn snapshot(&self) -> LegacyConnectSnapshot {
        LegacyConnectSnapshot {
            kind: "legacy_snapshot",
            account: self.account.clone(),
            providers: self.summaries(),
        }
    }
    /// The old exchange has no group claim. None retains this legacy limitation;
    /// no image capabilities are granted from this account snapshot.
    pub fn prepare_codex(&self) -> Result<Vec<PreparedConnection>> {
        let mut connections = Vec::with_capacity(self.providers.len());
        for p in &self.providers {
            let id = format!("osir-legacy-{}-{}", self.account.user_id, p.key_id);
            let binding = KeyBinding {
                account_origin: super::managed::trusted_account_origin()
                    .map_err(|_| LegacyConnectError::BindingUnavailable)?,
                account_id: self.account.user_id.to_string(),
                key_id: p.key_id,
                group_id: None,
                provider_id: id.clone(),
                gateway_origin: GATEWAY_ORIGIN.into(),
                tool: KeyTool::Codex,
            };
            let request = ConnectionRequest {
                tool: "codex".into(),
                key_id: p.key_id,
                name: format!("OSIR {}", p.platform),
                hide_aggregation_name: None,
                model: p.recommended_model.clone(),
                models: Vec::new(),
                generation: 0,
            };
            let mut provider =
                provider_row(&request, id).map_err(|_| LegacyConnectError::InvalidBundle)?;
            provider.settings_config["modelCatalog"] = serde_json::json!({"models":p.models.iter().map(|m| serde_json::json!({"model":m})).collect::<Vec<_>>()});
            connections.push(PreparedConnection {
                provider,
                binding,
                secret: NativeKeySecret::new(p.api_key.clone())
                    .map_err(|_| LegacyConnectError::InvalidBundle)?,
            });
        }
        Ok(connections)
    }
}

// Dedicated journal directory and lock. The shared recovery algorithm checks
// DB references before cleanup and preserves secrets on unknown DB state.
async fn import_bundle(app: tauri::AppHandle, plans: Vec<PreparedConnection>) -> Result<()> {
    tokio::task::spawn_blocking(move || -> std::result::Result<(), String> {
        let vault = super::managed::vault().map_err(|_| "vault unavailable")?;
        let _operation = recovery::lock()?;
        let directory = app
            .path()
            .app_data_dir()
            .map_err(|_| "journal unavailable")?
            .join("legacy-connect");
        let journal = recovery::Journal::new(&directory);
        let application = app.state::<crate::store::AppState>();
        recovery::recover_database(&journal, &application.db, &vault)?;
        journal.ensure_clear()?;
        for mut plan in plans {
            if let Some(existing) = application
                .db
                .get_provider_by_id(&plan.provider.id, "codex")
                .map_err(|_| "database unavailable")?
            {
                let saved = existing
                    .meta
                    .as_ref()
                    .and_then(|m| m.osir_binding.as_ref())
                    .ok_or("provider collision")?;
                if saved.source != crate::provider::AuthBindingSource::ManagedAccount
                    || saved.binding != plan.binding
                {
                    return Err("provider collision".into());
                }
                let secret = vault
                    .load(&saved.reference, &plan.binding)
                    .map_err(|_| "credential unavailable")?
                    .ok_or("credential missing")?;
                if secret.expose() != plan.secret.expose() {
                    return Err("credential changed".into());
                }
                super::managed::validate_provider(&crate::app_config::AppType::Codex, &existing)
                    .map_err(|_| "provider invalid")?;
                continue;
            }
            let prepared = vault
                .prepare_store(&plan.binding, &plan.secret)
                .map_err(|_| "credential preparation failed")?;
            journal.record(&prepared, recovery::Phase::Intent)?;
            let stored = match vault.store_prepared(&prepared, &plan.secret) {
                Ok(stored) => stored,
                Err(_) => {
                    journal.record(&prepared, recovery::Phase::CleanupPending)?;
                    recovery::recover_database(&journal, &application.db, &vault)?;
                    return Err("credential save unconfirmed".into());
                }
            };
            journal.record(&stored, recovery::Phase::Stored)?;
            plan.provider
                .meta
                .get_or_insert_with(Default::default)
                .osir_binding = Some(crate::provider::ManagedBinding {
                source: crate::provider::AuthBindingSource::ManagedAccount,
                binding: stored.binding.clone(),
                reference: stored.reference.clone(),
                masked_key: stored.masked_key.clone(),
            });
            let outcome = super::managed::validate_provider(
                &crate::app_config::AppType::Codex,
                &plan.provider,
            )
            .map_err(|_| "provider invalid".to_string())
            .and_then(|_| {
                crate::services::ProviderService::add_without_switching(
                    application.inner(),
                    crate::app_config::AppType::Codex,
                    plan.provider,
                )
                .map_err(|_| "provider save unconfirmed".to_string())
            });
            if let Err(error) = outcome {
                journal.record(&stored, recovery::Phase::CleanupPending)?;
                recovery::recover_database(&journal, &application.db, &vault)?;
                return Err(error);
            }
            journal.record(&stored, recovery::Phase::Committed)?;
            journal.clear()?;
        }
        Ok(())
    })
    .await
    .map_err(|_| LegacyConnectError::ImportUnconfirmed)?
    .map_err(|_| LegacyConnectError::ImportUnconfirmed)
}

/// One startup pass only; no timer or automatic authorization/network request.
pub fn recover_in_background(app: tauri::AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| -> std::result::Result<(), String> {
            let vault = super::managed::vault().map_err(|_| "vault unavailable")?;
            let _operation = recovery::lock()?;
            let directory = app
                .path()
                .app_data_dir()
                .map_err(|_| "journal unavailable")?
                .join("legacy-connect");
            let application = app.state::<crate::store::AppState>();
            recovery::recover_database(&recovery::Journal::new(&directory), &application.db, &vault)
        })();
        if result.is_err() {
            log::warn!("Legacy connection credential recovery remains pending");
        }
    });
}

// Only the OS credential store receives this private payload. Never IPC/log it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableExchange {
    version: u8,
    code: String,
    state: String,
    verifier: String,
    redirect: String,
    private_pem: String,
    public: String,
    idempotency: String,
    expires_at: i64,
    bundle: Option<NativeLegacyBundle>,
}

fn exchange_service(app: &tauri::AppHandle) -> String {
    format!(
        "{}.osir.legacy-exchange.v1.{}",
        app.config().identifier,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    )
}
fn exchange_marker(app: &tauri::AppHandle) -> Result<std::path::PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|_| LegacyConnectError::RecoveryRequired)?
        .join("legacy-connect")
        .join("exchange-state-v1"))
}
fn read_exchange_marker(app: &tauri::AppHandle) -> Result<String> {
    let path = exchange_marker(app)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("clear".into()),
        Err(_) => return Err(LegacyConnectError::RecoveryRequired),
    };
    if !metadata.file_type().is_file() || metadata.len() > 32 {
        return Err(LegacyConnectError::RecoveryRequired);
    }
    let value = std::fs::read_to_string(path).map_err(|_| LegacyConnectError::RecoveryRequired)?;
    if value != "clear" && value != "pending" && value != "unknown" {
        return Err(LegacyConnectError::RecoveryRequired);
    }
    Ok(value)
}
fn write_exchange_marker(app: &tauri::AppHandle, value: &str) -> Result<()> {
    let path = exchange_marker(app)?;
    let dir = path.parent().ok_or(LegacyConnectError::RecoveryRequired)?;
    std::fs::create_dir_all(dir).map_err(|_| LegacyConnectError::RecoveryRequired)?;
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(LegacyConnectError::RecoveryRequired);
    }
    crate::config::stage_write(&path, value.as_bytes(), Some(0o600), true)
        .and_then(|staged| staged.commit())
        .map_err(|_| LegacyConnectError::RecoveryRequired)?;
    std::fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| LegacyConnectError::RecoveryRequired)
}

async fn persist_exchange(app: tauri::AppHandle, inner: &LegacyAttemptState) -> Result<()> {
    let authorized = inner
        .authorized
        .as_ref()
        .ok_or(LegacyConnectError::RecoveryRequired)?;
    // Serialize while borrowed, then move bytes onto a blocking Keychain worker.
    let mut value = serde_json::json!({
        "version": 1, "code": authorized.code, "state": authorized.state,
        "verifier": authorized.verifier, "redirect": authorized.redirect,
        "private_pem": authorized.private.to_pkcs8_pem(LineEnding::LF)
            .map_err(|_| LegacyConnectError::RecoveryRequired)?.as_str(),
        "public": authorized.public, "idempotency": authorized.idempotency,
        "expires_at": chrono::Utc::now().timestamp() + authorized.deadline.saturating_duration_since(Instant::now()).as_secs() as i64,
        "bundle": inner.bundle,
    });
    let bytes = serde_json::to_vec(&value).map_err(|_| LegacyConnectError::RecoveryRequired)?;
    value = serde_json::Value::Null;
    drop(value);
    tokio::task::spawn_blocking(move || {
        use super::credentials::CredentialBackend;
        // Clear any completed attempt left by a failed cleanup before marking a
        // new one pending, so a failed write cannot expose an older bundle as
        // the new attempt's recovery payload. No exchange has started yet.
        if read_exchange_marker(&app)? == "clear" {
            super::credentials::NativeKeychain
                .remove(&exchange_service(&app), "attempt")
                .map_err(|_| LegacyConnectError::RecoveryRequired)?;
        }
        // A missing/failed credential write must never permit another grant.
        write_exchange_marker(&app, "pending")?;
        let mut bytes = bytes;
        let result = super::credentials::NativeKeychain
            .write(&exchange_service(&app), "attempt", &bytes)
            .map_err(|_| LegacyConnectError::RecoveryRequired);
        bytes.fill(0);
        result
    })
    .await
    .map_err(|_| LegacyConnectError::RecoveryRequired)?
}
async fn restore_exchange(app: tauri::AppHandle) -> Result<LegacyAttemptState> {
    tokio::task::spawn_blocking(move || {
        use super::credentials::CredentialBackend;
        let mut bytes = super::credentials::NativeKeychain
            .read(&exchange_service(&app), "attempt")
            .map_err(|_| LegacyConnectError::RecoveryRequired)?
            .ok_or(LegacyConnectError::RecoveryRequired)?;
        if bytes.len() > MAX_BODY * 2 {
            bytes.fill(0);
            return Err(LegacyConnectError::RecoveryRequired);
        }
        let wire = serde_json::from_slice::<DurableExchange>(&bytes);
        bytes.fill(0);
        let wire = wire.map_err(|_| LegacyConnectError::RecoveryRequired)?;
        if wire.version != 1 {
            return Err(LegacyConnectError::RecoveryRequired);
        }
        if let Some(bundle) = wire.bundle {
            bundle.validate(&bundle.summaries())?;
            return Ok(LegacyAttemptState {
                authorized: None,
                bundle: Some(bundle),
            });
        }
        let seconds = wire.expires_at - chrono::Utc::now().timestamp();
        if !(1..=600).contains(&seconds) {
            return Err(LegacyConnectError::RecoveryRequired);
        }
        let private = RsaPrivateKey::from_pkcs8_pem(&wire.private_pem)
            .map_err(|_| LegacyConnectError::RecoveryRequired)?;
        if private
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|_| LegacyConnectError::RecoveryRequired)?
            != wire.public
        {
            return Err(LegacyConnectError::RecoveryRequired);
        }
        let permit = FLOW
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| LegacyConnectError::Busy)?;
        Ok(LegacyAttemptState {
            authorized: Some(AuthorizedConnect {
                code: wire.code,
                state: wire.state,
                verifier: wire.verifier,
                redirect: wire.redirect,
                private,
                public: wire.public,
                idempotency: wire.idempotency,
                deadline: Instant::now() + Duration::from_secs(seconds as u64),
                _permit: permit,
            }),
            bundle: None,
        })
    })
    .await
    .map_err(|_| LegacyConnectError::RecoveryRequired)?
}
async fn finish_exchange(app: tauri::AppHandle) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        use super::credentials::CredentialBackend;
        // Import has committed (or the user explicitly reconciled). Persist
        // completion before deleting the only recovery payload: a failed marker
        // write must leave the pending attempt replayable after restart.
        write_exchange_marker(&app, "clear")?;
        if super::credentials::NativeKeychain
            .remove(&exchange_service(&app), "attempt")
            .is_err()
        {
            // A stale private record cannot be restored while the marker is
            // clear; the next durable attempt overwrites the same vault slot.
            log::warn!("Legacy connection completed; exchange credential cleanup pending");
        }
        Ok(())
    })
    .await
    .map_err(|_| LegacyConnectError::RecoveryRequired)?
}

/// Explicit replay only; never invoked during startup or ordinary new connect.
#[tauri::command]
pub async fn osir_retry_legacy_connect(
    app: tauri::AppHandle,
    state: tauri::State<'_, LegacyConnectState>,
) -> std::result::Result<LegacyConnectSnapshot, String> {
    run_connect(app, &state, true)
        .await
        .map_err(|e| e.to_string())
}

/// User attestation after reviewing cloud keys and local saved providers.
/// Does not claim cloud rollback, revoke keys, or make any network request.
#[tauri::command]
pub async fn osir_reconcile_legacy_connect(
    app: tauri::AppHandle,
    state: tauri::State<'_, LegacyConnectState>,
    confirmed_cloud_reviewed: bool,
) -> std::result::Result<(), String> {
    if !confirmed_cloud_reviewed {
        return Err(LegacyConnectError::RecoveryRequired.to_string());
    }
    let mut inner = state
        .inner
        .try_lock()
        .map_err(|_| LegacyConnectError::Busy.to_string())?;
    finish_exchange(app).await.map_err(|e| e.to_string())?;
    inner.authorized = None;
    inner.bundle = None;
    Ok(())
}

#[tauri::command]
pub fn osir_legacy_recovery_status(app: tauri::AppHandle) -> std::result::Result<String, String> {
    read_exchange_marker(&app).map_err(|e| e.to_string())
}
