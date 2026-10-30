//! Native session coordinator. Raw credentials and grants never implement IPC serde.
#[path = "../../commands/osir_invoice.rs"]
pub mod invoice;
use super::{
    account::AccountProfile,
    auth::{validate_scope, AuthConfig},
    authorization::{AuthorizationGrant, PendingAuthorization},
    client::{AccountClient, AccountError, AccountResource, AuthEndpoint},
    credentials::{
        CredentialBackend, CredentialError, CredentialStore, NativeKeychain, SecretRecord,
        SessionJournal,
    },
    models::{LogoutReport, SessionRecovery, SessionStatus},
};
use serde::{de::DeserializeOwned, Deserialize};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

const CLIENT_ID: &str = "osir-desktop";
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("桌面授权尚未启用")]
    Disabled,
    #[error("桌面授权配置无效")]
    Configuration,
    #[error("授权已取消或会话已改变")]
    Superseded,
    #[error("请先退出当前账户或完成本地凭据清理")]
    AlreadyConnected,
    #[error("请重新进行浏览器授权")]
    Unauthorized,
    #[error("授权响应格式无效")]
    InvalidResponse,
    #[error("账户服务暂时不可用")]
    Unavailable,
    #[error("安全凭据存储失败")]
    Storage,
    #[error("浏览器授权未完成")]
    Authorization,
}
impl From<AccountError> for SessionError {
    fn from(e: AccountError) -> Self {
        match e {
            AccountError::Unauthorized => Self::Unauthorized,
            _ => Self::Unavailable,
        }
    }
}
impl From<CredentialError> for SessionError {
    fn from(_: CredentialError) -> Self {
        Self::Storage
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateSessionKeyError {
    #[error(transparent)]
    Session(SessionError),
    #[error(transparent)]
    Request(super::client::CreateKeyError),
    #[error("创建密钥需要显式重新授权 keys:write")]
    WriteScopeRequired,
}

pub struct LoginAttempt {
    pending: PendingAuthorization,
    requested_scope: String,
    cancelled: oneshot::Receiver<()>,
    generation: u64,
}
impl LoginAttempt {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn browser_url(&self) -> &str {
        self.pending.browser_url()
    }
    pub async fn wait(self) -> Result<LoginGrant, SessionError> {
        let grant = self
            .pending
            .wait(self.cancelled)
            .await
            .map_err(|_| SessionError::Authorization)?;
        Ok(LoginGrant {
            grant,
            generation: self.generation,
            requested_scope: self.requested_scope,
        })
    }
}
pub struct LogoutTicket {
    generation: u64,
    record: Option<Arc<SecretRecord>>,
    account: Option<String>,
    slot: Option<String>,
    tombstoned: bool,
}
pub struct LoginGrant {
    grant: AuthorizationGrant,
    generation: u64,
    requested_scope: String,
}
/// Native-only result: project an explicit safe DTO before returning data to IPC.
pub struct SessionRead<T> {
    pub generation: u64,
    pub data: T,
}

// Synchronous drop guard also covers cancellation of an async command future.
struct InvalidationGuard<'a> {
    state: &'a Mutex<State>,
    journal: &'a SessionJournal,
    generation: u64,
    armed: bool,
}
impl Drop for InvalidationGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut state = self.state.lock().unwrap();
            if state.generation == self.generation {
                state.invalidate();
                state.record = None;
                state.profile = None;
                let _ = self.journal.write_slot(
                    state.cleanup_account.as_deref(),
                    state.cleanup_slot.as_deref(),
                    false,
                );
            }
        }
    }
}
// A trusted reference has only been read, so cancellation cannot make a rotating
// token unsafe. Keep the reference active and allow an explicit Keychain retry.
struct CredentialReadGuard<'a> {
    state: &'a Mutex<State>,
    generation: u64,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    armed: bool,
}
impl Drop for CredentialReadGuard<'_> {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if self.armed {
            let mut state = self.state.lock().unwrap();
            if state.generation == self.generation && state.recovery == SessionRecovery::Restoring {
                state.recovery = SessionRecovery::UnlockRequired;
                state.cancel = None;
            }
        }
    }
}
// A dropped recovery future must not leave the welcome screen busy forever.
// Token rotation has its own fail-closed invalidation guard.
struct RecoveryGuard<'a> {
    state: &'a Mutex<State>,
    generation: u64,
}
impl Drop for RecoveryGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        if state.generation == self.generation && state.recovery == SessionRecovery::Restoring {
            state.recovery = if state.record.is_some() {
                SessionRecovery::Idle
            } else {
                SessionRecovery::Blocked
            };
        }
    }
}
struct State {
    generation: u64,
    recovery: SessionRecovery,
    // Preserve credentials after an uncertain rotation, but never replay it.
    refresh_blocked: bool,
    cancel: Option<oneshot::Sender<()>>,
    record: Option<Arc<SecretRecord>>,
    profile: Option<AccountProfile>,
    // Retained only as metadata if deletion fails, allowing a logout retry.
    cleanup_account: Option<String>,
    cleanup_slot: Option<String>,
}
impl State {
    fn invalidate(&mut self) {
        self.recovery = SessionRecovery::Idle;
        self.refresh_blocked = false;
        self.generation = self.generation.wrapping_add(1);
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
    fn status(&self) -> SessionStatus {
        let connected = self.record.is_some() && self.profile.is_some();
        let recovery = match self.recovery {
            SessionRecovery::Idle if !connected && self.refresh_blocked => SessionRecovery::Blocked,
            SessionRecovery::Idle if !connected && self.record.is_some() => {
                SessionRecovery::Retryable
            }
            SessionRecovery::Idle if !connected && self.cleanup_account.is_some() => {
                SessionRecovery::Blocked
            }
            other => other,
        };
        SessionStatus {
            connected,
            recovery,
            can_create_keys: connected
                && self.record.as_ref().is_some_and(|record| {
                    record.scope.split(' ').any(|scope| scope == "keys:write")
                }),
            generation: self.generation,
            profile: if connected {
                self.profile.clone()
            } else {
                None
            },
        }
    }
}
pub struct AuthManager<B = NativeKeychain> {
    config: AuthConfig,
    client: AccountClient,
    credentials: Arc<CredentialStore<B>>,
    journal: Arc<SessionJournal>,
    state: Arc<Mutex<State>>,
    keychain_gate: Arc<Mutex<()>>,
    // Serializes refresh/exchange/storage/cleanup, never status or invalidation.
    operation: AsyncMutex<()>,
    // Coalesce startup/explicit recovery; followers never queue another read or rotation.
    recovery: AsyncMutex<()>,
    readiness: AsyncMutex<
        Option<(
            std::time::Instant,
            super::models::DesktopAuthCapability,
            bool,
        )>,
    >,
}
impl AuthManager<NativeKeychain> {
    pub fn new(config: AuthConfig, identity: &str) -> Result<Self, SessionError> {
        if config.enabled && !cfg!(any(target_os = "macos", target_os = "windows")) {
            return Err(SessionError::Storage);
        }
        let client = config.validate()?;
        let credentials = CredentialStore::new(&client.origin(), identity)?;
        Self::with_store(config, credentials)
    }
    pub fn new_persistent(
        config: AuthConfig,
        identity: &str,
        directory: &std::path::Path,
    ) -> Result<Self, SessionError> {
        let mut manager = Self::new(config, identity)?;
        manager.journal = Arc::new(SessionJournal::persistent(
            manager.client.origin(),
            directory,
        ));
        Ok(manager)
    }
}
impl<B: CredentialBackend + 'static> AuthManager<B> {
    pub fn with_store(
        config: AuthConfig,
        credentials: CredentialStore<B>,
    ) -> Result<Self, SessionError> {
        let client = config.validate()?;
        let recovery = if config.enabled {
            SessionRecovery::Pending
        } else {
            SessionRecovery::Idle
        };
        Ok(Self {
            config,
            journal: Arc::new(SessionJournal::memory(client.origin())),
            client,
            credentials: Arc::new(credentials),
            keychain_gate: Arc::new(Mutex::new(())),
            state: Arc::new(Mutex::new(State {
                generation: 0,
                recovery,
                refresh_blocked: false,
                cancel: None,
                record: None,
                profile: None,
                cleanup_account: None,
                cleanup_slot: None,
            })),
            operation: AsyncMutex::new(()),
            recovery: AsyncMutex::new(()),
            readiness: AsyncMutex::new(None),
        })
    }
    /// Native support. Remote readiness is checked separately.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }
    fn require_enabled(&self) -> Result<(), SessionError> {
        if !self.config.enabled {
            return Err(SessionError::Disabled);
        }
        Ok(())
    }
    pub fn capability(&self) -> super::models::DesktopAuthCapability {
        if self.config.enabled && cfg!(any(target_os = "macos", target_os = "windows")) {
            super::models::DesktopAuthCapability {
                status: super::models::DesktopAuthStatus::Available,
                reason: "桌面浏览器授权已由原生部署配置启用".into(),
            }
        } else {
            super::models::DesktopAuthCapability::default()
        }
    }
    pub fn status(&self) -> SessionStatus {
        self.state.lock().unwrap().status()
    }
    /// Explicit recovery escape hatch. Abandon only the local restore reference:
    /// never load/delete stored credentials or revoke a cloud grant. Persist the
    /// tombstone before changing memory so failure leaves the old login intact.
    pub fn abandon_saved_login(&self, expected: u64) -> Result<SessionStatus, SessionError> {
        self.require_enabled()?;
        let _recovery = self
            .recovery
            .try_lock()
            .map_err(|_| SessionError::Unavailable)?;
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| SessionError::Unavailable)?;
        let mut state = self.state.lock().unwrap();
        if state.generation != expected {
            return Err(SessionError::Superseded);
        }
        let status = state.status();
        if status.connected
            || !matches!(
                status.recovery,
                SessionRecovery::UnlockRequired
                    | SessionRecovery::Retryable
                    | SessionRecovery::Blocked
            )
        {
            return Err(SessionError::AlreadyConnected);
        }
        self.journal.write(None, false)?;
        state.invalidate();
        state.record = None;
        state.profile = None;
        state.cleanup_account = None;
        state.cleanup_slot = None;
        Ok(state.status())
    }
    /// Resume a preserved native grant after a temporary startup profile failure.
    /// No new consent, credential replacement, or cleanup bypass is performed.
    pub async fn recover_existing(&self) -> Result<Option<SessionStatus>, SessionError> {
        self.recover_existing_for_generation(self.status().generation)
            .await
    }
    pub async fn recover_existing_for_generation(
        &self,
        expected: u64,
    ) -> Result<Option<SessionStatus>, SessionError> {
        self.require_enabled()?;
        self.check(expected)?;
        if self.status().recovery == SessionRecovery::UnlockRequired {
            return self.restore_saved(true).await.map(Some);
        }
        let _recovery = match self.recovery.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                let _finished = self.recovery.lock().await;
                self.check(expected)?;
                let state = self.state.lock().unwrap();
                return Ok(state.record.as_ref().map(|_| state.status()));
            }
        };
        let (generation, record) = {
            let mut state = self.state.lock().unwrap();
            if state.generation != expected {
                return Err(SessionError::Superseded);
            }
            if matches!(
                state.recovery,
                SessionRecovery::Pending | SessionRecovery::Blocked
            ) || state.refresh_blocked
            {
                return Err(SessionError::Unavailable);
            }
            let Some(record) = state.record.clone() else {
                return if state.cleanup_account.is_some() {
                    Err(SessionError::AlreadyConnected)
                } else {
                    Ok(None)
                };
            };
            if state.profile.is_some() {
                return Ok(Some(state.status()));
            }
            state.recovery = SessionRecovery::Restoring;
            (state.generation, record)
        };
        let _completion = RecoveryGuard {
            state: &self.state,
            generation,
        };
        let reply = self
            .read_account::<AccountProfile>(AccountResource::Profile, &[])
            .await?;
        let mut state = self.state.lock().unwrap();
        if state.generation != generation || reply.generation != generation {
            return Err(SessionError::Superseded);
        }
        let current = state.record.as_ref().ok_or(SessionError::Superseded)?;
        if reply.data.id <= 0
            || reply.data.id.to_string() != record.account_id
            || current.account_id != record.account_id
            || current.origin != record.origin
            || current.origin != self.client.origin()
        {
            state.recovery = SessionRecovery::Blocked;
            return Err(SessionError::InvalidResponse);
        }
        state.profile = Some(reply.data);
        state.cancel = None;
        state.recovery = SessionRecovery::Idle;
        Ok(Some(state.status()))
    }
    /// Coalesce probes and cache failures too; never poll or replay authorization.
    pub async fn remote_capability(&self) -> super::models::DesktopAuthCapability {
        let local = self.capability();
        if !matches!(local.status, super::models::DesktopAuthStatus::Available) {
            return local;
        }
        let mut cached = self.readiness.lock().await;
        if let Some((at, capability, _)) = cached.as_ref() {
            if at.elapsed() < std::time::Duration::from_secs(60) {
                return capability.clone();
            }
        }
        let mut invoice_support = false;
        let capability = match self.client.desktop_authorization_ready().await {
            Ok(support) if support.ready() => {
                invoice_support = support.invoices();
                super::models::DesktopAuthCapability {
                    status: super::models::DesktopAuthStatus::Available,
                    reason: "OSIR 账户浏览器授权已就绪".into(),
                }
            }
            Ok(_) => super::models::DesktopAuthCapability {
                status: super::models::DesktopAuthStatus::Unavailable,
                reason: "OSIR 网站尚未启用兼容的账户授权服务，请稍后重试。".into(),
            },
            Err(_) => super::models::DesktopAuthCapability {
                status: super::models::DesktopAuthStatus::Unavailable,
                reason: "无法确认 OSIR 账户授权服务已就绪，请检查网络或等待网站更新后重试。".into(),
            },
        };
        *cached = Some((
            std::time::Instant::now(),
            capability.clone(),
            invoice_support,
        ));
        capability
    }
    fn check(&self, generation: u64) -> Result<(), SessionError> {
        if self.state.lock().unwrap().generation != generation {
            return Err(SessionError::Superseded);
        }
        Ok(())
    }
    pub async fn start(&self) -> Result<LoginAttempt, SessionError> {
        self.start_with_key_creation(false).await
    }
    /// Explicit opt-in is allowed before login and applies only to this attempt.
    pub async fn start_with_key_creation(
        &self,
        create_keys: bool,
    ) -> Result<LoginAttempt, SessionError> {
        self.start_for_generation(create_keys, None).await
    }
    /// Reauthorization must still refer to the session the user confirmed when
    /// the attempt is reserved, including concurrent cancellation/start calls.
    pub async fn start_for_generation(
        &self,
        create_keys: bool,
        expected: Option<u64>,
    ) -> Result<LoginAttempt, SessionError> {
        self.require_enabled()?;
        let (sender, cancelled) = oneshot::channel();
        let generation = {
            let mut state = self.state.lock().unwrap();
            if expected.is_some_and(|expected| expected != state.generation) {
                return Err(SessionError::Superseded);
            }
            if state.record.is_some()
                || state.cleanup_account.is_some()
                || matches!(
                    state.recovery,
                    SessionRecovery::Pending
                        | SessionRecovery::Restoring
                        | SessionRecovery::UnlockRequired
                        | SessionRecovery::Blocked
                )
            {
                return Err(SessionError::AlreadyConnected);
            }
            state.invalidate();
            state.cancel = Some(sender);
            state.generation
        };
        let endpoint = format!(
            "{}/auth/desktop/authorize",
            self.config.website_origin.trim_end_matches('/')
        );
        let mut pending = PendingAuthorization::bind(&endpoint, CLIENT_ID)
            .await
            .map_err(|_| SessionError::Authorization)?;
        let invoices = self
            .readiness
            .lock()
            .await
            .as_ref()
            .is_some_and(|(at, _, supported)| {
                *supported && at.elapsed() < std::time::Duration::from_secs(60)
            });
        let requested_scope = self.config.login_scope_with_invoices(create_keys, invoices);
        pending
            .set_scope(&requested_scope)
            .map_err(|_| SessionError::Configuration)?;
        self.check(generation)?;
        Ok(LoginAttempt {
            pending,
            requested_scope,
            cancelled,
            generation,
        })
    }
    /// Invalidate first, even if exchange or Keychain is currently blocked.
    /// An older failed browser attempt must never cancel its replacement.
    pub fn cancel_if_generation(&self, generation: u64) {
        let mut state = self.state.lock().unwrap();
        if state.generation == generation && state.record.is_none() {
            state.invalidate();
            let _ = self.journal.write_slot(
                state.cleanup_account.as_deref(),
                state.cleanup_slot.as_deref(),
                false,
            );
        }
    }
    pub fn account_origin(&self) -> String {
        self.client.origin()
    }
    /// Model catalog requests must charge the same budget as account requests.
    pub fn request_budget(&self) -> super::request_budget::RequestBudget {
        self.client.request_budget()
    }
    pub fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        // A late UI cancellation must not invalidate an already committed login.
        if state.record.is_none() {
            state.invalidate();
            let _ = self.journal.write_slot(
                state.cleanup_account.as_deref(),
                state.cleanup_slot.as_deref(),
                false,
            );
        }
    }

    async fn save(
        &self,
        record: Arc<SecretRecord>,
        generation: u64,
        create: bool,
    ) -> Result<(), SessionError> {
        let credentials = self.credentials.clone();
        let state = self.state.clone();
        let gate = self.keychain_gate.clone();
        let journal = self.journal.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = gate.lock().unwrap();
            {
                let current = state.lock().unwrap();
                if current.generation != generation {
                    return Err(SessionError::Superseded);
                }
                // Fail closed across a crash or ambiguous Keychain write.
                journal
                    .write_slot(Some(&record.account_id), record.slot.as_deref(), false)
                    .map_err(|error| {
                        log::warn!(
                            "OSIR account save failed: stage=journal_prepare reason={error:?}"
                        );
                        error
                    })?;
            }
            if state.lock().unwrap().generation != generation {
                return Err(SessionError::Superseded);
            }
            let saved = if create {
                credentials.create_for_user(&record)
            } else {
                credentials.store(&record)
            };
            saved.map_err(|error| {
                log::warn!("OSIR account save failed: stage=credential_write reason={error:?}");
                error
            })?;
            // spawn_blocking continues even if its caller is cancelled. Clean up
            // its late write under the same gate used by logout deletion.
            let current = state.lock().unwrap();
            if current.generation != generation {
                drop(current);
                credentials.delete_slot(&record.account_id, record.slot.as_deref())?;
                return Err(SessionError::Superseded);
            }
            Ok(())
        })
        .await
        .map_err(|_| SessionError::Storage)?
    }
    async fn delete(&self, account: String, slot: Option<String>) -> Result<(), SessionError> {
        let credentials = self.credentials.clone();
        let gate = self.keychain_gate.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = gate.lock().unwrap();
            credentials.delete_slot(&account, slot.as_deref())
        })
        .await
        .map_err(|_| SessionError::Storage)??;
        Ok(())
    }
    async fn revoke(&self, record: &SecretRecord) -> bool {
        self.client
            .auth_post::<serde_json::Value, _>(
                AuthEndpoint::Logout,
                &serde_json::json!({"refresh_token": record.refresh_token}),
            )
            .await
            .is_ok()
    }
    pub async fn complete(&self, login: LoginGrant) -> Result<SessionStatus, SessionError> {
        self.require_enabled()?;
        let _operation = self.operation.lock().await;
        self.check(login.generation)?;
        // One-shot code: do not retry on timeout or failure.
        let pair: TokenPair = self
            .client
            .auth_post(
                AuthEndpoint::Exchange,
                &serde_json::json!({
                    "grant_type": "authorization_code", "client_id": CLIENT_ID,
                    "code": login.grant.code, "code_verifier": login.grant.verifier,
                    "redirect_uri": login.grant.redirect_uri,
                }),
            )
            .await?;
        let (record, profile) = self.resolve(pair, None, &login.requested_scope).await?;
        let record = Arc::new(record);
        if self.check(login.generation).is_err() {
            self.revoke(&record).await;
            return Err(SessionError::Superseded);
        }
        self.commit(record, profile, login.generation, true).await?;
        Ok(self.status())
    }
    async fn resolve(
        &self,
        pair: TokenPair,
        expected: Option<&str>,
        requested_scope: &str,
    ) -> Result<(SecretRecord, AccountProfile), SessionError> {
        pair.validate(requested_scope)?;
        let profile: AccountProfile = match self
            .client
            .get(AccountResource::Profile, &pair.access_token, &[])
            .await
        {
            Ok(profile) => profile,
            Err(error) => {
                let _ = self
                    .client
                    .auth_post::<serde_json::Value, _>(
                        AuthEndpoint::Logout,
                        &serde_json::json!({"refresh_token": pair.refresh_token}),
                    )
                    .await;
                return Err(error.into());
            }
        };
        if profile.id <= 0 || expected.is_some_and(|id| id != profile.id.to_string()) {
            let _ = self
                .client
                .auth_post::<serde_json::Value, _>(
                    AuthEndpoint::Logout,
                    &serde_json::json!({"refresh_token": pair.refresh_token}),
                )
                .await;
            return Err(SessionError::InvalidResponse);
        }
        Ok((
            SecretRecord {
                access_token: pair.access_token,
                refresh_token: pair.refresh_token,
                expires_at: chrono::Utc::now().timestamp() + pair.expires_in,
                account_id: profile.id.to_string(),
                origin: self.client.origin(),
                scope: pair.scope,
                slot: None,
            },
            profile,
        ))
    }
    async fn commit(
        &self,
        record: Arc<SecretRecord>,
        profile: AccountProfile,
        generation: u64,
        persist: bool,
    ) -> Result<(), SessionError> {
        if profile.id.to_string() != record.account_id || record.origin != self.client.origin() {
            self.revoke(&record).await;
            return Err(SessionError::InvalidResponse);
        }
        // A new browser grant gets a new item, even for the same account. Never
        // update/read a predecessor owned by an older signing identity.
        let record = if persist {
            Arc::new(SecretRecord {
                access_token: record.access_token.clone(),
                refresh_token: record.refresh_token.clone(),
                expires_at: record.expires_at,
                account_id: record.account_id.clone(),
                origin: record.origin.clone(),
                scope: record.scope.clone(),
                slot: Some(uuid::Uuid::new_v4().simple().to_string()),
            })
        } else {
            record
        };
        {
            let mut state = self.state.lock().unwrap();
            if state.generation != generation {
                return Err(SessionError::Superseded);
            }
            state.cleanup_account = Some(record.account_id.clone());
            state.cleanup_slot = record.slot.clone();
        }
        let mut cancellation = InvalidationGuard {
            state: &self.state,
            journal: &self.journal,
            generation,
            armed: true,
        };
        // Startup reuses its validated saved record; refresh already persisted
        // its successor. Rewriting either would needlessly touch Keychain ACLs.
        if persist {
            if let Err(error) = self.save(record.clone(), generation, true).await {
                self.revoke(&record).await;
                return Err(error);
            }
        }
        let published = {
            let mut state = self.state.lock().unwrap();
            if state.generation == generation {
                // Activation and logout serialize under state; only after durable
                // credentials exist. A failed journal write cannot publish login.
                self.journal
                    .write_slot(Some(&record.account_id), record.slot.as_deref(), true)
                    .map_err(|error| {
                        log::warn!(
                            "OSIR account save failed: stage=journal_activate reason={error:?}"
                        );
                        error
                    })?;
                state.cleanup_account = Some(record.account_id.clone());
                state.cleanup_slot = record.slot.clone();
                state.record = Some(record.clone());
                state.profile = Some(profile);
                state.cancel = None;
                state.recovery = SessionRecovery::Idle;
                true
            } else {
                false
            }
        };
        if !published {
            let _ = self
                .delete(record.account_id.clone(), record.slot.clone())
                .await;
            self.revoke(&record).await;
            return Err(SessionError::Superseded);
        }
        cancellation.armed = false;
        Ok(())
    }
    /// Called once in a background startup task. Never enumerates Keychain.
    /// Only a trusted active reference can select an account; tombstones win.
    pub async fn restore_startup(&self) -> Result<SessionStatus, SessionError> {
        self.restore_saved(false).await
    }
    async fn restore_saved(&self, interactive: bool) -> Result<SessionStatus, SessionError> {
        self.require_enabled()?;
        let _recovery = match self.recovery.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                let _finished = self.recovery.lock().await;
                self.check(0)?;
                return Ok(self.status());
            }
        };
        {
            let mut state = self.state.lock().unwrap();
            if state.generation != 0 {
                return Err(SessionError::Superseded);
            }
            if state.recovery != SessionRecovery::Pending
                && !(interactive && state.recovery == SessionRecovery::UnlockRequired)
            {
                return Ok(state.status());
            }
            state.recovery = SessionRecovery::Restoring;
        }
        let completion = RecoveryGuard {
            state: &self.state,
            generation: 0,
        };
        let result = self.restore_startup_once(interactive).await;
        if result.is_ok() {
            let mut state = self.state.lock().unwrap();
            if state.generation == 0 && state.recovery == SessionRecovery::Restoring {
                state.recovery = SessionRecovery::Idle;
            }
        }
        drop(completion);
        result?;
        Ok(self.status())
    }
    async fn restore_startup_once(&self, interactive: bool) -> Result<SessionStatus, SessionError> {
        let _operation = self.operation.lock().await;
        self.check(0)?;
        let journal = self.journal.clone();
        let reference = tokio::task::spawn_blocking(move || journal.read())
            .await
            .map_err(|_| SessionError::Storage)??;
        self.check(0)?;
        let Some(reference) = reference else {
            return Ok(self.status());
        };
        if !reference.active {
            return Ok(self.status());
        }
        let account = reference.account.ok_or(SessionError::InvalidResponse)?;
        let slot = reference.slot;
        {
            let mut state = self.state.lock().unwrap();
            if state.generation != 0 || state.record.is_some() || state.cancel.is_some() {
                return Err(SessionError::Superseded);
            }
            let (sender, _receiver) = oneshot::channel();
            state.cancel = Some(sender);
            state.cleanup_account = Some(account.clone());
            state.cleanup_slot = slot.clone();
        }
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut read_cancellation = CredentialReadGuard {
            state: &self.state,
            generation: 0,
            cancelled: cancelled.clone(),
            armed: true,
        };
        let credentials = self.credentials.clone();
        let gate = self.keychain_gate.clone();
        let state = self.state.clone();
        let record = tokio::task::spawn_blocking(move || {
            // A timed-out spawn_blocking read keeps running in macOS. Do not
            // queue another read (or a prompt) behind that same blocked call.
            let _gate = gate.try_lock().map_err(|_| CredentialError::Storage)?;
            if cancelled.load(std::sync::atomic::Ordering::Acquire)
                || state.lock().unwrap().generation != 0
            {
                return Err(CredentialError::Storage);
            }
            if interactive {
                credentials.load_for_user(&account, slot.as_deref())
            } else {
                credentials.load_for_startup(&account, slot.as_deref())
            }
        })
        .await;
        self.check(0)?;
        let record = match record {
            Ok(Err(
                error @ (CredentialError::InteractionRequired
                | CredentialError::Unavailable
                | CredentialError::Storage),
            )) => {
                let reason = match error {
                    CredentialError::InteractionRequired => "interaction_required",
                    CredentialError::Unavailable => "store_unavailable",
                    _ => "store_failure",
                };
                log::warn!(
                    "OSIR account saved-login read deferred: reason={reason}, interactive={interactive}"
                );
                drop(read_cancellation);
                self.check(0)?;
                return Ok(self.status());
            }
            Err(_) => {
                log::warn!("OSIR account saved-login read deferred: worker_failure");
                drop(read_cancellation);
                self.check(0)?;
                return Ok(self.status());
            }
            Ok(record) => record,
        };
        // Missing, malformed or mismatched credentials are authoritative. Only
        // this terminal path (and later refresh/write paths) may tombstone.
        let mut cancellation = InvalidationGuard {
            state: &self.state,
            journal: &self.journal,
            generation: 0,
            armed: true,
        };
        read_cancellation.armed = false;
        let record = record?.ok_or(SessionError::Unauthorized)?;
        validate_scope(&record.scope)?;
        // Previously consented write grants remain valid with read-only login defaults.
        if record.origin != self.client.origin() {
            return Err(SessionError::InvalidResponse);
        }
        let record = Arc::new(record);
        // From here temporary reads/refresh admission failures retain the
        // validated native grant, including during startup restoration.
        self.preserve_refresh_session(record.clone(), 0)?;
        cancellation.armed = false;
        if record.expires_at <= chrono::Utc::now().timestamp() + 60 {
            self.rotate(record, 0).await?;
        } else {
            match self
                .client
                .get(AccountResource::Profile, &record.access_token, &[])
                .await
            {
                Ok(profile) => self.commit(record, profile, 0, false).await?,
                Err(AccountError::Unauthorized) => {
                    self.rotate(record, 0).await?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.check(0)?;
        cancellation.armed = false;
        Ok(self.status())
    }
    /// Compatibility native entry; the supplied id cannot bypass the journal.
    pub async fn restore(&self, account: &str) -> Result<SessionStatus, SessionError> {
        self.require_enabled()?;
        let journal = self.journal.clone();
        let reference = tokio::task::spawn_blocking(move || journal.read())
            .await
            .map_err(|_| SessionError::Storage)??;
        if !reference.is_some_and(|r| r.active && r.account.as_deref() == Some(account)) {
            return Err(SessionError::Unauthorized);
        }
        self.restore_startup().await
    }
    async fn access(&self) -> Result<(u64, Arc<SecretRecord>), SessionError> {
        self.access_after_rejection(None).await
    }
    // Credential identity matters: a parallel request may have refreshed within
    // the same auth generation already. Followers reuse its committed pair.
    async fn access_after_rejection(
        &self,
        rejected: Option<(u64, Arc<SecretRecord>)>,
    ) -> Result<(u64, Arc<SecretRecord>), SessionError> {
        self.require_enabled()?;
        let generation = rejected
            .as_ref()
            .map_or_else(|| self.status().generation, |(g, _)| *g);
        let _operation = self.operation.lock().await;
        self.check(generation)?;
        let record = self
            .state
            .lock()
            .unwrap()
            .record
            .clone()
            .ok_or(SessionError::Unauthorized)?;
        let force_refresh = rejected
            .as_ref()
            .is_some_and(|(_, previous)| Arc::ptr_eq(previous, &record));
        if !force_refresh && record.expires_at > chrono::Utc::now().timestamp() + 60 {
            return Ok((generation, record));
        }
        if self.state.lock().unwrap().refresh_blocked {
            return Err(SessionError::Unavailable);
        }
        self.rotate(record, generation).await
    }
    // Caller holds operation lock; no refresh token can be consumed twice.
    async fn rotate(
        &self,
        record: Arc<SecretRecord>,
        generation: u64,
    ) -> Result<(u64, Arc<SecretRecord>), SessionError> {
        {
            let state = self.state.lock().unwrap();
            if state.generation != generation {
                return Err(SessionError::Superseded);
            }
            // Persist BEFORE consuming a rotating token. Crash/cancel cannot
            // restore a predecessor whose refresh outcome is unknown.
            self.journal
                .write_slot(Some(&record.account_id), record.slot.as_deref(), false)?;
        }
        // Followers re-read state under the lock and use the winner's rotated pair.
        // Cancellation still tombstones an in-flight rotation; completed temporary
        // errors preserve credentials rather than treating admission as revocation.
        // If the request future is dropped after the server consumes refresh,
        // invalidate the generation so no follower can replay its predecessor.
        let mut cancellation = InvalidationGuard {
            state: &self.state,
            journal: &self.journal,
            generation,
            armed: true,
        };
        let result = async {
            let response = self
                .client
                .auth_post::<TokenPair, _>(
                    AuthEndpoint::Refresh,
                    &serde_json::json!({"refresh_token": record.refresh_token}),
                )
                .await;
            let pair = match response {
                Err(AccountError::AuthOutcomeUnknown) => {
                    self.block_refresh(generation)?;
                    return Err(SessionError::Unavailable);
                }
                other => other?,
            };
            // A successful response consumed the predecessor, even if its
            // payload or subsequent storage fails. Never replay that predecessor.
            self.block_refresh(generation)?;
            pair.validate(&record.scope)?;
            let next = Arc::new(SecretRecord {
                access_token: pair.access_token,
                refresh_token: pair.refresh_token,
                expires_at: chrono::Utc::now().timestamp() + pair.expires_in,
                account_id: record.account_id.clone(),
                origin: record.origin.clone(),
                scope: pair.scope,
                slot: record.slot.clone(),
            });
            if next.refresh_token == record.refresh_token {
                return Err(SessionError::InvalidResponse);
            }
            // Persist the successor before any fallible profile read. A profile
            // cooldown must not discard a successfully rotated refresh token.
            self.save(next.clone(), generation, false).await?;
            {
                let mut state = self.state.lock().unwrap();
                if state.generation != generation {
                    return Err(SessionError::Superseded);
                }
                self.journal
                    .write_slot(Some(&next.account_id), next.slot.as_deref(), true)?;
                state.record = Some(next.clone());
                state.refresh_blocked = false;
            }
            let profile: AccountProfile = self
                .client
                .get(AccountResource::Profile, &next.access_token, &[])
                .await?;
            if profile.id.to_string() != next.account_id {
                return Err(SessionError::Unauthorized);
            }
            self.commit(next.clone(), profile, generation, false)
                .await?;
            Ok::<_, SessionError>((generation, next))
        }
        .await;
        if matches!(&result, Err(SessionError::Unauthorized)) {
            {
                let mut state = self.state.lock().unwrap();
                if state.generation == generation {
                    state.invalidate();
                    state.record = None;
                    state.profile = None;
                }
            }
            let _ = self
                .delete(record.account_id.clone(), record.slot.clone())
                .await;
        } else if result.is_err() {
            if !self.state.lock().unwrap().refresh_blocked {
                self.preserve_refresh_session(record, generation)?;
            }
        }
        cancellation.armed = false;
        result
    }
    /// Only confirmed invalidation clears a session. This also restores the
    /// active journal after a local admission refusal before HTTP was sent.
    fn preserve_refresh_session(
        &self,
        previous: Arc<SecretRecord>,
        generation: u64,
    ) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap();
        if state.generation != generation {
            return Err(SessionError::Superseded);
        }
        let current = state.record.get_or_insert(previous);
        self.journal
            .write_slot(Some(&current.account_id), current.slot.as_deref(), true)?;
        Ok(())
    }
    fn block_refresh(&self, generation: u64) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap();
        if state.generation != generation {
            return Err(SessionError::Superseded);
        }
        state.refresh_blocked = true;
        Ok(())
    }
    /// GET only: one refresh/replay at most. Never replay token POSTs.
    /// Commands must project T into a safe explicit DTO.
    pub async fn read_account<T: DeserializeOwned>(
        &self,
        resource: AccountResource,
        query: &[(String, String)],
    ) -> Result<SessionRead<T>, SessionError> {
        let (generation, record) = self.access().await?;
        let mut result = self.client.get(resource, &record.access_token, query).await;
        self.check(generation)?;
        if matches!(&result, Err(AccountError::Unauthorized)) {
            let (_, next) = self
                .access_after_rejection(Some((generation, record)))
                .await?;
            result = self.client.get(resource, &next.access_token, query).await;
            self.check(generation)?;
            if matches!(&result, Err(AccountError::Unauthorized)) {
                // A late 401 cannot delete a newer rotated pair or account.
                let _operation = self.operation.lock().await;
                let invalidate = {
                    let mut state = self.state.lock().unwrap();
                    if state.generation == generation
                        && state.record.as_ref().is_some_and(|r| Arc::ptr_eq(r, &next))
                    {
                        state.invalidate();
                        state.record = None;
                        state.profile = None;
                        true
                    } else {
                        false
                    }
                };
                if invalidate {
                    let _ = self
                        .delete(next.account_id.clone(), next.slot.clone())
                        .await;
                }
            }
        }
        let data = result?;
        self.check(generation)?;
        Ok(SessionRead { generation, data })
    }
    /// Optional reporting never refreshes tokens, reads Keychain, or changes
    /// login state. If the in-memory access token is stale, skip this report.
    pub(crate) async fn record_client_activity(
        &self,
        metadata: &super::client::ClientActivityMetadata<'_>,
    ) -> Result<(), SessionError> {
        self.require_enabled()?;
        let (generation, record) = {
            let state = self.state.lock().map_err(|_| SessionError::Unavailable)?;
            (
                state.generation,
                state.record.clone().ok_or(SessionError::Unauthorized)?,
            )
        };
        if record.expires_at <= chrono::Utc::now().timestamp() + 10 {
            return Err(SessionError::Unauthorized);
        }
        self.check(generation)?;
        let result = self
            .client
            .record_client_activity(metadata, &record.access_token)
            .await;
        self.check(generation)?;
        result.map_err(Into::into)
    }
    /// Native-only WireKey; never return it through a Tauri command.
    pub async fn read_key(
        &self,
        id: i64,
        expected: u64,
    ) -> Result<SessionRead<super::account::WireKey>, SessionError> {
        if id <= 0 {
            return Err(SessionError::InvalidResponse);
        }
        self.check(expected)?;
        let (generation, record) = self.access().await?;
        if generation != expected {
            return Err(SessionError::Superseded);
        }
        let mut result = self.client.get_key(id, &record.access_token).await;
        self.check(generation)?;
        if matches!(&result, Err(AccountError::Unauthorized)) {
            let (_, next) = self
                .access_after_rejection(Some((generation, record)))
                .await?;
            result = self.client.get_key(id, &next.access_token).await;
        }
        self.check(generation)?;
        let data: super::account::WireKey = result?;
        if data.id != id {
            return Err(SessionError::InvalidResponse);
        }
        Ok(SessionRead { generation, data })
    }
    /// Native-only raw key result; caller projects metadata or applies through
    /// the configuration service. Never serialize/cache the returned WireKey.
    /// keys:write is required in the actual grant, never implicitly requested.
    pub async fn create_key(
        &self,
        request: super::client::CreateKeyRequest,
        expected: u64,
    ) -> Result<SessionRead<super::account::WireKey>, CreateSessionKeyError> {
        request.validate().map_err(|_| {
            CreateSessionKeyError::Request(super::client::CreateKeyError::InvalidInput)
        })?;
        self.check(expected)
            .map_err(CreateSessionKeyError::Session)?;
        let (generation, record) = self
            .access()
            .await
            .map_err(CreateSessionKeyError::Session)?;
        if generation != expected {
            return Err(CreateSessionKeyError::Session(SessionError::Superseded));
        }
        if !record.scope.split(' ').any(|s| s == "keys:write") {
            return Err(CreateSessionKeyError::WriteScopeRequired);
        }
        let result = self
            .client
            .create_key::<super::account::WireKey>(&request, &record.access_token)
            .await;
        // Once POST was sent, a generation change must not look safely retryable.
        self.check(expected).map_err(|_| {
            CreateSessionKeyError::Request(super::client::CreateKeyError::Uncertain)
        })?;
        let data = result.map_err(CreateSessionKeyError::Request)?;
        if data.id <= 0 || data.group_id != request.group_id {
            return Err(CreateSessionKeyError::Request(
                super::client::CreateKeyError::Uncertain,
            ));
        }
        Ok(SessionRead { generation, data })
    }
    /// POST is never retried: an ambiguous result may already have committed.
    pub async fn mark_announcement_read(
        &self,
        id: i64,
    ) -> Result<SessionRead<serde_json::Value>, SessionError> {
        self.mark_announcement_read_for_generation(id, self.status().generation)
            .await
    }
    pub async fn mark_announcement_read_for_generation(
        &self,
        id: i64,
        expected: u64,
    ) -> Result<SessionRead<serde_json::Value>, SessionError> {
        if id <= 0 {
            return Err(SessionError::InvalidResponse);
        }
        self.check(expected)?;
        let (generation, record) = self.access().await?;
        if generation != expected {
            return Err(SessionError::Superseded);
        }
        let result = self
            .client
            .mark_announcement_read(id, &record.access_token)
            .await;
        self.check(generation)?;
        Ok(SessionRead {
            generation,
            data: result?,
        })
    }
    /// Synchronous invalidation so commands can clear caches and emit immediately.
    pub fn begin_logout(&self) -> LogoutTicket {
        let mut state = self.state.lock().unwrap();
        state.invalidate();
        let record = state.record.take();
        state.profile = None;
        let account = state.cleanup_account.clone();
        let slot = state.cleanup_slot.clone();
        let tombstoned = self
            .journal
            .write_slot(account.as_deref(), slot.as_deref(), false)
            .is_ok();
        LogoutTicket {
            generation: state.generation,
            record,
            account,
            slot,
            tombstoned,
        }
    }
    pub async fn logout(&self) -> LogoutReport {
        let ticket = self.begin_logout();
        self.finish_logout(ticket).await
    }
    pub async fn finish_logout(&self, ticket: LogoutTicket) -> LogoutReport {
        let LogoutTicket {
            generation,
            record,
            account,
            slot,
            tombstoned,
        } = ticket;
        let _operation = self.operation.lock().await;
        if self.status().generation != generation {
            return LogoutReport {
                generation,
                local_cleared: false,
                remote_revoked: false,
            };
        }
        // Retry a failed durable write before reporting local completion.
        let tombstoned = tombstoned
            || self
                .journal
                .write_slot(account.as_deref(), slot.as_deref(), false)
                .is_ok();
        let deleted = match account {
            Some(account) => self.delete(account, slot).await.is_ok(),
            None => true,
        };
        let local_cleared = tombstoned && deleted;
        let remote_revoked = match record {
            Some(record) => self.revoke(&record).await,
            None => false,
        };
        if local_cleared {
            let mut state = self.state.lock().unwrap();
            if state.generation == generation {
                state.cleanup_account = None;
                state.cleanup_slot = None;
            }
        }
        LogoutReport {
            generation,
            local_cleared,
            remote_revoked,
        }
    }
}

