//! Separate optional anonymous metrics and account-linked version summaries.
//! Neither reports hardware, model configuration, paths or conversation data.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;
use uuid::Uuid;

const INTERVAL: Duration = Duration::from_secs(15 * 60);
const MAX_BYTES: usize = 2048;
const STORAGE_ERROR: &str = "客户端统计偏好暂不可用，请重试。";
const UNAVAILABLE: &str =
    "当前发行版不发送客户端统计；仅登录后的正式发布版参与，DEV、Beta 与未配置正式更新的本地包均不发送。";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preferences {
    schema_version: u8,
    enabled: bool,
    installation_id: Option<String>,
    last_attempt_unix_ms: Option<u64>,
    // Never inherit anonymous opt-in as consent to identify an account.
    #[serde(default)]
    account_enabled: bool,
    #[serde(default)]
    account_last_attempt_unix_ms: Option<u64>,
    #[serde(default)]
    failure_last_attempt_unix_ms: Option<u64>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            enabled: true,
            installation_id: None,
            last_attempt_unix_ms: None,
            account_enabled: false,
            account_last_attempt_unix_ms: None,
            failure_last_attempt_unix_ms: None,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTelemetryStatus {
    enabled: bool,
    account_enabled: bool,
    available: bool,
    reason: Option<&'static str>,
}

/// Monotonic limits remain authoritative if the wall clock changes. A persisted
/// timestamp in the future delays at most one interval in this process.
struct RateLimit {
    next_attempt: Instant,
}
impl RateLimit {
    fn new(last: Option<u64>, wall: u64, monotonic: Instant) -> Self {
        let delay = last
            .map(|last| INTERVAL.saturating_sub(Duration::from_millis(wall.saturating_sub(last))))
            .unwrap_or_default();
        Self {
            next_attempt: monotonic + delay,
        }
    }
    fn reserve(&mut self, now: Instant) -> bool {
        if now < self.next_attempt {
            return false;
        }
        self.next_attempt = now + INTERVAL;
        true
    }
}

struct LocalState {
    path: PathBuf,
    preferences: Preferences,
    rate: RateLimit,
    account_rate: RateLimit,
    failure_rate: RateLimit,
}
#[derive(Default)]
struct Runtime {
    local: Option<LocalState>,
    worker_started: bool,
    preference_generation: u64,
}
impl Runtime {
    fn local(&mut self, path: &Path, wall: u64, now: Instant) -> Result<&mut LocalState, String> {
        if self.local.is_none() {
            let preferences = read_preferences(path)?;
            self.local = Some(LocalState {
                path: path.to_owned(),
                rate: RateLimit::new(preferences.last_attempt_unix_ms, wall, now),
                account_rate: RateLimit::new(preferences.account_last_attempt_unix_ms, wall, now),
                failure_rate: RateLimit::new(preferences.failure_last_attempt_unix_ms, wall, now),
                preferences,
            });
        }
        let local = self.local.as_mut().ok_or(STORAGE_ERROR)?;
        if local.path != path {
            return Err(STORAGE_ERROR.into());
        }
        Ok(local)
    }
    fn request_worker(&mut self, available: bool, connected: bool, enabled: bool) -> bool {
        if !available || !connected || !enabled || self.worker_started {
            return false;
        }
        self.worker_started = true;
        true
    }
}
#[derive(Default)]
struct Inner {
    runtime: Mutex<Runtime>,
    changed: tokio::sync::Notify,
}
#[derive(Clone, Default)]
pub struct ClientTelemetryState(Arc<Inner>);

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Event {
    Heartbeat,
    UpdateInstalled,
    UpdateFailed,
    CredentialFailed,
    FrontendFailed,
}
#[derive(Serialize)]
struct Payload<'a> {
    event_id: String,
    installation_id: &'a str,
    version: &'a str,
    channel: &'static str,
    platform: &'a str,
    event: Event,
}
struct Prepared {
    body: Vec<u8>,
    preference_generation: u64,
}

