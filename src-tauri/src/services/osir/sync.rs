//! Shared native coordinator for read-only OSIR requests; instantiate once across windows.
//! Callers MUST supply only sanitized keys and JSON: never tokens, plaintext API
//! keys, prompts or conversation bodies. This module cannot redact arbitrary JSON.
//! Origins must already be trusted and canonical. HTTP authentication, retries,
//! visibility, credential refresh and mutations belong to the calling service.

use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    fmt,
    future::Future,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use tokio::sync::{watch, Semaphore};

pub trait Clock: Send + Sync + 'static {
    /// Monotonic elapsed time; implementations must not move backwards.
    fn now(&self) -> Duration;
}
pub struct SystemClock(Instant);
impl Default for SystemClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

#[derive(Clone, Copy)]
pub struct Config {
    pub max_entries: usize,
    pub max_cache_bytes: usize,
    pub max_response_bytes: usize,
    /// Includes running and queued leaders, even during invalidation cleanup.
    pub max_pending: usize,
    pub max_ttl: Duration,
    /// Account-wide leaders per rolling minute; cache hits/followers cost zero.
    pub max_requests_per_minute: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max_entries: 128,
            max_cache_bytes: 8 * 1024 * 1024,
            max_response_bytes: 1024 * 1024,
            max_pending: 32,
            max_ttl: Duration::from_secs(600),
            max_requests_per_minute: 60,
        }
    }
}
/// Fixed messages only: upstream errors are discarded without formatting/logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncError {
    InvalidInput,
    StaleSession,
    Invalidated,
    QueueFull,
    BudgetExceeded,
    CoolingDown,
    FetchFailed,
    TooLarge,
    WorkerStopped,
}
impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid sync input",
            Self::StaleSession => "inactive sync session",
            Self::Invalidated => "sync request invalidated",
            Self::QueueFull => "sync queue full",
            Self::BudgetExceeded => "account request budget exhausted",
            Self::CoolingDown => "sync request cooling down",
            Self::FetchFailed => "sync request failed",
            Self::TooLarge => "sync response too large",
            Self::WorkerStopped => "sync worker stopped",
        })
    }
}
impl std::error::Error for SyncError {}
pub type Reply = Result<Arc<Value>, SyncError>;

/// Opaque native capability; deliberately neither Debug nor Serialize.
#[derive(Clone)]
pub struct Session(Arc<Scope>);
struct Scope {
    origin: String,
    account: String,
    generation: u64,
}
impl Session {
    pub fn generation(&self) -> u64 {
        self.0.generation
    }
}

