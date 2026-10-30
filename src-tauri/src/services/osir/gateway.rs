//! Fixed native model discovery. Cache identity is trusted binding metadata,
//! never a credential or credential digest. No endpoint/header input from IPC.
use super::{
    key_vault::{NativeKeySecret, GATEWAY_ORIGIN},
    request_budget::{RequestBudget, RequestClass},
};
use crate::{provider::ManagedBinding, services::model_fetch::FetchedModel};
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_CACHE_KEYS: usize = 64;
const CACHE_TTL: Duration = Duration::from_secs(60);
const ERROR_TTL: Duration = Duration::from_secs(2);
const UNAVAILABLE: &str = "模型目录暂时不可用";

#[derive(Deserialize)]
struct Catalog {
    data: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    id: String,
}

// Full immutable metadata (including the random vault reference) distinguishes
// account, key, group, tool and rotations without retaining any secret index.
#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey(String);
impl CacheKey {
    fn from_account_key(
        account_origin: &str,
        account_id: &str,
        key_id: i64,
        group_id: Option<i64>,
        generation: u64,
    ) -> Result<Self, String> {
        if account_origin != "https://osirclaw.com"
            || account_id.trim().is_empty()
            || account_id.len() > 256
            || account_id.chars().any(char::is_control)
            || key_id <= 0
            || group_id.is_some_and(|id| id <= 0)
        {
            return Err(UNAVAILABLE.into());
        }
        serde_json::to_string(&(
            "account-key-v1",
            account_origin,
            account_id,
            key_id,
            group_id,
            generation,
        ))
        .map(Self)
        .map_err(|_| UNAVAILABLE.into())
    }

    fn from_binding(binding: &ManagedBinding) -> Result<Self, String> {
        if binding.source != crate::provider::AuthBindingSource::ManagedAccount
            || binding.binding.account_origin != "https://osirclaw.com"
            || binding.binding.gateway_origin != GATEWAY_ORIGIN
            || binding.binding.account_id.trim().is_empty()
            || binding.binding.key_id <= 0
            || binding.binding.group_id.is_some_and(|id| id <= 0)
        {
            return Err(UNAVAILABLE.into());
        }
        // masked_key is display text and is deliberately not cached/indexed.
        serde_json::to_string(&(&binding.binding, &binding.reference))
            .map(Self)
            .map_err(|_| UNAVAILABLE.into())
    }
}
struct Cached {
    until: Instant,
    result: Result<Vec<FetchedModel>, String>,
}
#[derive(Default)]
struct EntryState {
    cached: Option<Cached>,
}
#[derive(Default)]
struct CatalogCache {
    entries: Mutex<HashMap<CacheKey, Arc<tokio::sync::Mutex<EntryState>>>>,
}
impl CatalogCache {
    fn entry(&self, key: CacheKey) -> Result<Arc<tokio::sync::Mutex<EntryState>>, String> {
        let mut entries = self.entries.lock().map_err(|_| UNAVAILABLE)?;
        if let Some(entry) = entries.get(&key) {
            return Ok(entry.clone());
        }
        // Never evict an entry held by an active leader/waiter: that would allow
        // a second HTTP leader for the same identity.
        if entries.len() >= MAX_CACHE_KEYS {
            let idle = entries
                .iter()
                .find(|(_, entry)| Arc::strong_count(entry) == 1)
                .map(|(key, _)| key.clone())
                .ok_or(UNAVAILABLE)?;
            entries.remove(&idle);
        }
        let entry = Arc::new(tokio::sync::Mutex::new(EntryState::default()));
        entries.insert(key, entry.clone());
        Ok(entry)
    }
}
fn cache() -> &'static CatalogCache {
    static CACHE: OnceLock<CatalogCache> = OnceLock::new();
    CACHE.get_or_init(CatalogCache::default)
}
pub(crate) fn gateway_budget() -> &'static RequestBudget {
    static BUDGET: OnceLock<RequestBudget> = OnceLock::new();
    BUDGET.get_or_init(RequestBudget::default)
}