#[derive(Deserialize)]
struct TokenPair {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    token_type: String,
    scope: String,
}
impl TokenPair {
    fn validate(&self, requested: &str) -> Result<(), SessionError> {
        validate_scope(requested)?;
        validate_scope(&self.scope)?;
        if !self.token_type.eq_ignore_ascii_case("Bearer")
            // The server caps access TTL at the desktop session's remaining
            // lifetime. A rotated successor can legitimately have <61 seconds.
            || !(1..=86400).contains(&self.expires_in)
            || self
                .scope
                .split(' ')
                .any(|s| !requested.split(' ').any(|r| r == s))
            || [&self.access_token, &self.refresh_token].iter().any(|s| {
                s.is_empty() || s.len() > 16384 || !s.bytes().all(|b| b.is_ascii_graphic())
            })
        {
            return Err(SessionError::InvalidResponse);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn optional_account_activity_never_refreshes_or_reads_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Memory::default();
        let manager = persistent_manager(directory.path(), backend.clone());
        let metadata = super::super::client::ClientActivityMetadata {
            version: "1.2.3",
            channel: "prod",
            platform: "darwin-aarch64",
        };
        assert!(matches!(
            manager.record_client_activity(&metadata).await,
            Err(SessionError::Unauthorized)
        ));
        let mut stale = record();
        stale.expires_at = chrono::Utc::now().timestamp() - 1;
        manager.state.lock().unwrap().record = Some(Arc::new(stale));
        let before = manager.status().generation;
        assert!(matches!(
            manager.record_client_activity(&metadata).await,
            Err(SessionError::Unauthorized)
        ));
        assert_eq!(manager.status().generation, before);
        assert!(manager.state.lock().unwrap().record.is_some());
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(backend.items.lock().unwrap().is_empty());
        assert_eq!(backend.writes.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            backend
                .interactive_reads
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            backend
                .interactive_writes
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(manager.journal.read().unwrap().is_none());
    }

    // Real HTTPS transport, temporary certificate, memory-only credentials.
    // Local python3/openssl only; no downloads or production network.
    #[tokio::test]
    async fn local_https_exchange_profile_refresh_logout_contract() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let directory = tempfile::tempdir().unwrap();
        let cert = directory.path().join("cert.pem");
        let key = directory.path().join("key.pem");
        let config = directory.path().join("openssl.cnf");
        std::fs::write(
            &config,
            "[req]
distinguished_name=dn
x509_extensions=ext
prompt=no
[dn]
CN=127.0.0.1
[ext]
subjectAltName=IP:127.0.0.1
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
",
        )
        .unwrap();
        assert!(Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config"])
            .arg(&config)
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
        let script = r#"
import http.server, json, ssl, sys
profile = {'id':7,'email':'fixture@example.invalid','username':'fixture','balance':0}
def pair(suffix):
    return {'access_token':'fixture-access-'+suffix,'refresh_token':'drt_'+('a' if suffix=='first' else 'b')*43,
            'expires_in':600,'token_type':'Bearer','scope':'account:read keys:read'}
class Handler(http.server.BaseHTTPRequestHandler):
    index = 0
    failed = False
    def log_message(self, *args): pass
    def do_GET(self): self.respond()
    def do_POST(self): self.respond()
    def respond(self):
        try:
            i = Handler.index
            Handler.index += 1
            body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or '{}')
            assert (self.command,self.path) == [
              ('POST','/api/v1/auth/desktop/token'),('GET','/api/v1/user/profile'),
              ('POST','/api/v1/auth/refresh'),('GET','/api/v1/user/profile'),
              ('GET','/api/v1/user/profile'),('POST','/api/v1/auth/logout'),
              ('GET','/api/v1/user/profile'),('GET','/api/v1/user/profile'),
              ('POST','/api/v1/auth/logout'),('GET','/api/v1/user/profile'),
              ('GET','/api/v1/user/profile')][i]
            if i == 0:
                assert body == {'grant_type':'authorization_code','client_id':'osir-desktop',
                  'code':'c'*43,'code_verifier':'v'*43,
                  'redirect_uri':'http://127.0.0.1:12345/osir/desktop/callback'}
            elif i in (1,3,4):
                assert self.headers.get('Authorization') == 'Bearer fixture-access-'+('first' if i==1 else 'next')
                assert body == {}
            elif i in (6,7):
                assert self.headers.get('Authorization') == 'Bearer fixture-access-startup'
                assert body == {}
            elif i in (9,10):
                assert self.headers.get('Authorization') == 'Bearer fixture-access-cold'
                assert body == {}
            elif i == 8:
                assert body == {'refresh_token':'drt_'+'s'*43}
            else:
                assert body == {'refresh_token':'drt_'+('a' if i==2 else 'b')*43}
            if i in (0,2,5,8): assert self.headers.get('Authorization') is None
            data = pair('first') if i==0 else pair('next') if i==2 else {'message':'Logged out successfully'} if i==5 else profile
            encoded = json.dumps({'code':0,'data':data}).encode()
            self.send_response(503 if i==6 else 200)
            self.send_header('Content-Type','application/json')
            self.send_header('Cache-Control','no-store')
            self.send_header('Content-Length',str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)
        except Exception:
            Handler.failed = True
            raise
server = http.server.HTTPServer(('127.0.0.1',0),Handler)
server.timeout = 5
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(sys.argv[1],sys.argv[2])
server.socket = context.wrap_socket(server.socket,server_side=True)
print(server.server_port,flush=True)
for _ in range(11): server.handle_request()
assert Handler.index == 11 and not Handler.failed
"#;
        struct Server(std::process::Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut server = Server(
            Command::new("python3")
                .arg("-u")
                .arg("-c")
                .arg(script)
                .arg(&cert)
                .arg(&key)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut port = String::new();
        BufReader::new(server.0.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        let port: u16 = port.trim().parse().unwrap();
        let mut manager = manager(false);
        manager.config.enabled = true;
        manager.client = super::super::client::tests::local_tls_client(
            &format!("https://127.0.0.1:{port}/api/v1/"),
            &std::fs::read(&cert).unwrap(),
        );
        let backend = Memory::default();
        manager.credentials = Arc::new(
            CredentialStore::with_backend(
                &manager.client.origin(),
                "test.osir.dev",
                backend.clone(),
            )
            .unwrap(),
        );
        manager.journal = Arc::new(SessionJournal::memory(manager.client.origin()));
        let status = manager
            .complete(LoginGrant {
                generation: 0,
                requested_scope: "account:read keys:read".into(),
                grant: AuthorizationGrant {
                    code: "c".repeat(43),
                    verifier: "v".repeat(43),
                    redirect_uri: "http://127.0.0.1:12345/osir/desktop/callback".into(),
                },
            })
            .await
            .unwrap();
        assert!(status.connected);
        assert!(!status.can_create_keys);
        assert_eq!(status.profile.unwrap().id, 7);
        assert_eq!(backend.writes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(backend.creates.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            backend
                .interactive_writes
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        let previous = manager.state.lock().unwrap().record.clone().unwrap();
        let previous_slot = previous.slot.clone();
        let (_, next) = manager
            .access_after_rejection(Some((0, previous)))
            .await
            .unwrap();
        assert_eq!(next.refresh_token, format!("drt_{}", "b".repeat(43)));
        assert!(previous_slot.is_some());
        assert_eq!(next.slot, previous_slot);
        assert_eq!(manager.journal.read().unwrap().unwrap().slot, next.slot);
        // Rotation saves its successor once and never opens a system dialog.
        assert_eq!(backend.writes.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            backend
                .interactive_writes
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(Arc::ptr_eq(&manager.access().await.unwrap().1, &next));
        assert_eq!(
            manager
                .credentials
                .load_slot_for_refresh("7", next.slot.as_deref())
                .unwrap()
                .unwrap()
                .refresh_token,
            next.refresh_token
        );
        manager.state.lock().unwrap().profile = None;
        let restored = manager.recover_existing().await.unwrap().unwrap();
        assert!(restored.connected);
        assert_eq!(restored.profile.unwrap().id, 7);
        assert!(Arc::ptr_eq(
            manager.state.lock().unwrap().record.as_ref().unwrap(),
            &next
        ));
        assert_eq!(
            manager
                .credentials
                .load_slot_for_refresh("7", next.slot.as_deref())
                .unwrap()
                .unwrap()
                .refresh_token,
            next.refresh_token
        );
        // A second click with a loaded profile does not issue another request.
        assert!(manager
            .recover_existing()
            .await
            .unwrap()
            .unwrap()
            .profile
            .is_some());
        let report = manager.logout().await;
        assert!(report.local_cleared && report.remote_revoked);
        assert!(!manager.status().connected);
        assert!(backend.items.lock().unwrap().is_empty());
        assert!(!manager.journal.read().unwrap().unwrap().active);
        // Cold startup: credentials survive a temporary profile failure, but
        // neither login success nor key-write capability is published yet.
        let mut saved = record();
        saved.origin = manager.client.origin();
        saved.access_token = "fixture-access-startup".into();
        saved.refresh_token = format!("drt_{}", "s".repeat(43));
        manager.credentials.store(&saved).unwrap();
        manager.journal.write(Some("7"), true).unwrap();
        let blocked = Arc::new(BlockedRead::default());
        let _release = ReleaseRead(blocked.clone());
        let mut restart_backend = backend.clone();
        restart_backend.blocked_read = Some(blocked.clone());
        let mut restarted = AuthManager::with_store(
            manager.config.clone(),
            CredentialStore::with_backend(&saved.origin, "test.osir.dev", restart_backend).unwrap(),
        )
        .unwrap();
        restarted.client = super::super::client::tests::local_tls_client(
            &format!("https://127.0.0.1:{port}/api/v1/"),
            &std::fs::read(&cert).unwrap(),
        );
        restarted.journal = manager.journal.clone();
        assert_eq!(restarted.status().recovery, SessionRecovery::Pending);
        assert!(matches!(
            restarted.start().await,
            Err(SessionError::AlreadyConnected)
        ));
        // A timed-out startup Keychain read must allow explicit recovery all
        // the way through the existing HTTPS profile/retry/commit contract.
        let mut restore = Box::pin(restarted.restore_startup());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::select! {
                _ = blocked.started.notified() => {},
                _ = &mut restore => panic!("read completed before release"),
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), restore)
                .await
                .is_err()
        );
        let status = restarted.status();
        assert_eq!(status.generation, 0);
        assert_eq!(
            backend
                .interactive_reads
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(status.recovery, SessionRecovery::UnlockRequired);
        assert!(restarted.journal.read().unwrap().unwrap().active);
        blocked.release();
        let gate = restarted.keychain_gate.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                let _gate = gate.lock().unwrap();
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            restarted.recover_existing_for_generation(0).await,
            Err(SessionError::Unavailable)
        ));
        assert!(!restarted.status().connected);
        assert!(!restarted.status().can_create_keys);
        assert_eq!(restarted.status().recovery, SessionRecovery::Retryable);
        let dto = serde_json::to_string(&restarted.status()).unwrap();
        assert!(!dto.contains("fixture-access") && !dto.contains("drt_") && !dto.contains("scope"));
        assert!(restarted.journal.read().unwrap().unwrap().active);
        let saved_reads = backend.reads.load(std::sync::atomic::Ordering::SeqCst);
        // A repeated startup entry is observation only, including after failure.
        assert_eq!(
            restarted.restore_startup().await.unwrap().recovery,
            SessionRecovery::Retryable
        );
        assert_eq!(
            backend.reads.load(std::sync::atomic::Ordering::SeqCst),
            saved_reads
        );
        // No automatic retry bypasses the request budget's 503 cooldown.
        assert!(restarted.recover_existing_for_generation(0).await.is_err());
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let (first, follower) = tokio::join!(
            restarted.recover_existing_for_generation(0),
            restarted.recover_existing_for_generation(0),
        );
        for status in [first.unwrap().unwrap(), follower.unwrap().unwrap()] {
            assert!(status.connected);
            assert_eq!(status.recovery, SessionRecovery::Idle);
            assert_eq!(status.profile.unwrap().id, 7);
        }
        assert_eq!(
            restarted
                .credentials
                .load_for_refresh("7")
                .unwrap()
                .unwrap()
                .refresh_token,
            saved.refresh_token
        );
        // Both startup and explicit recovery reuse saved credentials unchanged.
        assert_eq!(backend.writes.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(
            backend
                .interactive_reads
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0
        );
        assert!(restarted.logout().await.local_cleared);
        assert!(matches!(
            restarted.recover_existing_for_generation(0).await,
            Err(SessionError::Superseded)
        ));
        // An already authorized, readable credential and active journal must
        // enter the workbench on each fresh process without a recovery click.
        // Memory-only storage and local HTTPS exercise the full startup path;
        // they do not claim a real macOS ACL or distribution build was tested.
        let mut cold_record = record();
        cold_record.origin = manager.client.origin();
        cold_record.access_token = "fixture-access-cold".into();
        cold_record.slot = Some("e".repeat(32));
        manager.credentials.store(&cold_record).unwrap();
        let cold_journal = Arc::new(SessionJournal::persistent(
            manager.client.origin(),
            directory.path(),
        ));
        cold_journal
            .write_slot(Some("7"), cold_record.slot.as_deref(), true)
            .unwrap();
        let writes_before = backend.writes.load(std::sync::atomic::Ordering::SeqCst);
        let interactive_before = backend
            .interactive_reads
            .load(std::sync::atomic::Ordering::SeqCst);
        for _ in 0..2 {
            let mut cold = AuthManager::with_store(
                manager.config.clone(),
                CredentialStore::with_backend(
                    &manager.client.origin(),
                    "test.osir.dev",
                    backend.clone(),
                )
                .unwrap(),
            )
            .unwrap();
            cold.client = super::super::client::tests::local_tls_client(
                &format!("https://127.0.0.1:{port}/api/v1/"),
                &std::fs::read(&cert).unwrap(),
            );
            cold.journal = cold_journal.clone();
            let status = cold.restore_startup().await.unwrap();
            assert!(status.connected);
            assert_eq!(status.recovery, SessionRecovery::Idle);
            assert_eq!(status.profile.unwrap().id, 7);
            // Re-entering the startup hook cannot repeat the read or GET.
            let reads = backend.reads.load(std::sync::atomic::Ordering::SeqCst);
            assert!(cold.restore_startup().await.unwrap().connected);
            assert_eq!(
                backend.reads.load(std::sync::atomic::Ordering::SeqCst),
                reads
            );
        }
        assert_eq!(
            backend.writes.load(std::sync::atomic::Ordering::SeqCst),
            writes_before
        );
        assert_eq!(
            backend
                .interactive_reads
                .load(std::sync::atomic::Ordering::SeqCst),
            interactive_before
        );
        assert!(cold_journal.read().unwrap().unwrap().active);
        assert!(server.0.wait().unwrap().success());
    }

    #[test]
    fn image_mutation_authorization_holds_session_lock_and_rejects_stale_identity() {
        let mut manager = manager(false);
        manager.config.enabled = true;
        {
            let mut state = manager.state.lock().unwrap();
            state.record = Some(Arc::new(record()));
            state.profile = Some(profile());
        }
        let origin = manager.client.origin();
        manager
            .with_current_image_account(0, "7", &origin, || {
                assert!(manager.state.try_lock().is_err());
            })
            .unwrap();
        for (generation, account, origin) in [
            (1, "7", origin.as_str()),
            (0, "8", origin.as_str()),
            (0, "7", "https://wrong.invalid"),
        ] {
            assert!(manager
                .with_current_image_account(generation, account, origin, || panic!(
                    "unauthorized mutation"
                ))
                .is_err());
        }
        let _ticket = manager.begin_logout();
        assert!(manager
            .with_current_image_account(0, "7", &origin, || panic!("logged-out mutation"))
            .is_err());
    }

    #[tokio::test]
    async fn uncertain_refresh_preserves_credentials_without_replay() {
        let separator = String::from_utf8(vec![13, 10]).unwrap();
        let response = [
            "HTTP/1.1 200 OK",
            "Content-Length: 2",
            "Connection: close",
            "",
            "{}",
        ]
        .join(&separator);
        let (client, worker) =
            super::super::client::tests::response_fixture(Box::leak(response.into_boxed_str()));
        let mut manager = manager(false);
        manager.config.enabled = true;
        manager.client = client;
        let mut expired = record();
        expired.expires_at = 1;
        let previous = Arc::new(expired);
        manager.state.lock().unwrap().record = Some(previous.clone());
        assert!(manager.access().await.is_err());
        worker.join().unwrap();
        assert!(!manager.status().connected);
        assert_eq!(manager.status().recovery, SessionRecovery::Blocked);
        assert!(manager.state.lock().unwrap().refresh_blocked);
        assert!(!manager.journal.read().unwrap().unwrap().active);
        assert!(Arc::ptr_eq(
            manager.state.lock().unwrap().record.as_ref().unwrap(),
            &previous
        ));
        assert!(manager.access().await.is_err());
        assert!(manager.recover_existing_for_generation(0).await.is_err());
        assert_eq!(manager.status().generation, 0);
    }

    #[tokio::test]
    async fn shared_budget_busy_and_cooldown_do_not_logout_or_consume_refresh() {
        use super::super::request_budget::RequestClass;
        let mut manager = manager(false);
        manager.config.enabled = true;
        let mut expired = record();
        expired.expires_at = 1;
        let previous = Arc::new(expired);
        manager.state.lock().unwrap().record = Some(previous.clone());
        manager.state.lock().unwrap().profile = Some(profile());
        let budget = manager.request_budget();
        let guards: Vec<_> = (0..4)
            .map(|_| budget.try_admit(RequestClass::Standard).unwrap())
            .collect();
        assert!(matches!(
            manager.access().await,
            Err(SessionError::Unavailable)
        ));
        assert!(manager.status().connected);
        assert_eq!(manager.status().generation, 0);
        assert!(Arc::ptr_eq(
            manager.state.lock().unwrap().record.as_ref().unwrap(),
            &previous
        ));
        assert!(manager.journal.read().unwrap().unwrap().active);
        drop(guards);
        budget.observe_response(429, Some(std::time::Duration::from_secs(30)));
        assert!(manager.access().await.is_err());
        assert!(manager.status().connected);
        assert!(!manager.state.lock().unwrap().refresh_blocked);
    }

    #[tokio::test]
    async fn refresh_http_temporary_errors_preserve_session_but_401_clears_it() {
        for (status, connected) in [
            ("429 Too Many Requests", true),
            ("503 Service Unavailable", true),
            ("401 Unauthorized", false),
        ] {
            let response =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let (client, worker) =
                super::super::client::tests::response_fixture(Box::leak(response.into_boxed_str()));
            let mut manager = manager(false);
            manager.config.enabled = true;
            manager.client = client;
            let mut expired = record();
            expired.expires_at = 1;
            manager.state.lock().unwrap().record = Some(Arc::new(expired));
            manager.state.lock().unwrap().profile = Some(profile());
            assert!(manager.access().await.is_err());
            worker.join().unwrap();
            assert_eq!(manager.status().connected, connected, "{status}");
            assert_eq!(manager.status().generation, u64::from(!connected));
        }
    }

    #[tokio::test]
    async fn write_scope_is_explicit_per_attempt_even_before_login() {
        let mut manager = manager(false);
        manager.config.enabled = true;
        manager.config.scope = "account:read keys:read keys:write".into();
        let elevated = manager.start_with_key_creation(true).await.unwrap();
        assert_eq!(
            elevated.requested_scope,
            "account:read keys:read keys:write"
        );
        assert!(!manager.status().can_create_keys);
        let readonly = manager.start().await.unwrap();
        assert_eq!(readonly.requested_scope, "account:read keys:read");
        assert!(manager.check(elevated.generation()).is_err());
        let url = url::Url::parse(readonly.browser_url()).unwrap();
        let scopes: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k == "scope")
            .map(|(_, v)| v.into_owned())
            .collect();
        assert_eq!(scopes, vec![readonly.requested_scope.clone()]);
    }

    #[test]
    fn active_grant_controls_safe_creation_capability() {
        let manager = manager(false);
        assert!(!manager.status().can_create_keys);
        let mut grant = record();
        grant.scope = "account:read keys:read keys:write".into();
        manager.state.lock().unwrap().record = Some(Arc::new(grant));
        assert!(!manager.status().can_create_keys);
        manager.state.lock().unwrap().profile = Some(profile());
        let dto = serde_json::to_value(manager.status()).unwrap();
        assert_eq!(dto["canCreateKeys"], true);
        assert!(dto.get("scope").is_none());
        manager.begin_logout();
        assert!(!manager.status().can_create_keys);
    }

    #[test]
    fn temporary_refresh_failure_preserves_generation_journal_and_successor() {
        let manager = manager(false);
        let previous = Arc::new(record());
        manager
            .preserve_refresh_session(previous.clone(), 0)
            .unwrap();
        assert!(!manager.status().connected);
        assert_eq!(manager.status().recovery, SessionRecovery::Retryable);
        assert!(manager.journal.read().unwrap().unwrap().active);
        let mut rotated = record();
        rotated.refresh_token = "rotated-fixture".into();
        let successor = Arc::new(rotated);
        manager.state.lock().unwrap().record = Some(successor.clone());
        manager.preserve_refresh_session(previous, 0).unwrap();
        assert!(Arc::ptr_eq(
            manager.state.lock().unwrap().record.as_ref().unwrap(),
            &successor
        ));
        assert_eq!(manager.status().generation, 0);
        manager.begin_logout();
        assert!(matches!(
            manager.preserve_refresh_session(successor, 0),
            Err(SessionError::Superseded)
        ));
        assert!(!manager.status().connected);
    }
    #[tokio::test]
    async fn recovery_without_record_never_bypasses_cleanup() {
        let mut manager = manager(false);
        manager.config.enabled = true;
        assert!(manager.recover_existing().await.unwrap().is_none());
        manager.state.lock().unwrap().cleanup_account = Some("7".into());
        assert!(matches!(
            manager.recover_existing().await,
            Err(SessionError::AlreadyConnected)
        ));
        assert!(manager.state.lock().unwrap().record.is_none());
        assert_eq!(
            manager.state.lock().unwrap().cleanup_account.as_deref(),
            Some("7")
        );
    }

    #[test]
    fn refreshed_tokens_cannot_exceed_existing_grant() {
        let mut pair = TokenPair {
            access_token: "fixture".into(),
            refresh_token: "fixture-next".into(),
            expires_in: 600,
            token_type: "Bearer".into(),
            scope: "account:read keys:read keys:write".into(),
        };
        let read_grant = AuthConfig::default().login_scope(false);
        let write_grant = AuthConfig::default().login_scope(true);
        assert!(pair.validate(read_grant).is_err());
        assert!(pair.validate(write_grant).is_ok());
        pair.scope = read_grant.into();
        assert!(pair.validate(write_grant).is_ok());
        assert!(pair.validate("account:read unknown").is_err());
    }
    #[derive(Default)]
    struct BlockedRead {
        started: tokio::sync::Notify,
        released: Mutex<bool>,
        wake: std::sync::Condvar,
    }
    impl BlockedRead {
        fn wait(&self) {
            self.started.notify_one();
            // Bounded even if the test fails before its release guard is dropped.
            let _ = self
                .wake
                .wait_timeout_while(
                    self.released.lock().unwrap(),
                    std::time::Duration::from_secs(5),
                    |released| !*released,
                )
                .unwrap();
        }
        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.wake.notify_all();
        }
    }
    struct ReleaseRead(Arc<BlockedRead>);
    impl Drop for ReleaseRead {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    #[derive(Default, Clone)]
    struct Memory {
        items: Arc<Mutex<HashMap<(String, String), Vec<u8>>>>,
        reads: Arc<std::sync::atomic::AtomicUsize>,
        writes: Arc<std::sync::atomic::AtomicUsize>,
        creates: Arc<std::sync::atomic::AtomicUsize>,
        interactive_creates: Arc<std::sync::atomic::AtomicUsize>,
        interactive_writes: Arc<std::sync::atomic::AtomicUsize>,
        interactive_reads: Arc<std::sync::atomic::AtomicUsize>,
        read_error: Arc<Mutex<Option<CredentialError>>>,
        blocked_read: Option<Arc<BlockedRead>>,
        blocked_create: Option<Arc<BlockedRead>>,
        fail: bool,
        fail_delete: bool,
        require_interaction: bool,
    }
    impl CredentialBackend for Memory {
        fn create_interactive(
            &self,
            service: &str,
            account: &str,
            bytes: &[u8],
        ) -> Result<(), CredentialError> {
            self.interactive_creates
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.create(service, account, bytes)
        }
        fn create(
            &self,
            service: &str,
            account: &str,
            bytes: &[u8],
        ) -> Result<(), CredentialError> {
            self.creates
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(blocked) = &self.blocked_create {
                blocked.wait();
            }
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                return Err(CredentialError::Storage);
            }
            let mut items = self.items.lock().unwrap();
            let key = (service.into(), account.into());
            if items.contains_key(&key) {
                return Err(CredentialError::Storage);
            }
            items.insert(key, bytes.to_vec());
            Ok(())
        }
        fn write(&self, service: &str, account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                return Err(CredentialError::Storage);
            }
            self.items
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), bytes.to_vec());
            Ok(())
        }
        fn write_interactive(
            &self,
            service: &str,
            account: &str,
            bytes: &[u8],
        ) -> Result<(), CredentialError> {
            self.interactive_writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.write(service, account, bytes)
        }
        fn read_interactive(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<Vec<u8>>, CredentialError> {
            self.interactive_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.read(service, account)
        }
        fn read(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(blocked) = &self.blocked_read {
                blocked.wait();
            }
            if let Some(error) = self.read_error.lock().unwrap().take() {
                return Err(error);
            }
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&(service.into(), account.into()))
                .cloned())
        }
        fn read_without_prompt(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<Vec<u8>>, CredentialError> {
            if self.require_interaction {
                Err(CredentialError::InteractionRequired)
            } else {
                self.read(service, account)
            }
        }
        fn remove(&self, service: &str, account: &str) -> Result<(), CredentialError> {
            if self.fail || self.fail_delete {
                return Err(CredentialError::Storage);
            }
            self.items
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }
    fn manager(fail: bool) -> AuthManager<Memory> {
        let config = AuthConfig {
            enabled: false,
            ..AuthConfig::default()
        };
        let origin = config.validate().unwrap().origin();
        AuthManager::with_store(
            config,
            CredentialStore::with_backend(
                &origin,
                "test.osir.dev",
                Memory {
                    fail,
                    ..Memory::default()
                },
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn record() -> SecretRecord {
        SecretRecord {
            access_token: "fake-access".into(),
            refresh_token: "fake-refresh".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            account_id: "7".into(),
            origin: "https://osirclaw.com".into(),
            scope: "account:read".into(),
            slot: None,
        }
    }
    fn profile() -> AccountProfile {
        AccountProfile {
            id: 7,
            email: "test@example.com".into(),
            username: "test".into(),
            balance: serde_json::Number::from(0),
        }
    }
    fn persistent_manager(directory: &std::path::Path, backend: Memory) -> AuthManager<Memory> {
        let mut config = AuthConfig::default();
        config.enabled = true;
        let origin = config.validate().unwrap().origin();
        let mut manager = AuthManager::with_store(
            config,
            CredentialStore::with_backend(&origin, "test.osir.dev", backend).unwrap(),
        )
        .unwrap();
        manager.journal = Arc::new(SessionJournal::persistent(origin, directory));
        manager
    }
    #[tokio::test]
    async fn browser_login_save_allows_authorization_but_token_refresh_remains_silent() {
        use std::sync::atomic::Ordering::SeqCst;
        let directory = tempfile::tempdir().unwrap();
        let backend = Memory::default();
        let manager = persistent_manager(directory.path(), backend.clone());
        let mut fresh = record();
        fresh.slot = Some("e".repeat(32));
        manager.save(Arc::new(fresh), 0, true).await.unwrap();
        let mut refreshed = record();
        refreshed.slot = Some("e".repeat(32));
        refreshed.refresh_token = "synthetic-successor".into();
        manager.save(Arc::new(refreshed), 0, false).await.unwrap();
        assert_eq!(backend.interactive_creates.load(SeqCst), 1);
        assert_eq!(backend.creates.load(SeqCst), 1);
        assert_eq!(backend.writes.load(SeqCst), 2);
        assert_eq!(backend.interactive_writes.load(SeqCst), 0);
        assert_eq!(backend.interactive_reads.load(SeqCst), 0);
        assert_eq!(backend.items.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn fresh_login_does_not_read_update_or_delete_a_locked_legacy_item() {
        use super::super::request_budget::RequestClass;
        use std::sync::atomic::Ordering::SeqCst;
        let directory = tempfile::tempdir().unwrap();
        let backend = Memory {
            require_interaction: true,
            fail_delete: true,
            ..Memory::default()
        };
        let manager = persistent_manager(directory.path(), backend.clone());
        manager.credentials.store(&record()).unwrap();
        let legacy_items = backend.items.lock().unwrap().clone();
        manager.journal.write(Some("7"), true).unwrap();
        manager.state.lock().unwrap().recovery = SessionRecovery::UnlockRequired;
        let ready = manager.abandon_saved_login(0).unwrap();
        manager
            .commit(Arc::new(record()), profile(), ready.generation, true)
            .await
            .unwrap();
        let reference = manager.journal.read().unwrap().unwrap();
        let slot = reference.slot.unwrap();
        assert!(reference.active);
        assert_eq!(slot.len(), 32);
        assert!(manager.status().connected);
        assert_eq!(backend.items.lock().unwrap().len(), 2);
        for (key, bytes) in legacy_items {
            assert_eq!(backend.items.lock().unwrap()[&key], bytes);
        }
        assert_eq!(backend.creates.load(SeqCst), 1);
        assert_eq!(backend.interactive_creates.load(SeqCst), 1);
        assert_eq!(backend.reads.load(SeqCst), 0);
        assert_eq!(backend.interactive_reads.load(SeqCst), 0);
        assert_eq!(backend.interactive_writes.load(SeqCst), 0);
        // Startup selects precisely the new reference; locally refuse HTTP.
        let restarted = persistent_manager(
            directory.path(),
            Memory {
                require_interaction: false,
                ..backend.clone()
            },
        );
        let budget = restarted.request_budget();
        let _busy: Vec<_> = (0..4)
            .map(|_| budget.try_admit(RequestClass::Standard).unwrap())
            .collect();
        assert!(matches!(
            restarted.restore_startup().await,
            Err(SessionError::Unavailable)
        ));
        assert_eq!(
            restarted
                .state
                .lock()
                .unwrap()
                .record
                .as_ref()
                .unwrap()
                .slot
                .as_deref(),
            Some(slot.as_str())
        );
        assert_eq!(
            restarted.journal.read().unwrap().unwrap().slot.as_deref(),
            Some(slot.as_str())
        );
        assert_eq!(backend.interactive_reads.load(SeqCst), 0);
    }
    #[tokio::test]
    async fn late_fresh_save_cleanup_cannot_delete_the_next_login_for_the_same_account() {
        use super::super::request_budget::RequestClass;
        let directory = tempfile::tempdir().unwrap();
        let blocked = Arc::new(BlockedRead::default());
        let _release = ReleaseRead(blocked.clone());
        let backend = Memory {
            blocked_create: Some(blocked.clone()),
            ..Memory::default()
        };
        let manager = Arc::new(persistent_manager(directory.path(), backend.clone()));
        let budget = manager.request_budget();
        let _busy: Vec<_> = (0..4)
            .map(|_| budget.try_admit(RequestClass::Standard).unwrap())
            .collect();
        let first_manager = manager.clone();
        let first = tokio::spawn(async move {
            first_manager
                .commit(Arc::new(record()), profile(), 0, true)
                .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            blocked.started.notified(),
        )
        .await
        .unwrap();
        manager.cancel();
        let generation = manager.status().generation;
        let second_manager = manager.clone();
        let second = tokio::spawn(async move {
            second_manager
                .commit(Arc::new(record()), profile(), generation, true)
                .await
        });
        blocked.release();
        assert!(matches!(
            first.await.unwrap(),
            Err(SessionError::Superseded)
        ));
        second.await.unwrap().unwrap();
        let reference = manager.journal.read().unwrap().unwrap();
        assert!(reference.active && reference.slot.is_some());
        assert!(manager.status().connected);
        assert_eq!(backend.items.lock().unwrap().len(), 1);
        assert!(manager
            .credentials
            .load_slot_for_refresh("7", reference.slot.as_deref())
            .unwrap()
            .is_some());
    }
    #[tokio::test]
    async fn abandon_saved_login_only_tombstones_failed_recovery_without_credential_io() {
        use std::sync::atomic::Ordering::SeqCst;
        for recovery in [
            SessionRecovery::UnlockRequired,
            SessionRecovery::Retryable,
            SessionRecovery::Blocked,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let backend = Memory {
                require_interaction: true,
                fail_delete: true,
                ..Memory::default()
            };
            let manager = persistent_manager(directory.path(), backend.clone());
            manager.credentials.store(&record()).unwrap();
            manager.journal.write(Some("7"), true).unwrap();
            {
                let mut state = manager.state.lock().unwrap();
                state.recovery = recovery;
                state.cleanup_account = Some("7".into());
                if recovery == SessionRecovery::Retryable {
                    state.record = Some(Arc::new(record()));
                }
            }
            let old_items = backend.items.lock().unwrap().clone();
            let status = manager.abandon_saved_login(0).unwrap();
            assert_eq!(status.generation, 1);
            assert_eq!(status.recovery, SessionRecovery::Idle);
            assert!(!status.connected && !status.can_create_keys);
            let reference = manager.journal.read().unwrap().unwrap();
            assert!(!reference.active && reference.account.is_none());
            assert!(manager.state.lock().unwrap().record.is_none());
            assert!(manager.state.lock().unwrap().cleanup_account.is_none());
            assert_eq!(*backend.items.lock().unwrap(), old_items);
            let restarted = persistent_manager(directory.path(), backend.clone());
            assert!(!restarted.restore_startup().await.unwrap().connected);
            assert_eq!(backend.reads.load(SeqCst), 0);
            assert_eq!(backend.interactive_reads.load(SeqCst), 0);
            assert_eq!(backend.writes.load(SeqCst), 1);
            assert_eq!(backend.interactive_writes.load(SeqCst), 0);
        }
    }
    #[tokio::test]
    async fn abandon_saved_login_rejects_stale_connected_and_busy_states() {
        let directory = tempfile::tempdir().unwrap();
        let manager = persistent_manager(directory.path(), Memory::default());
        manager.journal.write(Some("7"), true).unwrap();
        for recovery in [
            SessionRecovery::Idle,
            SessionRecovery::Pending,
            SessionRecovery::Restoring,
        ] {
            manager.state.lock().unwrap().recovery = recovery;
            assert!(matches!(
                manager.abandon_saved_login(0),
                Err(SessionError::AlreadyConnected)
            ));
        }
        manager.state.lock().unwrap().recovery = SessionRecovery::UnlockRequired;
        assert!(matches!(
            manager.abandon_saved_login(1),
            Err(SessionError::Superseded)
        ));
        {
            let _busy = manager.recovery.lock().await;
            assert!(matches!(
                manager.abandon_saved_login(0),
                Err(SessionError::Unavailable)
            ));
        }
        {
            let _busy = manager.operation.lock().await;
            assert!(matches!(
                manager.abandon_saved_login(0),
                Err(SessionError::Unavailable)
            ));
        }
        {
            let mut state = manager.state.lock().unwrap();
            state.record = Some(Arc::new(record()));
            state.profile = Some(profile());
        }
        assert!(matches!(
            manager.abandon_saved_login(0),
            Err(SessionError::AlreadyConnected)
        ));
        assert_eq!(manager.status().generation, 0);
        assert!(manager.journal.read().unwrap().unwrap().active);
    }
    #[test]
    fn abandon_saved_login_journal_failure_preserves_old_state() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = persistent_manager(directory.path(), Memory::default());
        manager.journal.write(Some("7"), true).unwrap();
        let old_journal = manager.journal.clone();
        let blocker = directory.path().join("file");
        std::fs::write(&blocker, b"fixture").unwrap();
        manager.journal = Arc::new(SessionJournal::persistent(
            manager.client.origin(),
            &blocker,
        ));
        {
            let mut state = manager.state.lock().unwrap();
            state.recovery = SessionRecovery::Idle;
            state.record = Some(Arc::new(record()));
            state.cleanup_account = Some("7".into());
        }
        assert!(matches!(
            manager.abandon_saved_login(0),
            Err(SessionError::Storage)
        ));
        assert_eq!(manager.status().generation, 0);
        assert_eq!(manager.status().recovery, SessionRecovery::Retryable);
        assert!(manager.state.lock().unwrap().record.is_some());
        assert_eq!(
            manager.state.lock().unwrap().cleanup_account.as_deref(),
            Some("7")
        );
        assert!(old_journal.read().unwrap().unwrap().active);
    }
    #[tokio::test]
    async fn abandon_saved_login_prevents_late_credential_reads_from_reviving_old_login() {
        use std::sync::atomic::Ordering::SeqCst;
        let directory = tempfile::tempdir().unwrap();
        let blocked = Arc::new(BlockedRead::default());
        let _release = ReleaseRead(blocked.clone());
        let backend = Memory {
            blocked_read: Some(blocked.clone()),
            ..Memory::default()
        };
        let manager = persistent_manager(directory.path(), backend.clone());
        manager.credentials.store(&record()).unwrap();
        manager.journal.write(Some("7"), true).unwrap();
        let mut restore = Box::pin(manager.restore_startup());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::select! {
                _ = blocked.started.notified() => {},
                _ = &mut restore => panic!("read completed before release"),
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), restore)
                .await
                .is_err()
        );
        let status = manager.abandon_saved_login(0).unwrap();
        blocked.release();
        let gate = manager.keychain_gate.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                let _gate = gate.lock().unwrap();
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(manager.status().generation, status.generation);
        assert!(!manager.status().connected);
        assert!(manager.state.lock().unwrap().record.is_none());
        assert!(matches!(
            manager.restore_startup().await,
            Err(SessionError::Superseded)
        ));
        assert!(!manager.journal.read().unwrap().unwrap().active);
        assert_eq!(backend.reads.load(SeqCst), 1);
    }
    #[tokio::test]
    async fn abandon_saved_login_cancelled_generation_cannot_start_or_commit() {
        let directory = tempfile::tempdir().unwrap();
        let manager = persistent_manager(directory.path(), Memory::default());
        manager.state.lock().unwrap().recovery = SessionRecovery::UnlockRequired;
        let status = manager.abandon_saved_login(0).unwrap();
        manager.cancel();
        assert!(matches!(
            manager
                .start_for_generation(false, Some(status.generation))
                .await,
            Err(SessionError::Superseded)
        ));
        assert!(matches!(
            manager
                .commit(Arc::new(record()), profile(), status.generation, true)
                .await,
            Err(SessionError::Superseded)
        ));
        assert!(!manager.status().connected);
        assert!(!manager.journal.read().unwrap().unwrap().active);
    }
    #[tokio::test]
    async fn abandon_saved_login_new_save_failure_preserves_old_item_and_blocks_success() {
        use super::super::request_budget::RequestClass;
        let directory = tempfile::tempdir().unwrap();
        let backend = Memory::default();
        let mut manager = persistent_manager(directory.path(), backend.clone());
        manager.credentials.store(&record()).unwrap();
        let old_items = backend.items.lock().unwrap().clone();
        manager.journal.write(Some("7"), true).unwrap();
        manager.state.lock().unwrap().recovery = SessionRecovery::UnlockRequired;
        let status = manager.abandon_saved_login(0).unwrap();
        manager.credentials = Arc::new(
            CredentialStore::with_backend(
                &manager.client.origin(),
                "test.osir.dev",
                Memory {
                    fail: true,
                    ..backend.clone()
                },
            )
            .unwrap(),
        );
        // Refuse request admission: this synthetic storage-failure test makes
        // no network requests, including the existing new-grant cleanup.
        let budget = manager.request_budget();
        let _busy: Vec<_> = (0..4)
            .map(|_| budget.try_admit(RequestClass::Standard).unwrap())
            .collect();
        let mut successor = record();
        successor.access_token = "synthetic-successor-access".into();
        successor.refresh_token = "synthetic-successor-refresh".into();
        assert!(matches!(
            manager
                .commit(Arc::new(successor), profile(), status.generation, true)
                .await,
            Err(SessionError::Storage)
        ));
        assert!(!manager.status().connected);
        assert_eq!(manager.status().recovery, SessionRecovery::Blocked);
        assert_eq!(*backend.items.lock().unwrap(), old_items);
        assert!(!manager.journal.read().unwrap().unwrap().active);
    }
    #[tokio::test]
    async fn durable_logout_tombstone_blocks_restart_when_keychain_delete_fails() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Memory {
            fail_delete: true,
            ..Memory::default()
        };
        let manager = persistent_manager(directory.path(), backend.clone());
        manager
            .commit(Arc::new(record()), profile(), 0, true)
            .await
            .unwrap();
        assert!(manager.journal.read().unwrap().unwrap().active);
        let ticket = manager.begin_logout();
        let slot = ticket.slot.clone();
        assert!(!manager.journal.read().unwrap().unwrap().active);
        // Suppress HTTP: the local deletion failure is tested independently.
        let ticket = LogoutTicket {
            record: None,
            ..ticket
        };
        let report = manager.finish_logout(ticket).await;
        assert!(!report.local_cleared);
        let restarted = persistent_manager(directory.path(), backend.clone());
        assert!(!restarted.restore_startup().await.unwrap().connected);
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(restarted
            .credentials
            .load_slot_for_refresh("7", slot.as_deref())
            .unwrap()
            .is_some());
    }
    #[tokio::test]
    async fn startup_restore_loses_to_cancel_logout_and_new_attempt_before_io() {
        for action in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let manager = persistent_manager(directory.path(), Memory::default());
            manager.journal.write(Some("7"), true).unwrap();
            match action {
                0 => manager.cancel(),
                1 => {
                    manager.begin_logout();
                }
                _ => {
                    let mut state = manager.state.lock().unwrap();
                    state.invalidate();
                    let (sender, _receiver) = oneshot::channel();
                    state.cancel = Some(sender);
                }
            }
            assert!(matches!(
                manager.restore_startup().await,
                Err(SessionError::Superseded)
            ));
            assert!(!manager.status().connected);
        }
    }
    #[tokio::test]
    async fn startup_preserves_locked_credentials_until_explicit_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Memory {
            require_interaction: true,
            ..Memory::default()
        };
        let manager = persistent_manager(directory.path(), backend.clone());
        manager.journal.write(Some("7"), true).unwrap();
        for _ in 0..2 {
            let status = manager.restore_startup().await.unwrap();
            assert!(!status.connected);
            assert_eq!(status.recovery, SessionRecovery::UnlockRequired);
        }
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(manager.journal.read().unwrap().unwrap().active);
        assert!(matches!(
            manager.start().await,
            Err(SessionError::AlreadyConnected)
        ));
        assert!(matches!(
            manager.recover_existing_for_generation(1).await,
            Err(SessionError::Superseded)
        ));
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        // The explicit attempt is the first interactive read. A missing item
        // is now authoritative and cannot be silently replaced by new login.
        assert!(matches!(
            manager.recover_existing_for_generation(0).await,
            Err(SessionError::Unauthorized)
        ));
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(manager.status().recovery, SessionRecovery::Blocked);
        assert!(!manager.journal.read().unwrap().unwrap().active);
    }
    #[tokio::test]
    async fn startup_read_timeout_preserves_reference_and_never_queues_another_read() {
        use super::super::request_budget::RequestClass;
        let directory = tempfile::tempdir().unwrap();
        let blocked = Arc::new(BlockedRead::default());
        let _release = ReleaseRead(blocked.clone());
        let backend = Memory {
            blocked_read: Some(blocked.clone()),
            ..Memory::default()
        };
        let manager = persistent_manager(directory.path(), backend.clone());
        manager.credentials.store(&record()).unwrap();
        manager.journal.write(Some("7"), true).unwrap();
        let mut restore = Box::pin(manager.restore_startup());
        // Synchronize with the actual blocking credential read before timing out.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::select! {
                _ = blocked.started.notified() => {},
                _ = &mut restore => panic!("read completed before release"),
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), restore)
                .await
                .is_err()
        );
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        let status = manager.status();
        assert_eq!(status.generation, 0);
        assert_eq!(status.recovery, SessionRecovery::UnlockRequired);
        assert!(!status.connected && !status.can_create_keys);
        assert!(manager.journal.read().unwrap().unwrap().active);
        assert!(manager.state.lock().unwrap().cancel.is_none());
        assert!(manager.state.lock().unwrap().record.is_none());
        assert!(manager.operation.try_lock().is_ok());
        assert!(manager.recovery.try_lock().is_ok());
        assert!(matches!(
            manager.start().await,
            Err(SessionError::AlreadyConnected)
        ));
        // Neither repeated startup nor simultaneous explicit clicks can queue
        // another OS read behind the detached, still-blocked Keychain call.
        assert_eq!(
            manager.restore_startup().await.unwrap().recovery,
            SessionRecovery::UnlockRequired
        );
        let (first, follower) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(
                manager.recover_existing_for_generation(0),
                manager.recover_existing_for_generation(0),
            )
        })
        .await
        .unwrap();
        assert!(first.is_ok() && follower.is_ok());
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(manager.status().recovery, SessionRecovery::UnlockRequired);
        blocked.release();
        let gate = manager.keychain_gate.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                let _gate = gate.lock().unwrap();
            }),
        )
        .await
        .unwrap()
        .unwrap();
        // A late read result cannot publish login; only an explicit retry may
        // load the trusted reference again. Refuse HTTP locally via the budget.
        assert!(manager.state.lock().unwrap().record.is_none());
        let budget = manager.request_budget();
        let _busy: Vec<_> = (0..4)
            .map(|_| budget.try_admit(RequestClass::Standard).unwrap())
            .collect();
        assert!(matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                manager.recover_existing_for_generation(0),
            )
            .await
            .unwrap(),
            Err(SessionError::Unavailable)
        ));
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(manager.status().generation, 0);
        assert_eq!(manager.status().recovery, SessionRecovery::Retryable);
        assert!(manager.state.lock().unwrap().record.is_some());
        assert!(manager.journal.read().unwrap().unwrap().active);
    }
    #[tokio::test]
    async fn startup_temporary_storage_errors_preserve_explicit_retry() {
        for kind in 0..3 {
            let error = || match kind {
                0 => CredentialError::Storage,
                1 => CredentialError::Unavailable,
                _ => CredentialError::InteractionRequired,
            };
            let directory = tempfile::tempdir().unwrap();
            let backend = Memory::default();
            let manager = persistent_manager(directory.path(), backend.clone());
            manager.journal.write(Some("7"), true).unwrap();
            *backend.read_error.lock().unwrap() = Some(error());
            assert_eq!(
                manager.restore_startup().await.unwrap().recovery,
                SessionRecovery::UnlockRequired
            );
            *backend.read_error.lock().unwrap() = Some(error());
            assert_eq!(
                manager
                    .recover_existing_for_generation(0)
                    .await
                    .unwrap()
                    .unwrap()
                    .recovery,
                SessionRecovery::UnlockRequired
            );
            assert_eq!(manager.status().generation, 0);
            assert!(manager.journal.read().unwrap().unwrap().active);
            assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
            assert!(manager.state.lock().unwrap().cancel.is_none());
            // Once the backend gives a definite missing result, it is terminal.
            assert!(matches!(
                manager.recover_existing_for_generation(0).await,
                Err(SessionError::Unauthorized)
            ));
            assert_eq!(manager.status().recovery, SessionRecovery::Blocked);
            assert!(!manager.journal.read().unwrap().unwrap().active);
        }
    }
    #[tokio::test]
    async fn startup_missing_malformed_and_mismatched_credentials_remain_blocked() {
        for invalid in 0..6 {
            let directory = tempfile::tempdir().unwrap();
            let backend = Memory::default();
            let manager = persistent_manager(directory.path(), backend.clone());
            manager.journal.write(Some("7"), true).unwrap();
            if invalid > 0 {
                manager.credentials.store(&record()).unwrap();
                let mut items = backend.items.lock().unwrap();
                let bytes = items.values_mut().next().unwrap();
                if invalid == 1 {
                    *bytes = b"invalid fixture".to_vec();
                } else {
                    let mut value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
                    match invalid {
                        2 => value["account_id"] = serde_json::json!("8"),
                        3 => value["origin"] = serde_json::json!("https://other.example.invalid"),
                        4 => value["scope"] = serde_json::json!("account:read unknown"),
                        _ => value["version"] = serde_json::json!(2),
                    }
                    *bytes = serde_json::to_vec(&value).unwrap();
                }
            }
            assert!(manager.restore_startup().await.is_err());
            assert!(!manager.status().connected);
            assert_eq!(manager.status().recovery, SessionRecovery::Blocked);
            assert_eq!(manager.status().generation, 1);
            assert!(manager.state.lock().unwrap().record.is_none());
            assert!(!manager.journal.read().unwrap().unwrap().active);
        }
    }
    #[tokio::test]
    async fn timed_out_startup_recovery_releases_busy_state_without_reading_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Memory::default();
        let manager = persistent_manager(directory.path(), backend.clone());
        manager.journal.write(Some("7"), true).unwrap();
        let _busy = manager.operation.lock().await;
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            manager.restore_startup(),
        )
        .await
        .is_err());
        assert!(!manager.status().connected);
        assert_eq!(manager.status().recovery, SessionRecovery::Blocked);
        assert_eq!(backend.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(manager.journal.read().unwrap().unwrap().active);
        assert!(matches!(
            manager.start().await,
            Err(SessionError::AlreadyConnected)
        ));
    }
    #[tokio::test]
    async fn canceled_rotation_or_write_leaves_durable_tombstone() {
        let directory = tempfile::tempdir().unwrap();
        let manager = persistent_manager(directory.path(), Memory::default());
        manager.journal.write(Some("7"), true).unwrap();
        manager.state.lock().unwrap().cleanup_account = Some("7".into());
        {
            let _guard = InvalidationGuard {
                state: &manager.state,
                journal: &manager.journal,
                generation: 0,
                armed: true,
            };
        }
        assert!(!manager.journal.read().unwrap().unwrap().active);
        assert_eq!(manager.status().generation, 1);
    }
    #[tokio::test]
    async fn journal_failure_cannot_publish_and_old_save_cannot_activate() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("file");
        std::fs::write(&blocker, b"fixture").unwrap();
        let manager = persistent_manager(&blocker, Memory::default());
        assert!(manager.save(Arc::new(record()), 0, false).await.is_err());
        assert!(!manager.status().connected);
        let manager = persistent_manager(directory.path(), Memory::default());
        manager.cancel();
        assert!(matches!(
            manager.save(Arc::new(record()), 0, false).await,
            Err(SessionError::Superseded)
        ));
        assert!(!manager.journal.read().unwrap().unwrap().active);
    }
    #[tokio::test]
    async fn old_logout_cleanup_cannot_tombstone_or_delete_new_login() {
        let directory = tempfile::tempdir().unwrap();
        let manager = persistent_manager(directory.path(), Memory::default());
        let ticket = manager.begin_logout();
        manager.state.lock().unwrap().invalidate();
        manager
            .commit(Arc::new(record()), profile(), 2, true)
            .await
            .unwrap();
        assert!(!manager.finish_logout(ticket).await.local_cleared);
        assert!(manager.status().connected);
        assert!(manager.journal.read().unwrap().unwrap().active);
        let reference = manager.journal.read().unwrap().unwrap();
        assert!(manager
            .credentials
            .load_slot_for_refresh("7", reference.slot.as_deref())
            .unwrap()
            .is_some());
    }
    #[tokio::test]
    async fn key_operations_reject_stale_generation_and_missing_write_scope_before_http() {
        let mut manager = manager(false);
        manager.config.enabled = true;
        manager.state.lock().unwrap().record = Some(Arc::new(record()));
        let request = super::super::client::CreateKeyRequest {
            name: "fixture".into(),
            group_id: Some(7),
            quota: serde_json::Number::from(0),
            expires_in_days: None,
        };
        assert!(matches!(
            manager.read_key(7, 1).await,
            Err(SessionError::Superseded)
        ));
        assert!(matches!(
            manager.create_key(request, 0).await,
            Err(CreateSessionKeyError::WriteScopeRequired)
        ));
        assert!(matches!(
            manager.mark_announcement_read_for_generation(7, 1).await,
            Err(SessionError::Superseded)
        ));
    }
    #[test]
    fn late_cancel_preserves_connected_session_and_generation() {
        let manager = manager(false);
        let current = Arc::new(record());
        {
            let mut state = manager.state.lock().unwrap();
            state.record = Some(current.clone());
            state.profile = Some(profile());
        }
        manager.cancel();
        manager.cancel_if_generation(0);
        assert!(manager.status().connected);
        assert_eq!(manager.status().generation, 0);
        assert!(Arc::ptr_eq(
            manager.state.lock().unwrap().record.as_ref().unwrap(),
            &current
        ));
    }
    #[test]
    fn older_attempt_cleanup_cannot_cancel_new_attempt() {
        let manager = manager(false);
        let (sender, _receiver) = oneshot::channel();
        {
            let mut state = manager.state.lock().unwrap();
            state.generation = 2;
            state.cancel = Some(sender);
        }
        manager.cancel_if_generation(1);
        assert_eq!(manager.status().generation, 2);
        assert!(manager.state.lock().unwrap().cancel.is_some());
        manager.cancel_if_generation(2);
        assert_eq!(manager.status().generation, 3);
    }
    #[tokio::test]
    async fn concurrent_401_followers_reuse_committed_pair_without_refresh_post() {
        let mut manager = manager(false);
        manager.config.enabled = true;
        let manager = Arc::new(manager);
        let rejected = Arc::new(record());
        let winner = Arc::new(record());
        manager.state.lock().unwrap().record = Some(winner.clone());
        let (a, b) = tokio::join!(
            manager.access_after_rejection(Some((0, rejected.clone()))),
            manager.access_after_rejection(Some((0, rejected))),
        );
        assert!(Arc::ptr_eq(&a.unwrap().1, &winner));
        assert!(Arc::ptr_eq(&b.unwrap().1, &winner));
        manager.begin_logout();
        assert!(matches!(
            manager.access_after_rejection(Some((0, winner))).await,
            Err(SessionError::Superseded)
        ));
    }
    #[tokio::test]
    async fn disabled_native_config_never_binds_or_logs_in() {
        let manager = manager(false);
        assert!(!manager.config.enabled);
        assert!(matches!(manager.start().await, Err(SessionError::Disabled)));
        assert!(!manager.status().connected);
    }
    #[test]
    fn invoice_grants_remain_explicit_and_cannot_be_escalated() {
        let config = AuthConfig::default();
        assert_eq!(
            config.login_scope_with_invoices(false, false),
            "account:read keys:read"
        );
        let requested = config.login_scope_with_invoices(true, true);
        assert_eq!(
            requested,
            "account:read keys:read keys:write invoices:read invoices:write"
        );
        let mut pair = TokenPair {
            access_token: "fixture-access".into(),
            refresh_token: "fixture-refresh".into(),
            expires_in: 600,
            token_type: "Bearer".into(),
            scope: requested.clone(),
        };
        assert!(pair.validate(&requested).is_ok());
        assert!(pair.validate(config.login_scope(true)).is_err());
        pair.scope = "account:read invoices:read invoices:read".into();
        assert!(pair.validate(&requested).is_err());
        pair.scope = "account:read future:unknown".into();
        assert!(pair.validate(&requested).is_err());
    }

    #[tokio::test]
    async fn invoice_scope_is_requested_only_from_fresh_advertised_support() {
        let mut manager = manager(false);
        manager.config.enabled = true;
        let capability = manager.capability();
        *manager.readiness.lock().await =
            Some((std::time::Instant::now(), capability.clone(), true));
        let supported = manager.start().await.unwrap();
        assert_eq!(
            supported.requested_scope,
            "account:read keys:read invoices:read invoices:write"
        );
        *manager.readiness.lock().await = Some((
            std::time::Instant::now() - std::time::Duration::from_secs(61),
            capability.clone(),
            true,
        ));
        let stale = manager.start().await.unwrap();
        assert_eq!(stale.requested_scope, "account:read keys:read");
        *manager.readiness.lock().await = Some((std::time::Instant::now(), capability, false));
        let unavailable = manager.start().await.unwrap();
        assert_eq!(unavailable.requested_scope, "account:read keys:read");
    }

    #[test]
    fn response_rejects_scope_escalation_type_expiry_and_tokens() {
        let mut pair = TokenPair {
            access_token: "fake".into(),
            refresh_token: "fake-refresh".into(),
            expires_in: 600,
            token_type: "Bearer".into(),
            scope: "account:read".into(),
        };
        assert!(pair.validate("account:read").is_ok());
        pair.scope = "account:read keys:write".into();
        assert!(pair.validate("account:read").is_err());
        pair.scope = "account:read account:read".into();
        assert!(pair.validate("account:read").is_err());
        pair.scope = "account:read".into();
        for seconds in [1, 30, 60] {
            pair.expires_in = seconds;
            assert!(pair.validate("account:read").is_ok());
        }
        pair.expires_in = 0;
        assert!(pair.validate("account:read").is_err());
        pair.expires_in = -1;
        assert!(pair.validate("account:read").is_err());
        pair.expires_in = i64::MAX;
        assert!(pair.validate("account:read").is_err());
        pair.expires_in = 600;
        pair.token_type = "Basic".into();
        assert!(pair.validate("account:read").is_err());
    }
    #[tokio::test]
    async fn storage_failure_cannot_publish_session() {
        let manager = manager(true);
        // Test the persistence seam without any HTTP or real Keychain.
        assert!(manager.save(Arc::new(record()), 0, false).await.is_err());
        assert!(!manager.status().connected);
    }
    #[tokio::test]
    async fn persisted_commit_projects_only_safe_profile() {
        let manager = manager(false);
        let mut r = record();
        r.origin = manager.client.origin();
        manager
            .commit(Arc::new(r), profile(), 0, true)
            .await
            .unwrap();
        let json = serde_json::to_string(&manager.status()).unwrap();
        assert!(!json.contains("fake-access"));
        assert!(!json.contains("fake-refresh"));
        assert!(manager.status().connected);
    }
    #[tokio::test]
    async fn logout_invalidates_before_operation_lock_and_reports_delete_failure() {
        let manager = Arc::new(manager(true));
        {
            let mut state = manager.state.lock().unwrap();
            state.record = Some(Arc::new(record()));
            state.profile = Some(profile());
            state.cleanup_account = Some("7".into());
        }
        let operation = manager.operation.lock().await;
        let other = manager.clone();
        let task = tokio::spawn(async move { other.logout().await });
        tokio::task::yield_now().await;
        assert!(!manager.status().connected);
        assert_eq!(manager.status().generation, 1);
        // Avoid remote HTTP entirely: no credentials remain in coordinator state.
        task.abort();
        let _ = task.await;
        drop(operation);
        let report = manager.logout().await;
        assert!(!report.local_cleared);
        assert!(!report.remote_revoked);
    }
    #[test]
    fn dropped_refresh_future_blocks_old_generation_and_token_replay() {
        let manager = manager(false);
        {
            let mut state = manager.state.lock().unwrap();
            state.record = Some(Arc::new(record()));
        }
        {
            let _guard = InvalidationGuard {
                state: &manager.state,
                journal: &manager.journal,
                generation: 0,
                armed: true,
            };
        }
        assert!(!manager.status().connected);
        assert!(manager.check(0).is_err());
    }
    #[tokio::test]
    async fn stale_grant_is_rejected_before_network() {
        let mut manager = manager(false);
        manager.config.enabled = true;
        manager.begin_logout();
        let grant = LoginGrant {
            generation: 0,
            requested_scope: "account:read keys:read".into(),
            grant: AuthorizationGrant {
                code: "fake".into(),
                verifier: "fake".into(),
                redirect_uri: "http://127.0.0.1:1/osir/desktop/callback".into(),
            },
        };
        assert!(matches!(
            manager.complete(grant).await,
            Err(SessionError::Superseded)
        ));
    }
    #[tokio::test]
    async fn queued_refresh_followers_recheck_rotated_session_under_singleflight_lock() {
        let mut manager = manager(false);
        manager.config.enabled = true;
        let manager = Arc::new(manager);
        let mut old = record();
        old.expires_at = 1;
        manager.state.lock().unwrap().record = Some(Arc::new(old));
        let operation = manager.operation.lock().await;
        let a = manager.clone();
        let b = manager.clone();
        let first = tokio::spawn(async move { a.access().await });
        let second = tokio::spawn(async move { b.access().await });
        tokio::task::yield_now().await;
        let winner = Arc::new(record());
        manager.state.lock().unwrap().record = Some(winner.clone());
        drop(operation);
        let (_, a) = first.await.unwrap().unwrap();
        let (_, b) = second.await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&winner, &a));
        assert!(Arc::ptr_eq(&a, &b));
    }
}