fn wall_now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}
fn platform_key() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-aarch64"),
        ("macos", "x86_64") => Some("darwin-x86_64"),
        ("windows", "aarch64") => Some("windows-aarch64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("windows", "x86") => Some("windows-i686"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    }
}
fn production_release(identifier: &str, release: bool, updater: bool, version: &str) -> bool {
    let mut parts = version.split('.');
    let stable_version = (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
                && part.parse::<u64>().is_ok()
        })
    }) && parts.next().is_none();
    identifier == "com.osir.client" && release && updater && stable_version
}
fn available(app: &tauri::AppHandle) -> bool {
    production_release(
        &app.config().identifier,
        !cfg!(debug_assertions),
        crate::update_distribution::ensure_update_available(app.config()).is_ok(),
        &app.package_info().version.to_string(),
    ) && platform_key().is_some()
}
fn connected(app: &tauri::AppHandle) -> bool {
    app.try_state::<crate::commands::OsirAccountState>()
        .is_some_and(|state| state.auth.status().connected)
}
fn storage_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    // Local app data stays outside exported/imported CC Switch configuration.
    app.path()
        .app_local_data_dir()
        .map(|path| path.join("client-telemetry.json"))
        .map_err(|_| STORAGE_ERROR.into())
}
fn read_preferences(path: &Path) -> Result<Preferences, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preferences::default())
        }
        Err(_) => return Err(STORAGE_ERROR.into()),
    };
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err(STORAGE_ERROR.into());
    }
    let bytes = std::fs::read(path).map_err(|_| STORAGE_ERROR)?;
    let preferences: Preferences = serde_json::from_slice(&bytes).map_err(|_| STORAGE_ERROR)?;
    if preferences.schema_version != 1
        || preferences.installation_id.as_ref().is_some_and(|id| {
            Uuid::parse_str(id)
                .map(|uuid| uuid.get_version_num() != 4 || uuid.to_string() != *id)
                .unwrap_or(true)
        })
    {
        return Err(STORAGE_ERROR.into());
    }
    Ok(preferences)
}
fn write_preferences(path: &Path, preferences: &Preferences) -> Result<(), String> {
    let bytes = serde_json::to_vec(preferences).map_err(|_| STORAGE_ERROR)?;
    crate::config::atomic_write_private(path, &bytes).map_err(|_| STORAGE_ERROR.into())
}
fn trusted_endpoint(base: &str) -> Option<url::Url> {
    let url = url::Url::parse(base).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.path() != "/api/v1/"
        || url.query().is_some()
        || url.fragment().is_some()
        || base.trim() != base
        || base.chars().any(char::is_control)
    {
        return None;
    }
    url.join("osir-client/events").ok()
}