fn project(bytes: &[u8], secret: &str) -> Result<Vec<FetchedModel>, String> {
    let catalog: Catalog = serde_json::from_slice(bytes).map_err(|_| "模型目录格式不兼容")?;
    if catalog.data.len() > 10000 {
        return Err("模型目录超出容量限制".into());
    }
    let mut ids = BTreeSet::new();
    for entry in catalog.data {
        if entry.id.is_empty()
            || entry.id.len() > 256
            || entry.id.chars().any(char::is_control)
            || entry.id.contains(secret)
            || entry.id.trim() != entry.id
        {
            return Err("模型目录包含无效标识".into());
        }
        ids.insert(entry.id);
    }
    Ok(ids
        .into_iter()
        .map(|id| FetchedModel { id, owned_by: None })
        .collect())
}

/// Compatibility entry: all calls share the process gateway request budget.
/// No safe identity is supplied here, so no caching/deduplication is attempted.
pub async fn models(secret: &NativeKeySecret) -> Result<Vec<FetchedModel>, String> {
    models_with_budget(secret, gateway_budget()).await
}

/// Share the account-key catalog read with the later native save validation.
/// The cache key contains verified account/key metadata and session generation,
/// never the bearer token or a credential digest.
pub async fn models_for_account_key(
    secret: &NativeKeySecret,
    account_origin: &str,
    account_id: &str,
    key_id: i64,
    group_id: Option<i64>,
    generation: u64,
    budget: &RequestBudget,
) -> Result<Vec<FetchedModel>, String> {
    let key = CacheKey::from_account_key(account_origin, account_id, key_id, group_id, generation)?;
    let entry = cache().entry(key)?;
    cached_models(&entry, secret, || models_with_budget(secret, budget)).await
}

/// Native caller must obtain binding AND secret from the same verified vault
/// load. Never construct binding from renderer input. Pass the account client's
/// existing budget to share admission/cooldowns with auth and account requests.
/// Replacing the immutable vault reference automatically isolates cached results.
pub async fn models_for_binding(
    secret: &NativeKeySecret,
    binding: &ManagedBinding,
    budget: &RequestBudget,
) -> Result<Vec<FetchedModel>, String> {
    let entry = cache().entry(CacheKey::from_binding(binding)?)?;
    cached_models(&entry, secret, || models_with_budget(secret, budget)).await
}

async fn cached_models<F, Fut>(
    entry: &tokio::sync::Mutex<EntryState>,
    secret: &NativeKeySecret,
    fetch: F,
) -> Result<Vec<FetchedModel>, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<FetchedModel>, String>>,
{
    // The lock covers HTTP + projection. Cancellation releases it; no detached
    // work owns the secret, and the next waiter may become the leader.
    let mut state = entry.lock().await;
    if let Some(cached) = &state.cached {
        if cached.until > Instant::now() {
            // Recheck against this call's secret even on a cache hit.
            return safe_result(cached.result.clone(), secret);
        }
    }
    let result = safe_result(fetch().await, secret);
    let ttl = if result.is_ok() { CACHE_TTL } else { ERROR_TTL };
    state.cached = Some(Cached {
        until: Instant::now() + ttl,
        result: result.clone(),
    });
    result
}
fn safe_result(
    result: Result<Vec<FetchedModel>, String>,
    secret: &NativeKeySecret,
) -> Result<Vec<FetchedModel>, String> {
    match result {
        Ok(models)
            if models
                .iter()
                .all(|model| !model.id.contains(secret.expose()) && model.owned_by.is_none()) =>
        {
            Ok(models)
        }
        Ok(_) => Err(UNAVAILABLE.into()),
        // Never cache or echo arbitrary transport/projection error details.
        Err(_) => Err(UNAVAILABLE.into()),
    }
}

/// One charged attempt per actual HTTP request. No automatic retry. Hold the
/// guard through bounded body reading/projection; failures/cancellation count.
pub async fn models_with_budget(
    secret: &NativeKeySecret,
    budget: &RequestBudget,
) -> Result<Vec<FetchedModel>, String> {
    let http = http_client().build().map_err(|_| UNAVAILABLE)?;
    let request = http
        .get(format!("{GATEWAY_ORIGIN}/v1/models"))
        .bearer_auth(secret.expose())
        .build()
        .map_err(|_| UNAVAILABLE)?;
    execute_models(&http, request, secret, budget).await
}