impl<B: CredentialBackend + 'static> AuthManager<B> {
    /// Explicit read, no background polling or automatic token POST replay.
    /// A 401 is returned to the user; a later explicit action may retry.
    pub(crate) async fn read_image_contract(
        &self,
        key_id: i64,
        expected: u64,
        expected_account: &str,
        expected_origin: &str,
    ) -> Result<SessionRead<serde_json::Value>, SessionError> {
        self.check(expected)?;
        if key_id <= 0 || self.client.origin() != expected_origin {
            return Err(SessionError::Configuration);
        }
        let (generation, record) = self.access().await?;
        if generation != expected || record.account_id != expected_account {
            return Err(SessionError::Superseded);
        }
        let result = self
            .client
            .get_image_contract(key_id, &record.access_token)
            .await;
        self.check(generation)?;
        Ok(SessionRead {
            generation,
            data: result?,
        })
    }
}

impl<B: CredentialBackend + 'static> AuthManager<B> {
    /// Synchronous, short local mutation only; caller must not re-enter session
    /// methods or perform network work. Serializes authorization with logout.
    pub(crate) fn with_current_image_account<R>(
        &self,
        expected: u64,
        expected_account: &str,
        expected_origin: &str,
        operation: impl FnOnce() -> R,
    ) -> Result<R, SessionError> {
        self.require_enabled()?;
        if self.client.origin() != expected_origin {
            return Err(SessionError::Configuration);
        }
        let state = self.state.lock().map_err(|_| SessionError::Unavailable)?;
        if state.generation != expected
            || state
                .record
                .as_ref()
                .is_none_or(|record| record.account_id != expected_account)
            || state
                .profile
                .as_ref()
                .is_none_or(|profile| profile.id.to_string() != expected_account)
        {
            return Err(SessionError::Superseded);
        }
        let result = operation();
        drop(state);
        Ok(result)
    }
}