impl ClientTelemetryState {
    fn status(&self, path: &Path, available: bool) -> Result<ClientTelemetryStatus, String> {
        let mut runtime = self.0.runtime.lock().map_err(|_| STORAGE_ERROR)?;
        let local = runtime.local(path, wall_now().ok_or(STORAGE_ERROR)?, Instant::now())?;
        Ok(ClientTelemetryStatus {
            enabled: local.preferences.enabled,
            account_enabled: local.preferences.account_enabled,
            available,
            reason: (!available).then_some(UNAVAILABLE),
        })
    }
    fn set_enabled(&self, path: &Path, enabled: bool) -> Result<(), String> {
        self.set_preference(path, enabled, false)
    }
    fn set_account_enabled(&self, path: &Path, enabled: bool) -> Result<(), String> {
        self.set_preference(path, enabled, true)
    }
    fn set_preference(&self, path: &Path, enabled: bool, account: bool) -> Result<(), String> {
        let mut runtime = self.0.runtime.lock().map_err(|_| STORAGE_ERROR)?;
        let local = runtime.local(path, wall_now().ok_or(STORAGE_ERROR)?, Instant::now())?;
        let mut next = local.preferences.clone();
        if account {
            next.account_enabled = enabled;
        } else {
            next.enabled = enabled;
        }
        write_preferences(path, &next)?;
        local.preferences = next;
        runtime.preference_generation = runtime.preference_generation.wrapping_add(1);
        drop(runtime);
        // Interrupt a sleeping worker or an in-flight optional HTTP request.
        self.0.changed.notify_waiters();
        Ok(())
    }
    fn prepare_account(&self, path: &Path, allowed: bool, wall: u64, now: Instant) -> Option<u64> {
        if !allowed {
            return None;
        }
        let mut runtime = self.0.runtime.lock().ok()?;
        let generation = runtime.preference_generation;
        let local = runtime.local(path, wall, now).ok()?;
        if !local.preferences.account_enabled || !local.account_rate.reserve(now) {
            return None;
        }
        local.preferences.account_last_attempt_unix_ms = Some(wall);
        write_preferences(path, &local.preferences).ok()?;
        Some(generation)
    }
    fn account_still_allowed(&self, generation: u64) -> bool {
        self.0.runtime.lock().is_ok_and(|runtime| {
            runtime.preference_generation == generation
                && runtime
                    .local
                    .as_ref()
                    .is_some_and(|local| local.preferences.account_enabled)
        })
    }
    fn prepare(
        &self,
        path: &Path,
        allowed: bool,
        event: Event,
        version: &str,
        platform: &str,
        wall: u64,
        now: Instant,
    ) -> Option<Prepared> {
        // Neither a renderer snapshot nor a local preference can enable DEV/beta.
        if !allowed {
            return None;
        }
        let mut runtime = self.0.runtime.lock().ok()?;
        let generation = runtime.preference_generation;
        let local = runtime.local(path, wall, now).ok()?;
        let failure = matches!(event, Event::CredentialFailed | Event::FrontendFailed);
        let rate = if failure {
            &mut local.failure_rate
        } else {
            &mut local.rate
        };
        if !local.preferences.enabled || !rate.reserve(now) {
            return None;
        }
        let id = local
            .preferences
            .installation_id
            .get_or_insert_with(|| Uuid::new_v4().to_string())
            .clone();
        if failure {
            local.preferences.failure_last_attempt_unix_ms = Some(wall);
        } else {
            local.preferences.last_attempt_unix_ms = Some(wall);
        }
        // Persist before dispatch, including failed attempts, to bound restarts.
        write_preferences(path, &local.preferences).ok()?;
        let body = serde_json::to_vec(&Payload {
            event_id: Uuid::new_v4().to_string(),
            installation_id: &id,
            version,
            channel: "prod",
            platform,
            event,
        })
        .ok()?;
        (body.len() <= MAX_BYTES).then_some(Prepared {
            body,
            preference_generation: generation,
        })
    }
    fn still_allowed(&self, prepared: &Prepared) -> bool {
        self.0.runtime.lock().is_ok_and(|runtime| {
            runtime.preference_generation == prepared.preference_generation
                && runtime
                    .local
                    .as_ref()
                    .is_some_and(|local| local.preferences.enabled)
        })
    }
    fn sync(&self, app: tauri::AppHandle) {
        let allowed = available(&app);
        let signed_in = connected(&app);
        if !allowed || !signed_in {
            self.0.changed.notify_waiters();
            return;
        }
        let Ok(path) = storage_path(&app) else { return };
        let Ok(status) = self.status(&path, allowed) else {
            return;
        };
        let should_start = self.0.runtime.lock().is_ok_and(|mut runtime| {
            runtime.request_worker(allowed, signed_in, status.enabled || status.account_enabled)
        });
        if !should_start {
            return;
        }
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                state.send(&app, Event::Heartbeat).await;
                tokio::select! {
                    _ = tokio::time::sleep(INTERVAL) => {},
                    _ = state.0.changed.notified() => {},
                }
            }
        });
    }
    async fn send(&self, app: &tauri::AppHandle, event: Event) {
        tokio::join!(self.send_anonymous(app, event), self.send_account(app));
    }
    async fn send_account(&self, app: &tauri::AppHandle) {
        let Some(wall) = wall_now() else { return };
        let Ok(path) = storage_path(app) else { return };
        let Some(platform) = platform_key() else {
            return;
        };
        let Some(generation) = self.prepare_account(
            &path,
            available(app) && connected(app),
            wall,
            Instant::now(),
        ) else {
            return;
        };
        if !self.account_still_allowed(generation) || !connected(app) {
            return;
        }
        let Some(account) = app.try_state::<crate::commands::OsirAccountState>() else {
            return;
        };
        let version = app.package_info().version.to_string();
        let metadata = crate::services::osir::client::ClientActivityMetadata {
            version: &version,
            channel: "prod",
            platform,
        };
        // One request under the existing account budget; no refresh or retries.
        tokio::select! {
            _ = account.auth.record_client_activity(&metadata) => {},
            _ = self.0.changed.notified() => {},
        }
    }
    async fn send_anonymous(&self, app: &tauri::AppHandle, event: Event) {
        let Some(wall) = wall_now() else { return };
        let Ok(path) = storage_path(app) else { return };
        let Some(platform) = platform_key() else {
            return;
        };
        let Some(prepared) = self.prepare(
            &path,
            available(app) && connected(app),
            event,
            &app.package_info().version.to_string(),
            platform,
            wall,
            Instant::now(),
        ) else {
            return;
        };
        // Reuse only compiled deployment origin; never its authenticated client.
        let Some(endpoint) =
            trusted_endpoint(&crate::services::osir::auth::AuthConfig::default().account_api_base)
        else {
            return;
        };
        let Ok(http) = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .user_agent("OSIR-Client")
            .build()
        else {
            return;
        };
        if !self.still_allowed(&prepared) || !connected(app) {
            return;
        }
        let request = async {
            let Ok(mut response) = http
                .post(endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(prepared.body)
                .send()
                .await
            else {
                return;
            };
            if response.status() != reqwest::StatusCode::OK {
                return;
            }
            let mut body = Vec::new();
            while let Ok(Some(chunk)) = response.chunk().await {
                if body.len() + chunk.len() > MAX_BYTES {
                    return;
                }
                body.extend_from_slice(&chunk);
            }
            // No response strings, request identifiers or payload are logged.
            #[derive(Deserialize)]
            struct Accepted {
                accepted: bool,
                duplicate: bool,
            }
            if let Ok(reply) = serde_json::from_slice::<Accepted>(&body) {
                let _ = (reply.accepted, reply.duplicate);
            }
        };
        tokio::select! {
            _ = request => {},
            _ = self.0.changed.notified() => {},
        }
    }
}