fn http_client() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
}

// Private transport seam: production always constructs the fixed gateway URL
// above. Tests exercise the same policy against an isolated loopback server.
async fn execute_models(
    http: &reqwest::Client,
    request: reqwest::Request,
    secret: &NativeKeySecret,
    budget: &RequestBudget,
) -> Result<Vec<FetchedModel>, String> {
    let guard = budget
        .admit(RequestClass::Standard)
        .await
        .map_err(|e| e.to_string())?;
    let mut response = http.execute(request).await.map_err(|_| UNAVAILABLE)?;
    guard.observe_response(
        response.status().as_u16(),
        retry_after(response.headers(), chrono::Utc::now()),
    );
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 | 403 => "密钥无权读取模型，请检查密钥状态与分组权限",
            429 => "模型目录请求过于频繁，请稍后重试",
            _ => UNAVAILABLE,
        }
        .into());
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err("模型目录超出容量限制".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "模型目录读取未完成")? {
        if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err("模型目录超出容量限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let models = project(&bytes, secret.expose())?;
    guard.finish_success();
    Ok(models)
}

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
        .ok()?
        .with_timezone(&chrono::Utc);
    Some(Duration::from_secs(
        date.signed_duration_since(now)
            .num_seconds()
            .max(0)
            .min(300) as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn transport_fixture(
        response: String,
        budget: &RequestBudget,
    ) -> Result<Vec<FetchedModel>, String> {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).unwrap(), 1);
                headers.push(byte[0]);
                assert!(headers.len() < 16384);
            }
            socket.write_all(response.as_bytes()).unwrap();
        });
        let http = http_client()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let secret = secret();
        let request = http
            .get(format!("http://{address}/v1/models"))
            .bearer_auth(secret.expose())
            .build()
            .unwrap();
        let result = execute_models(&http, request, &secret, budget).await;
        worker.join().unwrap();
        result
    }

    #[tokio::test]
    async fn transport_refuses_redirect_without_contacting_destination() {
        let destination = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let budget = RequestBudget::default();
        let response = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{}/capture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", destination.local_addr().unwrap());
        assert_eq!(
            transport_fixture(response, &budget).await.unwrap_err(),
            UNAVAILABLE
        );
        assert_eq!(
            destination.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(budget.failed_attempts(), 1);
    }

    #[tokio::test]
    async fn transport_failure_cools_shared_budget_and_releases_concurrency() {
        use super::super::request_budget::AdmissionError;
        let budget = RequestBudget::default();
        assert!(transport_fixture("HTTP/1.1 503 Service Unavailable\r\nRetry-After: 30\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(), &budget).await.is_err());
        assert_eq!(budget.failed_attempts(), 1);
        assert!(matches!(
            budget.clone().try_admit(RequestClass::Standard),
            Err(AdmissionError::Cooldown { .. })
        ));

        let budget = RequestBudget::default();
        assert!(transport_fixture(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_BODY + 1
            ),
            &budget
        )
        .await
        .is_err());
        assert_eq!(budget.failed_attempts(), 1);
        let guards: Vec<_> = (0..4)
            .map(|_| budget.try_admit(RequestClass::Standard).unwrap())
            .collect();
        assert_eq!(guards.len(), 4);
    }

    #[tokio::test]
    async fn transport_projects_success_and_rejects_truncated_body() {
        let budget = RequestBudget::default();
        let body = r#"{"data":[{"id":"model-a"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        assert_eq!(
            transport_fixture(response, &budget).await.unwrap()[0].id,
            "model-a"
        );
        assert_eq!(budget.failed_attempts(), 0);
        assert_eq!(
            transport_fixture(
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}".into(),
                &budget
            )
            .await
            .unwrap_err(),
            "模型目录读取未完成"
        );
        assert_eq!(budget.failed_attempts(), 1);
    }
    #[test]
    fn projection_deduplicates_and_discards_unknown_wire_fields() {
        let result = project(br#"{"data":[{"id":"model-b","key":"fixture-secret"},{"id":"model-a"},{"id":"model-b"}]}"#, "fixture-secret").unwrap();
        assert_eq!(
            result.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["model-a", "model-b"]
        );
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("fixture-secret"));
    }
    #[test]
    fn projection_rejects_sensitive_or_malformed_ids_without_echo() {
        for id in ["", " model", "bad\nmodel", "fixture-secret"] {
            let bytes = serde_json::to_vec(&serde_json::json!({"data":[{"id":id}]})).unwrap();
            let error = project(&bytes, "fixture-secret").unwrap_err();
            assert!(!error.contains("fixture-secret"));
        }
    }

    fn binding() -> ManagedBinding {
        use crate::provider::AuthBindingSource;
        use crate::services::osir::key_vault::{KeyBinding, KeyTool};
        ManagedBinding {
            source: AuthBindingSource::ManagedAccount,
            binding: KeyBinding {
                account_origin: "https://osirclaw.com".into(),
                account_id: "fixture-account".into(),
                key_id: 1,
                group_id: Some(2),
                tool: KeyTool::Codex,
                provider_id: "fixture-provider".into(),
                gateway_origin: GATEWAY_ORIGIN.into(),
            },
            reference: serde_json::from_value(serde_json::json!({
                "version": 1, "id": "550e8400-e29b-41d4-a716-446655440000"
            }))
            .unwrap(),
            masked_key: "••••abcd".into(),
        }
    }

    fn secret() -> NativeKeySecret {
        NativeKeySecret::new("fixture-gateway-secret".into()).unwrap()
    }

    fn catalog(id: &str) -> Vec<FetchedModel> {
        vec![FetchedModel {
            id: id.into(),
            owned_by: None,
        }]
    }

    // Poll exactly once so contention/cancellation tests establish that the
    // leader owns the lock and the follower is waiting, without timing sleeps.
    async fn assert_pending<F: std::future::Future>(future: std::pin::Pin<&mut F>) {
        let mut future = future;
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn same_binding_coalesces_in_flight_fetch_and_reuses_success() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = CatalogCache::default();
        let binding = binding();
        let entry = cache
            .entry(CacheKey::from_binding(&binding).unwrap())
            .unwrap();
        let same = cache
            .entry(CacheKey::from_binding(&binding).unwrap())
            .unwrap();
        assert!(Arc::ptr_eq(&entry, &same));
        let secret = secret();
        let calls = AtomicUsize::new(0);
        let release = tokio::sync::Notify::new();
        let mut leader = Box::pin(cached_models(&entry, &secret, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            release.notified().await;
            Ok(catalog("leader-model"))
        }));
        assert_pending(leader.as_mut()).await;
        let mut followers = (0..8)
            .map(|_| {
                Box::pin(cached_models(&same, &secret, || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(catalog("unexpected-follower-model"))
                }))
            })
            .collect::<Vec<_>>();
        for follower in &mut followers {
            assert_pending(follower.as_mut()).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.notify_one();
        assert_eq!(leader.await.unwrap()[0].id, "leader-model");
        for follower in followers {
            let models = tokio::time::timeout(Duration::from_secs(1), follower)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(models[0].id, "leader-model");
        }
        let hit = cached_models(&entry, &secret, || async {
            panic!("cache hit must not invoke fetch")
        })
        .await
        .unwrap();
        assert_eq!(hit[0].id, "leader-model");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failure_is_redacted_and_cooled_down_then_expired_entry_retries() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let entry = tokio::sync::Mutex::new(EntryState::default());
        let secret = secret();
        let calls = AtomicUsize::new(0);
        let before = Instant::now();
        let failure = cached_models(&entry, &secret, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(format!("transport leaked {}", secret.expose()))
        })
        .await
        .unwrap_err();
        assert_eq!(failure, UNAVAILABLE);
        {
            let state = entry.lock().await;
            let cached = state.cached.as_ref().unwrap();
            assert!(cached.until >= before + ERROR_TTL);
            assert!(cached.until <= Instant::now() + ERROR_TTL);
            assert_eq!(cached.result.as_ref().unwrap_err(), UNAVAILABLE);
        }
        assert_eq!(
            cached_models(&entry, &secret, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(catalog("too-early"))
            })
            .await
            .unwrap_err(),
            UNAVAILABLE
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Production uses std::time::Instant. Expire only this local fixture,
        // avoiding real sleeps or Tokio's optional test-util clock feature.
        entry.lock().await.cached.as_mut().unwrap().until = Instant::now() - ERROR_TTL;
        let before = Instant::now();
        let recovered = cached_models(&entry, &secret, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(catalog("recovered"))
        })
        .await
        .unwrap();
        assert_eq!(recovered[0].id, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(entry.lock().await.cached.as_ref().unwrap().until >= before + CACHE_TTL);
    }

    #[tokio::test]
    async fn account_key_catalog_isolates_session_account_key_and_group() {
        let cache = CatalogCache::default();
        let identity = |account: &str, key, group, generation| {
            CacheKey::from_account_key("https://osirclaw.com", account, key, group, generation)
                .unwrap()
        };
        let baseline = identity("account-a", 1, Some(2), 3);
        let first = cache.entry(baseline.clone()).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.entry(baseline).unwrap()));
        let secret = secret();
        cached_models(&first, &secret, || async { Ok(catalog("account-a-model")) })
            .await
            .unwrap();
        for key in [
            identity("account-b", 1, Some(2), 3),
            identity("account-a", 4, Some(2), 3),
            identity("account-a", 1, Some(5), 3),
            identity("account-a", 1, None, 3),
            identity("account-a", 1, Some(2), 6),
        ] {
            let separate = cache.entry(key).unwrap();
            assert!(!Arc::ptr_eq(&first, &separate));
            assert!(separate.lock().await.cached.is_none());
        }
        assert!(CacheKey::from_account_key(
            "https://untrusted.invalid",
            "account-a",
            1,
            Some(2),
            3
        )
        .is_err());
    }

    #[tokio::test]
    async fn binding_dimensions_and_vault_rotation_have_independent_catalogs() {
        use crate::services::osir::key_vault::KeyTool;
        let cache = CatalogCache::default();
        let baseline = binding();
        let first = cache
            .entry(CacheKey::from_binding(&baseline).unwrap())
            .unwrap();
        let secret = secret();
        cached_models(&first, &secret, || async { Ok(catalog("baseline")) })
            .await
            .unwrap();
        for dimension in 0..7 {
            let mut different = baseline.clone();
            match dimension {
                0 => different.binding.account_id = "other-account".into(),
                1 => different.binding.key_id += 1,
                2 => different.binding.group_id = Some(3),
                3 => different.binding.group_id = None,
                4 => different.binding.tool = KeyTool::GeminiCli,
                5 => different.binding.provider_id = "other-row".into(),
                _ => {
                    different.reference = serde_json::from_value(serde_json::json!({
                        "version": 1, "id": "550e8400-e29b-41d4-a716-446655440001"
                    }))
                    .unwrap()
                }
            }
            let entry = cache
                .entry(CacheKey::from_binding(&different).unwrap())
                .unwrap();
            assert!(!Arc::ptr_eq(&first, &entry));
            let expected = format!("identity-{dimension}");
            let result = cached_models(&entry, &secret, || async { Ok(catalog(&expected)) })
                .await
                .unwrap();
            assert_eq!(result[0].id, expected);
        }
        let mut display_only = baseline;
        display_only.masked_key = "••••wxyz".into();
        let same = cache
            .entry(CacheKey::from_binding(&display_only).unwrap())
            .unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert_eq!(cache.entries.lock().unwrap().len(), 8);
    }

    #[tokio::test]
    async fn cancellation_releases_leader_lock_and_waiter_becomes_leader() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let entry = tokio::sync::Mutex::new(EntryState::default());
        let secret = secret();
        let calls = AtomicUsize::new(0);
        let mut leader = Box::pin(cached_models(&entry, &secret, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
            Ok(catalog("cancelled"))
        }));
        assert_pending(leader.as_mut()).await;
        let mut waiter = Box::pin(cached_models(&entry, &secret, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(catalog("replacement"))
        }));
        assert_pending(waiter.as_mut()).await;
        drop(leader);
        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result[0].id, "replacement");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            entry
                .try_lock()
                .unwrap()
                .cached
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()[0]
                .id,
            "replacement"
        );
    }

    #[tokio::test]
    async fn cancelling_a_queued_waiter_does_not_block_later_waiters() {
        let entry = tokio::sync::Mutex::new(EntryState::default());
        let secret = secret();
        let release = tokio::sync::Notify::new();
        let mut leader = Box::pin(cached_models(&entry, &secret, || async {
            release.notified().await;
            Ok(catalog("leader"))
        }));
        assert_pending(leader.as_mut()).await;
        let mut cancelled = Box::pin(cached_models(&entry, &secret, || async {
            panic!("queued waiter must not fetch")
        }));
        assert_pending(cancelled.as_mut()).await;
        let mut surviving = Box::pin(cached_models(&entry, &secret, || async {
            panic!("surviving waiter should reuse leader result")
        }));
        assert_pending(surviving.as_mut()).await;
        drop(cancelled);
        release.notify_one();
        leader.await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), surviving)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result[0].id, "leader");
    }

    #[tokio::test]
    async fn capacity_rejects_when_all_entries_held_and_evicts_only_idle_entry() {
        let cache = CatalogCache::default();
        let mut held = Vec::new();
        let mut keys = Vec::new();
        for index in 0..MAX_CACHE_KEYS {
            let mut binding = binding();
            binding.binding.provider_id = format!("provider-{index}");
            let key = CacheKey::from_binding(&binding).unwrap();
            held.push(cache.entry(key.clone()).unwrap());
            keys.push(key);
        }
        let mut extra = binding();
        extra.binding.provider_id = "overflow".into();
        let extra_key = CacheKey::from_binding(&extra).unwrap();
        assert_eq!(cache.entry(extra_key.clone()).err().unwrap(), UNAVAILABLE);
        assert_eq!(cache.entries.lock().unwrap().len(), MAX_CACHE_KEYS);
        // A hit must work at capacity and preserve the active entry's identity.
        let same = cache.entry(keys[0].clone()).unwrap();
        assert!(Arc::ptr_eq(&held[0], &same));
        drop(same);
        let evicted_key = keys.pop().unwrap();
        drop(held.pop().unwrap());
        let added = cache.entry(extra_key).unwrap();
        {
            let entries = cache.entries.lock().unwrap();
            assert_eq!(entries.len(), MAX_CACHE_KEYS);
            assert!(!entries.contains_key(&evicted_key));
            assert!(Arc::ptr_eq(entries.get(&keys[0]).unwrap(), &held[0]));
        }
        let secret = secret();
        assert_eq!(
            cached_models(&added, &secret, || async { Ok(catalog("added")) })
                .await
                .unwrap()[0]
                .id,
            "added"
        );
    }

    #[tokio::test]
    async fn cache_hit_rechecks_current_secret_and_never_returns_sensitive_metadata() {
        let entry = tokio::sync::Mutex::new(EntryState::default());
        let secret = secret();
        cached_models(&entry, &secret, || async {
            Ok(catalog("other-fixture-secret"))
        })
        .await
        .unwrap();
        let rotated_secret = NativeKeySecret::new("other-fixture-secret".into()).unwrap();
        assert_eq!(
            cached_models(&entry, &rotated_secret, || async {
                panic!("hit must recheck locally")
            })
            .await
            .unwrap_err(),
            UNAVAILABLE
        );
        entry.lock().await.cached = None;
        assert_eq!(
            cached_models(&entry, &secret, || async {
                Ok(vec![FetchedModel {
                    id: "model".into(),
                    owned_by: Some(secret.expose().into()),
                }])
            })
            .await
            .unwrap_err(),
            UNAVAILABLE
        );
    }
}