/// Include ALL endpoint filters, pagination and timezone in parameters.
/// Object keys are normalized recursively; array order remains meaningful.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RequestKey {
    resource: String,
    parameters: String,
}
impl RequestKey {
    pub fn new(resource: &str, parameters: Value) -> Result<Self, SyncError> {
        fn normalize(value: Value) -> Value {
            match value {
                Value::Object(map) => {
                    let mut entries: Vec<_> = map.into_iter().collect();
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    Value::Object(
                        entries
                            .into_iter()
                            .map(|(k, v)| (k, normalize(v)))
                            .collect(),
                    )
                }
                Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
                other => other,
            }
        }
        if resource.is_empty() || resource.len() > 256 || size(&parameters, 8192).is_none() {
            return Err(SyncError::InvalidInput);
        }
        Ok(Self {
            resource: resource.into(),
            parameters: normalize(parameters).to_string(),
        })
    }
}
#[derive(Clone, Copy)]
pub enum QueryClass {
    Standard,
    Heavy,
}
#[derive(Clone, Copy)]
pub struct ReadPolicy {
    pub ttl: Duration,
    pub class: QueryClass,
}
struct Cached {
    value: Arc<Value>,
    expires: Duration,
    bytes: usize,
    used: u64,
}
struct Flight {
    result: watch::Sender<Option<Reply>>,
}
#[derive(Default)]
struct State {
    session: Option<Session>,
    generation: u64,
    sequence: u64,
    cache: HashMap<RequestKey, Cached>,
    flights: HashMap<RequestKey, Arc<Flight>>,
    requests: VecDeque<Duration>,
    failures: HashMap<RequestKey, Duration>,
}
struct Inner {
    config: Config,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    total: Semaphore,
    heavy: Semaphore,
    pending: Arc<Semaphore>,
}
impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}
#[derive(Clone)]
pub struct SyncCoordinator(Arc<Inner>);
impl SyncCoordinator {
    pub fn new(config: Config, clock: Arc<dyn Clock>) -> Result<Self, SyncError> {
        if config.max_entries == 0
            || config.max_cache_bytes == 0
            || config.max_response_bytes == 0
            || config.max_pending == 0
            || config.max_pending > Semaphore::MAX_PERMITS
            || config.max_ttl.is_zero()
            || config.max_requests_per_minute == 0
        {
            return Err(SyncError::InvalidInput);
        }
        Ok(Self(Arc::new(Inner {
            config,
            clock,
            state: Mutex::new(State::default()),
            total: Semaphore::new(2),
            heavy: Semaphore::new(1),
            pending: Arc::new(Semaphore::new(config.max_pending)),
        })))
    }
    /// Call after authorization succeeds. Every call rotates generation, even
    /// for the same account. Only one active account/origin exists per instance.
    pub fn start_session(&self, origin: &str, account: &str) -> Result<Session, SyncError> {
        if origin.is_empty() || account.is_empty() || origin.len() > 2048 || account.len() > 256 {
            return Err(SyncError::InvalidInput);
        }
        let mut state = self.0.lock();
        let generation = state
            .generation
            .checked_add(1)
            .ok_or(SyncError::StaleSession)?;
        clear(&mut state, None, SyncError::StaleSession);
        let session = Session(Arc::new(Scope {
            origin: origin.into(),
            account: account.into(),
            generation,
        }));
        state.generation = generation;
        state.session = Some(session.clone());
        state.requests.clear();
        state.failures.clear();
        Ok(session)
    }
    /// Immediately blocks local reads; credential deletion/revocation is external.
    /// An old session cannot log out a newly authorized account.
    pub fn logout(&self, session: &Session) -> Result<(), SyncError> {
        let mut state = self.0.lock();
        check(&state, session)?;
        state.session = None;
        state.generation = state.generation.saturating_add(1);
        clear(&mut state, None, SyncError::StaleSession);
        Ok(())
    }
    /// None clears everything; Some invalidates all filters of one resource.
    pub fn invalidate(&self, session: &Session, resource: Option<&str>) -> Result<(), SyncError> {
        let mut state = self.0.lock();
        check(&state, session)?;
        clear(&mut state, resource, SyncError::Invalidated);
        Ok(())
    }
    /// First leader determines TTL/class; resource adapters must use consistent
    /// policy. Dropping a waiter leaves shared work running. Invalidation cancels
    /// queued/running fetch futures. No retries or mutation handling are provided.
    pub async fn read<F, Fut, E>(
        &self,
        session: &Session,
        key: RequestKey,
        policy: ReadPolicy,
        fetch: F,
    ) -> Reply
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value, E>> + Send + 'static,
    {
        self.read_with_freshness(session, key, policy, false, fetch)
            .await
    }
    /// An explicit refresh bypasses only the cached response. It still shares
    /// an existing fetch and obeys the same session, queue, rate and cooldown
    /// limits. A failed refresh leaves the last successful response intact.
    pub async fn read_fresh<F, Fut, E>(
        &self,
        session: &Session,
        key: RequestKey,
        policy: ReadPolicy,
        fetch: F,
    ) -> Reply
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value, E>> + Send + 'static,
    {
        self.read_with_freshness(session, key, policy, true, fetch)
            .await
    }
    async fn read_with_freshness<F, Fut, E>(
        &self,
        session: &Session,
        key: RequestKey,
        policy: ReadPolicy,
        fresh: bool,
        fetch: F,
    ) -> Reply
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value, E>> + Send + 'static,
    {
        let mut receiver = {
            let mut state = self.0.lock();
            check(&state, session)?;
            let now = self.0.clock.now();
            state.cache.retain(|_, e| e.expires > now);
            state.sequence = state.sequence.saturating_add(1);
            let sequence = state.sequence;
            if !fresh {
                if let Some(entry) = state.cache.get_mut(&key) {
                    entry.used = sequence;
                    return Ok(entry.value.clone());
                }
            }
            if let Some(flight) = state.flights.get(&key) {
                flight.result.subscribe()
            } else {
                state.failures.retain(|_, until| *until > now);
                if state.failures.contains_key(&key) {
                    return Err(SyncError::CoolingDown);
                }
                let pending = self
                    .0
                    .pending
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| SyncError::QueueFull)?;
                while state
                    .requests
                    .front()
                    .is_some_and(|time| now.saturating_sub(*time) >= Duration::from_secs(60))
                {
                    state.requests.pop_front();
                }
                if state.requests.len() >= self.0.config.max_requests_per_minute {
                    return Err(SyncError::BudgetExceeded);
                }
                state.requests.push_back(now);
                let (sender, receiver) = watch::channel(None);
                let flight = Arc::new(Flight { result: sender });
                state.flights.insert(key.clone(), flight.clone());
                let guard = Completion {
                    inner: self.0.clone(),
                    session: session.clone(),
                    key,
                    flight,
                    finished: false,
                };
                let mut cancelled = guard.flight.result.subscribe();
                tokio::spawn(async move {
                    let _pending = pending;
                    let inner = guard.inner.clone();
                    let work = async {
                        // Heavy waiters never occupy total slots while waiting
                        // for the single aggregate slot.
                        let _heavy = match policy.class {
                            QueryClass::Heavy => Some(
                                inner
                                    .heavy
                                    .acquire()
                                    .await
                                    .map_err(|_| SyncError::WorkerStopped)?,
                            ),
                            QueryClass::Standard => None,
                        };
                        let _total = inner
                            .total
                            .acquire()
                            .await
                            .map_err(|_| SyncError::WorkerStopped)?;
                        let value = fetch().await.map_err(|_| SyncError::FetchFailed)?;
                        let bytes = size(&value, inner.config.max_response_bytes)
                            .ok_or(SyncError::TooLarge)?;
                        Ok((Arc::new(value), bytes))
                    };
                    tokio::select! {
                        biased;
                        _ = cancelled.changed() => {},
                        result = work => guard.finish(result, policy.ttl),
                    }
                });
                receiver
            }
        };
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                check(&self.0.lock(), session)?;
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| SyncError::WorkerStopped)?;
        }
    }
}
fn check(state: &State, session: &Session) -> Result<(), SyncError> {
    match &state.session {
        Some(active)
            if Arc::ptr_eq(&active.0, &session.0)
                && active.0.origin == session.0.origin
                && active.0.account == session.0.account =>
        {
            Ok(())
        }
        _ => Err(SyncError::StaleSession),
    }
}
fn clear(state: &mut State, resource: Option<&str>, error: SyncError) {
    let matches = |key: &RequestKey| resource.map_or(true, |r| key.resource == r);
    state.cache.retain(|key, _| !matches(key));
    state.flights.retain(|key, flight| {
        if matches(key) {
            flight.result.send_replace(Some(Err(error)));
            false
        } else {
            true
        }
    });
}
struct Completion {
    inner: Arc<Inner>,
    session: Session,
    key: RequestKey,
    flight: Arc<Flight>,
    finished: bool,
}
impl Completion {
    fn finish(mut self, result: Result<(Arc<Value>, usize), SyncError>, ttl: Duration) {
        let mut state = self.inner.lock();
        if check(&state, &self.session).is_ok()
            && state
                .flights
                .get(&self.key)
                .map_or(false, |f| Arc::ptr_eq(f, &self.flight))
        {
            let reply = match result {
                Ok((value, bytes)) => {
                    let config = self.inner.config;
                    let now = self.inner.clock.now();
                    state.cache.retain(|_, e| e.expires > now);
                    let ttl = ttl.min(config.max_ttl);
                    if !ttl.is_zero() && bytes <= config.max_cache_bytes {
                        while state.cache.len() >= config.max_entries
                            || state.cache.values().map(|e| e.bytes).sum::<usize>()
                                > config.max_cache_bytes - bytes
                        {
                            let oldest = state
                                .cache
                                .iter()
                                .min_by_key(|(_, e)| e.used)
                                .map(|(k, _)| k.clone());
                            if let Some(key) = oldest {
                                state.cache.remove(&key);
                            } else {
                                break;
                            }
                        }
                        state.sequence = state.sequence.saturating_add(1);
                        let used = state.sequence;
                        state.cache.insert(
                            self.key.clone(),
                            Cached {
                                value: value.clone(),
                                bytes,
                                used,
                                expires: now.saturating_add(ttl),
                            },
                        );
                    }
                    Ok(value)
                }
                Err(error) => {
                    // Bound failure metadata too. Never cache an upstream body.
                    if state.failures.len() >= self.inner.config.max_entries {
                        if let Some(oldest) = state
                            .failures
                            .iter()
                            .min_by_key(|(_, until)| **until)
                            .map(|(key, _)| key.clone())
                        {
                            state.failures.remove(&oldest);
                        }
                    }
                    state.failures.insert(
                        self.key.clone(),
                        self.inner
                            .clock
                            .now()
                            .saturating_add(Duration::from_secs(5)),
                    );
                    Err(error)
                }
            };
            state.flights.remove(&self.key);
            self.flight.result.send_replace(Some(reply));
        }
        self.finished = true;
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self.inner.lock();
            if state
                .flights
                .get(&self.key)
                .map_or(false, |f| Arc::ptr_eq(f, &self.flight))
            {
                state.flights.remove(&self.key);
                self.flight
                    .result
                    .send_replace(Some(Err(SyncError::WorkerStopped)));
            }
        }
    }
}
/// Bound serialized response bytes without allocating a second response buffer.
fn size(value: &Value, limit: usize) -> Option<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "size limit",
                ));
            }
            self.bytes += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    #[derive(Default)]
    struct FakeClock(AtomicU64);
    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_secs(self.0.load(Ordering::SeqCst))
        }
    }
    fn setup(config: Config) -> (SyncCoordinator, Session, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock::default());
        let coordinator = SyncCoordinator::new(config, clock.clone()).unwrap();
        let session = coordinator
            .start_session("https://example.invalid", "account-a")
            .unwrap();
        (coordinator, session, clock)
    }
    fn key(resource: &str) -> RequestKey {
        RequestKey::new(resource, json!({"page": 1, "timezone": "UTC"})).unwrap()
    }
    fn policy(class: QueryClass) -> ReadPolicy {
        ReadPolicy {
            ttl: Duration::from_secs(10),
            class,
        }
    }
    async fn value(c: &SyncCoordinator, s: &Session, resource: &str, n: u64) -> Reply {
        c.read(
            s,
            key(resource),
            policy(QueryClass::Standard),
            move || async move { Ok::<_, ()>(json!(n)) },
        )
        .await
    }
    // Poll a pending future once, without timers or scheduler-dependent sleeps.
    async fn pending<F: Future>(future: std::pin::Pin<&mut F>) {
        let mut future = future;
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn rolling_budget_counts_leaders_not_cache_hits_and_survives_invalidation() {
        let (c, s, clock) = setup(Config {
            max_requests_per_minute: 2,
            ..Config::default()
        });
        value(&c, &s, "a", 1).await.unwrap();
        assert_eq!(*value(&c, &s, "a", 9).await.unwrap(), json!(1));
        value(&c, &s, "b", 2).await.unwrap();
        c.invalidate(&s, None).unwrap();
        assert_eq!(value(&c, &s, "c", 3).await, Err(SyncError::BudgetExceeded));
        clock.0.store(59, Ordering::SeqCst);
        assert_eq!(value(&c, &s, "c", 3).await, Err(SyncError::BudgetExceeded));
        clock.0.store(60, Ordering::SeqCst);
        value(&c, &s, "c", 3).await.unwrap();
        let next = c
            .start_session("https://example.invalid", "account-b")
            .unwrap();
        value(&c, &next, "d", 4).await.unwrap();
    }

    #[tokio::test]
    async fn explicit_refresh_bypasses_cache_without_changing_other_filters_or_budget() {
        let (c, s, _) = setup(Config {
            max_requests_per_minute: 3,
            ..Config::default()
        });
        value(&c, &s, "usage", 1).await.unwrap();
        let other = RequestKey::new("usage", json!({"page": 2, "timezone": "UTC"})).unwrap();
        c.read(&s, other.clone(), policy(QueryClass::Standard), || async {
            Ok::<_, ()>(json!(9))
        })
        .await
        .unwrap();
        let fresh = c
            .read_fresh(&s, key("usage"), policy(QueryClass::Standard), || async {
                Ok::<_, ()>(json!(2))
            })
            .await
            .unwrap();
        assert_eq!(*fresh, json!(2));
        assert_eq!(*value(&c, &s, "usage", 99).await.unwrap(), json!(2));
        assert_eq!(
            *c.read(&s, other, policy(QueryClass::Standard), || async {
                panic!("refresh must not clear other filters");
                #[allow(unreachable_code)]
                Ok::<_, ()>(Value::Null)
            })
            .await
            .unwrap(),
            json!(9)
        );
        assert_eq!(
            c.read_fresh(&s, key("usage"), policy(QueryClass::Standard), || async {
                Ok::<_, ()>(json!(3))
            })
            .await,
            Err(SyncError::BudgetExceeded)
        );
    }

    #[tokio::test]
    async fn explicit_refresh_coalesces_and_preserves_previous_value_while_pending() {
        let (c, s, _) = setup(Config::default());
        value(&c, &s, "usage", 1).await.unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let mut first = Box::pin(c.read_fresh(
            &s,
            key("usage"),
            policy(QueryClass::Standard),
            move || async move {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok::<_, ()>(json!(2))
            },
        ));
        pending(first.as_mut()).await;
        started_rx.await.unwrap();
        assert_eq!(*value(&c, &s, "usage", 99).await.unwrap(), json!(1));
        let mut follower =
            Box::pin(
                c.read_fresh(&s, key("usage"), policy(QueryClass::Standard), || async {
                    panic!("refresh follower must share the running fetch");
                    #[allow(unreachable_code)]
                    Ok::<_, ()>(Value::Null)
                }),
            );
        pending(follower.as_mut()).await;
        drop(first);
        release_tx.send(()).unwrap();
        assert_eq!(*follower.await.unwrap(), json!(2));
        assert_eq!(*value(&c, &s, "usage", 99).await.unwrap(), json!(2));
    }

    #[tokio::test]
    async fn failed_explicit_refresh_keeps_cache_and_cannot_bypass_cooldown() {
        let (c, s, _) = setup(Config::default());
        value(&c, &s, "usage", 1).await.unwrap();
        assert_eq!(
            c.read_fresh(&s, key("usage"), policy(QueryClass::Standard), || async {
                Err::<Value, _>(())
            })
            .await,
            Err(SyncError::FetchFailed)
        );
        assert_eq!(*value(&c, &s, "usage", 99).await.unwrap(), json!(1));
        assert_eq!(
            c.read_fresh(&s, key("usage"), policy(QueryClass::Standard), || async {
                panic!("refresh must honor failure cooldown");
                #[allow(unreachable_code)]
                Ok::<_, ()>(Value::Null)
            })
            .await,
            Err(SyncError::CoolingDown)
        );
    }

    #[tokio::test]
    async fn coalesces_followers_and_survives_waiter_cancellation() {
        let (c, s, _) = setup(Config::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let mut first = Box::pin(c.read(
            &s,
            key("profile"),
            policy(QueryClass::Standard),
            move || async move {
                count.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok::<_, ()>(json!(7))
            },
        ));
        pending(first.as_mut()).await;
        started_rx.await.unwrap();
        let mut follower =
            Box::pin(
                c.read(&s, key("profile"), policy(QueryClass::Standard), || async {
                    panic!("follower fetch must never run");
                    #[allow(unreachable_code)]
                    Ok::<_, ()>(Value::Null)
                }),
            );
        pending(follower.as_mut()).await;
        drop(first);
        release_tx.send(()).unwrap();
        assert_eq!(*follower.await.unwrap(), json!(7));
        assert_eq!(*value(&c, &s, "profile", 99).await.unwrap(), json!(7));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ttl_lru_byte_limit_and_parameter_normalization() {
        let (c, s, clock) = setup(Config {
            max_entries: 2,
            max_cache_bytes: 4,
            max_ttl: Duration::from_secs(5),
            ..Config::default()
        });
        value(&c, &s, "a", 1).await.unwrap();
        value(&c, &s, "b", 2).await.unwrap();
        assert_eq!(*value(&c, &s, "a", 9).await.unwrap(), json!(1));
        value(&c, &s, "c", 3).await.unwrap();
        assert!(!c.0.lock().cache.contains_key(&key("b")));
        clock.0.store(5, Ordering::SeqCst);
        assert_eq!(*value(&c, &s, "a", 4).await.unwrap(), json!(4));
        value(&c, &s, "large", 1234).await.unwrap();
        assert_eq!(c.0.lock().cache.len(), 1);
        assert!(c.0.lock().cache.values().map(|e| e.bytes).sum::<usize>() <= 4);
        let a = RequestKey::new(
            "usage",
            serde_json::from_str(r#"{"z":{"b":2,"a":1},"a":0}"#).unwrap(),
        )
        .unwrap();
        let b = RequestKey::new("usage", json!({"a":0,"z":{"a":1,"b":2}})).unwrap();
        assert!(a == b);
        assert!(
            key("usage") != RequestKey::new("usage", json!({"page":2,"timezone":"UTC"})).unwrap()
        );
    }

    #[tokio::test]
    async fn invalidation_cannot_refill_or_remove_replacement() {
        let (c, s, _) = setup(Config::default());
        let (started_tx, started_rx) = oneshot::channel();
        let (_release_tx, release_rx) = oneshot::channel::<()>();
        let mut old = Box::pin(c.read(
            &s,
            key("keys"),
            policy(QueryClass::Standard),
            move || async move {
                started_tx.send(()).unwrap();
                let _ = release_rx.await;
                Ok::<_, ()>(json!(1))
            },
        ));
        pending(old.as_mut()).await;
        started_rx.await.unwrap();
        value(&c, &s, "profile", 5).await.unwrap();
        c.invalidate(&s, Some("keys")).unwrap();
        assert_eq!(old.await, Err(SyncError::Invalidated));
        assert_eq!(*value(&c, &s, "keys", 2).await.unwrap(), json!(2));
        assert_eq!(*value(&c, &s, "keys", 3).await.unwrap(), json!(2));
        assert_eq!(*value(&c, &s, "profile", 9).await.unwrap(), json!(5));

        // Simulate an already completing transport independently of cancellation:
        // the old completion must not replace/remove the new flight.
        let stale = Arc::new(Flight {
            result: watch::channel(None).0,
        });
        let replacement = Arc::new(Flight {
            result: watch::channel(None).0,
        });
        c.0.lock().flights.insert(key("race"), replacement.clone());
        Completion {
            inner: c.0.clone(),
            session: s.clone(),
            key: key("race"),
            flight: stale,
            finished: false,
        }
        .finish(Ok((Arc::new(json!(99)), 2)), Duration::from_secs(10));
        let state = c.0.lock();
        assert!(Arc::ptr_eq(
            state.flights.get(&key("race")).unwrap(),
            &replacement
        ));
        assert!(!state.cache.contains_key(&key("race")));
    }

    #[tokio::test]
    async fn session_rotation_logout_and_foreign_capabilities_are_isolated() {
        let (c, first, _) = setup(Config::default());
        value(&c, &first, "profile", 1).await.unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let mut old = Box::pin(c.read(
            &first,
            key("slow"),
            policy(QueryClass::Standard),
            move || async move {
                started_tx.send(()).unwrap();
                std::future::pending::<()>().await;
                Ok::<_, ()>(json!(1))
            },
        ));
        pending(old.as_mut()).await;
        started_rx.await.unwrap();
        let second = c
            .start_session("https://other.invalid", "account-b")
            .unwrap();
        assert!(second.generation() > first.generation());
        assert_eq!(old.await, Err(SyncError::StaleSession));
        assert_eq!(
            value(&c, &first, "profile", 99).await,
            Err(SyncError::StaleSession)
        );
        assert_eq!(*value(&c, &second, "profile", 2).await.unwrap(), json!(2));
        assert_eq!(c.logout(&first), Err(SyncError::StaleSession));
        let third = c
            .start_session("https://other.invalid", "account-b")
            .unwrap();
        assert!(third.generation() > second.generation());
        assert_eq!(*value(&c, &third, "profile", 3).await.unwrap(), json!(3));
        let (other, foreign, _) = setup(Config::default());
        assert_eq!(
            value(&other, &third, "profile", 4).await,
            Err(SyncError::StaleSession)
        );
        assert_eq!(
            value(&c, &foreign, "profile", 4).await,
            Err(SyncError::StaleSession)
        );
        c.logout(&third).unwrap();
        assert!(c.0.lock().cache.is_empty());
        assert_eq!(
            value(&c, &third, "profile", 4).await,
            Err(SyncError::StaleSession)
        );
    }

    #[tokio::test]
    async fn failures_and_oversized_responses_are_not_cached() {
        let (c, s, clock) = setup(Config {
            max_response_bytes: 8,
            ..Config::default()
        });
        let error = c
            .read(&s, key("profile"), policy(QueryClass::Standard), || async {
                Err::<Value, _>("secret upstream token and response")
            })
            .await
            .unwrap_err();
        assert_eq!(error, SyncError::FetchFailed);
        assert_eq!(error.to_string(), "sync request failed");
        assert!(c.0.lock().flights.is_empty());
        assert_eq!(
            value(&c, &s, "profile", 7).await,
            Err(SyncError::CoolingDown)
        );
        c.invalidate(&s, Some("profile")).unwrap();
        assert_eq!(
            value(&c, &s, "profile", 7).await,
            Err(SyncError::CoolingDown)
        );
        clock.0.store(5, Ordering::SeqCst);
        assert_eq!(*value(&c, &s, "profile", 7).await.unwrap(), json!(7));
        let oversized = c
            .read(&s, key("big"), policy(QueryClass::Standard), || async {
                Ok::<_, ()>(json!("123456789"))
            })
            .await;
        assert_eq!(oversized, Err(SyncError::TooLarge));
        clock.0.store(10, Ordering::SeqCst);
        assert_eq!(*value(&c, &s, "big", 1).await.unwrap(), json!(1));
    }

    #[tokio::test]
    async fn dropped_worker_notifies_waiters_and_allows_recovery() {
        let (c, s, _) = setup(Config::default());
        let (sender, mut receiver) = watch::channel(None);
        let flight = Arc::new(Flight { result: sender });
        c.0.lock().flights.insert(key("profile"), flight.clone());
        // Same RAII path used when a spawned worker panics or is aborted.
        drop(Completion {
            inner: c.0.clone(),
            session: s.clone(),
            key: key("profile"),
            flight,
            finished: false,
        });
        receiver.changed().await.unwrap();
        assert_eq!(
            receiver.borrow().clone(),
            Some(Err(SyncError::WorkerStopped))
        );
        assert!(c.0.lock().flights.is_empty());
        assert_eq!(*value(&c, &s, "profile", 1).await.unwrap(), json!(1));
    }

    #[tokio::test]
    async fn total_heavy_and_pending_budgets_are_enforced() {
        let (c, s, _) = setup(Config {
            max_pending: 4,
            ..Config::default()
        });
        let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
        let mut futures = Vec::new();
        let mut releases = Vec::new();
        for (name, class) in [
            ("h1", QueryClass::Heavy),
            ("h2", QueryClass::Heavy),
            ("s1", QueryClass::Standard),
            ("s2", QueryClass::Standard),
        ] {
            let (tx, rx) = oneshot::channel();
            releases.push(tx);
            let started = started.clone();
            futures.push(Box::pin(c.read(
                &s,
                key(name),
                policy(class),
                move || async move {
                    started.send(name).unwrap();
                    rx.await.unwrap();
                    Ok::<_, ()>(json!(name))
                },
            )));
            pending(futures.last_mut().unwrap().as_mut()).await;
        }
        let a = starts.recv().await.unwrap();
        let b = starts.recv().await.unwrap();
        assert!(a == "h1" || b == "h1");
        assert!(a == "s1" || b == "s1");
        assert_eq!(c.0.total.available_permits(), 0);
        assert_eq!(c.0.heavy.available_permits(), 0);
        assert!(starts.try_recv().is_err());
        assert_eq!(
            value(&c, &s, "overflow", 0).await,
            Err(SyncError::QueueFull)
        );
        releases.remove(0).send(()).unwrap();
        let next = starts.recv().await.unwrap();
        assert!(next == "h2" || next == "s2");
        for release in releases {
            release.send(()).unwrap();
        }
        for future in futures {
            future.await.unwrap();
        }
        assert_eq!(c.0.total.available_permits(), 2);
        assert_eq!(c.0.heavy.available_permits(), 1);
        value(&c, &s, "recovered", 1).await.unwrap();
    }
}