#[tauri::command]
pub fn osir_client_telemetry_status(
    app: tauri::AppHandle,
    state: tauri::State<'_, ClientTelemetryState>,
) -> Result<ClientTelemetryStatus, String> {
    state.status(&storage_path(&app)?, available(&app))
}
#[tauri::command]
pub fn osir_client_telemetry_set_enabled(
    app: tauri::AppHandle,
    state: tauri::State<'_, ClientTelemetryState>,
    enabled: bool,
) -> Result<ClientTelemetryStatus, String> {
    let path = storage_path(&app)?;
    state.set_enabled(&path, enabled)?;
    state.sync(app.clone());
    state.status(&path, available(&app))
}
#[tauri::command]
pub fn osir_client_telemetry_sync(
    app: tauri::AppHandle,
    state: tauri::State<'_, ClientTelemetryState>,
) {
    state.sync(app);
}

#[tauri::command]
pub fn osir_client_activity_set_enabled(
    app: tauri::AppHandle,
    state: tauri::State<'_, ClientTelemetryState>,
    enabled: bool,
) -> Result<ClientTelemetryStatus, String> {
    let path = storage_path(&app)?;
    state.set_account_enabled(&path, enabled)?;
    state.sync(app.clone());
    state.status(&path, available(&app))
}

/// Native updater integration only: call with the running binary's actual
/// result, never an announced/downloaded version or a renderer event string.
pub enum ClientUpdateResult {
    Installed,
    Failed,
}
pub fn record_credential_failure(app: tauri::AppHandle) {
    record_failure(app, Event::CredentialFailed);
}
fn record_failure(app: tauri::AppHandle, event: Event) {
    let Some(state) = app.try_state::<ClientTelemetryState>() else {
        return;
    };
    let state = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        state.send(&app, event).await;
    });
}
#[tauri::command]
pub fn osir_client_report_frontend_failure(app: tauri::AppHandle) {
    // No error string, stack, filename or renderer-chosen event crosses IPC.
    record_failure(app, Event::FrontendFailed);
}
pub fn record_update_result(app: tauri::AppHandle, result: ClientUpdateResult) {
    let Some(state) = app.try_state::<ClientTelemetryState>() else {
        return;
    };
    let state = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        state
            .send(
                &app,
                match result {
                    ClientUpdateResult::Installed => Event::UpdateInstalled,
                    ClientUpdateResult::Failed => Event::UpdateFailed,
                },
            )
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_have_a_separate_persisted_limit_and_no_error_details() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.json");
        let state = ClientTelemetryState::default();
        let now = Instant::now();
        assert!(state
            .prepare(
                &path,
                true,
                Event::Heartbeat,
                "0.1.3",
                "darwin-aarch64",
                1000,
                now
            )
            .is_some());
        let failure = state
            .prepare(
                &path,
                true,
                Event::CredentialFailed,
                "0.1.3",
                "darwin-aarch64",
                1000,
                now,
            )
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&failure.body).unwrap();
        assert_eq!(body["event"], "credential_failed");
        assert_eq!(body.as_object().unwrap().len(), 6);
        assert!(state
            .prepare(
                &path,
                true,
                Event::FrontendFailed,
                "0.1.3",
                "darwin-aarch64",
                1001,
                now
            )
            .is_none());
        assert!(ClientTelemetryState::default()
            .prepare(
                &path,
                true,
                Event::FrontendFailed,
                "0.1.3",
                "darwin-aarch64",
                1001,
                now
            )
            .is_none());
    }

    #[test]
    fn legacy_anonymous_opt_in_never_grants_account_linking() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.json");
        std::fs::write(&path, br#"{"schema_version":1,"enabled":true,"installation_id":null,"last_attempt_unix_ms":null}"#).unwrap();
        let state = ClientTelemetryState::default();
        assert!(state.status(&path, true).unwrap().enabled);
        assert!(!state.status(&path, true).unwrap().account_enabled);
        assert!(state
            .prepare_account(&path, true, 1000, Instant::now())
            .is_none());
    }

    #[test]
    fn account_reports_have_independent_consent_rate_and_no_installation_id() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.json");
        let state = ClientTelemetryState::default();
        state.set_account_enabled(&path, true).unwrap();
        let now = Instant::now();
        assert!(state.prepare_account(&path, false, 1000, now).is_none());
        let generation = state.prepare_account(&path, true, 1000, now).unwrap();
        assert!(state.account_still_allowed(generation));
        assert!(state.prepare_account(&path, true, 1001, now).is_none());
        assert!(read_preferences(&path).unwrap().installation_id.is_none());
        assert!(read_preferences(&path).unwrap().enabled);
        assert!(state
            .prepare_account(&path, true, 901000, now + INTERVAL)
            .is_some());
        state.set_account_enabled(&path, false).unwrap();
        assert!(!state.account_still_allowed(generation));
        assert!(state
            .prepare_account(&path, true, 1801000, now + INTERVAL * 2)
            .is_none());
        let metadata = crate::services::osir::client::ClientActivityMetadata {
            version: "1.2.3",
            channel: "prod",
            platform: "darwin-aarch64",
        };
        let body = serde_json::to_value(metadata).unwrap();
        assert_eq!(body.as_object().unwrap().len(), 3);
        assert_eq!(body["version"], "1.2.3");
        assert!(body.get("installation_id").is_none());
        assert!(body.get("email").is_none());
    }

    #[test]
    fn only_compiled_production_release_with_updater_is_eligible() {
        assert!(production_release("com.osir.client", true, true, "1.2.3"));
        for identifier in [
            "com.osir.client.dev",
            "com.osir.client.beta",
            "com.ccswitch.desktop",
        ] {
            assert!(!production_release(identifier, true, true, "1.2.3"));
        }
        assert!(!production_release("com.osir.client", false, true, "1.2.3"));
        assert!(!production_release("com.osir.client", true, false, "1.2.3"));
        assert!(!production_release(
            "com.osir.client",
            true,
            true,
            "1.2.3-beta.1"
        ));
        assert!(!production_release(
            "com.osir.client",
            true,
            true,
            "invalid"
        ));
    }

    #[test]
    fn endpoint_reuses_trusted_account_base_without_credentials_or_redirect_target() {
        assert_eq!(
            trusted_endpoint("https://osirclaw.com/api/v1/")
                .unwrap()
                .as_str(),
            "https://osirclaw.com/api/v1/osir-client/events"
        );
        for base in [
            "http://osirclaw.com/api/v1/",
            "https://user:pass@osirclaw.com/api/v1/",
            "https://osirclaw.com/api/v1/?token=secret",
            "https://osirclaw.com/api/v1/#fragment",
            "https://osirclaw.com:444/api/v1/",
            "https://osirclaw.com/v1/",
        ] {
            assert!(trusted_endpoint(base).is_none());
        }
    }

    #[test]
    fn monotonic_limit_cannot_be_bypassed_by_forward_or_backward_wall_clock_changes() {
        let now = Instant::now();
        let mut rate = RateLimit::new(None, 1000, now);
        assert!(rate.reserve(now));
        assert!(!rate.reserve(now + INTERVAL - Duration::from_millis(1)));
        assert!(rate.reserve(now + INTERVAL));
        assert!(!rate.reserve(now + INTERVAL));
    }

    #[test]
    fn persisted_cooldown_survives_restart_and_clock_rollback_is_bounded() {
        let now = Instant::now();
        let mut rate = RateLimit::new(Some(1_000_000), 1_000_100, now);
        assert!(!rate.reserve(now));
        assert!(rate.reserve(now + INTERVAL));
        let mut rollback = RateLimit::new(Some(u64::MAX), 1000, now);
        assert!(!rollback.reserve(now));
        assert!(!rollback.reserve(now + INTERVAL - Duration::from_millis(1)));
        assert!(rollback.reserve(now + INTERVAL));
    }

    #[test]
    fn worker_starts_at_most_once_and_only_after_real_session_and_preference_gates() {
        let mut runtime = Runtime::default();
        assert!(!runtime.request_worker(false, true, true));
        assert!(!runtime.request_worker(true, false, true));
        assert!(!runtime.request_worker(true, true, false));
        assert!(runtime.request_worker(true, true, true));
        assert!(!runtime.request_worker(true, true, true));
    }

    #[test]
    fn status_read_never_creates_installation_id_or_returns_it_to_renderer() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.json");
        let state = ClientTelemetryState::default();
        let status = serde_json::to_value(state.status(&path, false).unwrap()).unwrap();
        assert_eq!(status.as_object().unwrap().len(), 4);
        assert_eq!(status["accountEnabled"], false);
        assert_eq!(status["enabled"], true);
        assert_eq!(status["available"], false);
        assert!(status.get("installationId").is_none());
        assert!(!path.exists());
    }

    #[test]
    fn disabled_or_nonproduction_sends_do_not_create_identifiers_or_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.json");
        let state = ClientTelemetryState::default();
        assert!(state
            .prepare(
                &path,
                false,
                Event::Heartbeat,
                "1.2.3",
                "darwin-aarch64",
                1000,
                Instant::now()
            )
            .is_none());
        assert!(!path.exists());
        state.set_enabled(&path, false).unwrap();
        assert!(state
            .prepare(
                &path,
                true,
                Event::Heartbeat,
                "1.2.3",
                "darwin-aarch64",
                1000,
                Instant::now()
            )
            .is_none());
        assert!(read_preferences(&path).unwrap().installation_id.is_none());
    }

    #[test]
    fn payload_is_small_exactly_allowlisted_and_failed_attempts_share_one_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.json");
        let state = ClientTelemetryState::default();
        state.set_enabled(&path, true).unwrap();
        let now = Instant::now();
        let prepared = state
            .prepare(
                &path,
                true,
                Event::Heartbeat,
                "1.2.3",
                "darwin-aarch64",
                1000,
                now,
            )
            .unwrap();
        assert!(prepared.body.len() <= MAX_BYTES);
        let payload: serde_json::Value = serde_json::from_slice(&prepared.body).unwrap();
        let mut keys = payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "channel",
                "event",
                "event_id",
                "installation_id",
                "platform",
                "version"
            ]
        );
        assert_eq!(payload["channel"], "prod");
        assert_eq!(payload["event"], "heartbeat");
        assert_eq!(
            Uuid::parse_str(payload["installation_id"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            4
        );
        for event in [
            Event::Heartbeat,
            Event::UpdateInstalled,
            Event::UpdateFailed,
        ] {
            assert!(state
                .prepare(&path, true, event, "1.2.3", "darwin-aarch64", u64::MAX, now)
                .is_none());
        }
        assert_eq!(
            read_preferences(&path).unwrap().last_attempt_unix_ms,
            Some(1000)
        );
        let next = state
            .prepare(
                &path,
                true,
                Event::UpdateFailed,
                "1.2.3",
                "darwin-aarch64",
                901_000,
                now + INTERVAL,
            )
            .unwrap();
        let next: serde_json::Value = serde_json::from_slice(&next.body).unwrap();
        assert_eq!(next["installation_id"], payload["installation_id"]);
        assert_ne!(next["event_id"], payload["event_id"]);
    }

    #[test]
    fn opt_out_invalidates_already_prepared_dispatch_and_survives_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.json");
        let state = ClientTelemetryState::default();
        state.set_enabled(&path, true).unwrap();
        let now = Instant::now();
        let prepared = state
            .prepare(
                &path,
                true,
                Event::Heartbeat,
                "1.2.3",
                "darwin-aarch64",
                1000,
                now,
            )
            .unwrap();
        assert!(state.still_allowed(&prepared));
        state.set_enabled(&path, false).unwrap();
        assert!(!state.still_allowed(&prepared));
        assert!(
            !ClientTelemetryState::default()
                .status(&path, true)
                .unwrap()
                .enabled
        );
        assert!(state
            .prepare(
                &path,
                true,
                Event::Heartbeat,
                "1.2.3",
                "darwin-aarch64",
                901_000,
                now + INTERVAL
            )
            .is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn malformed_preferences_fail_closed_without_resetting_a_saved_opt_out() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telemetry.json");
        for bytes in [b"invalid".as_slice(), b"{\"schema_version\":1,\"enabled\":false,\"installation_id\":\"host-name\",\"last_attempt_unix_ms\":null}"] {
            std::fs::write(&path, bytes).unwrap();
            let state = ClientTelemetryState::default();
            assert!(state.status(&path, true).is_err());
            assert!(state.prepare(&path, true, Event::Heartbeat, "1.2.3", "darwin-aarch64", 1000, Instant::now()).is_none());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }
}
