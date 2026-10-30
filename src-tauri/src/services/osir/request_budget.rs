//! One shared account HTTP budget. Charge every auth/refresh/GET/replay/mutation
//! attempt. No sleeping, implicit retry, request data, or invalidation/reset API.
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const WINDOW: Duration = Duration::from_secs(60);
const MAX_COOLDOWN: Duration = Duration::from_secs(300);

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Duration;
}
struct SystemClock(Instant);
impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestClass {
    Standard,
    Heavy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Cooldown { retry_after: Duration },
    RateLimited { retry_after: Duration },
    Busy,
    HeavyBusy,
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cooldown { .. } => "账户请求暂处冷却期",
            Self::RateLimited { .. } => "账户请求预算已用尽",
            Self::Busy => "账户请求并发已满",
            Self::HeavyBusy => "账户统计请求并发已满",
        })
    }
}
impl std::error::Error for AdmissionError {}
#[derive(Clone, Copy)]
pub struct Config {
    pub attempts_per_minute: usize,
    pub max_active: usize,
    pub max_heavy: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            attempts_per_minute: 60,
            max_active: 4,
            max_heavy: 2,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidConfig;
impl std::fmt::Display for InvalidConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("请求预算配置无效")
    }
}
impl std::error::Error for InvalidConfig {}
struct State {
    attempts: VecDeque<Duration>,
    active: usize,
    heavy: usize,
    cooldown_until: Duration,
    last_now: Duration,
    failed: u64,
}
struct Inner {
    clock: Arc<dyn Clock>,
    config: Config,
    state: Mutex<State>,
}
/// Clone shares limits and cooldowns; Arc<RequestBudget> is also supported.
/// Retain for the AuthManager/client lifetime, including resource invalidation.
#[derive(Clone)]
pub struct RequestBudget(Arc<Inner>);
impl Default for RequestBudget {
    fn default() -> Self {
        Self::with_clock(Config::default(), Arc::new(SystemClock(Instant::now())))
            .expect("fixed budget configuration")
    }
}
impl RequestBudget {
    pub fn with_clock(config: Config, clock: Arc<dyn Clock>) -> Result<Self, InvalidConfig> {
        if config.attempts_per_minute == 0
            || config.attempts_per_minute > 60
            || config.max_active == 0
            || config.max_active > 4
            || config.max_heavy == 0
            || config.max_heavy > 2
            || config.max_heavy > config.max_active
        {
            return Err(InvalidConfig);
        }
        Ok(Self(Arc::new(Inner {
            clock,
            config,
            state: Mutex::new(State {
                attempts: VecDeque::new(),
                active: 0,
                heavy: 0,
                cooldown_until: Duration::ZERO,
                last_now: Duration::ZERO,
                failed: 0,
            }),
        })))
    }
    fn now(&self, state: &mut State) -> Duration {
        let now = self.0.clock.now().max(state.last_now);
        state.last_now = now;
        now
    }
    /// Async entry point, immediate admission or safe error; never queues/waits.
    /// Hold guard through response-body validation. Cancellation retains charge.
    pub async fn admit(&self, class: RequestClass) -> Result<AttemptGuard, AdmissionError> {
        self.try_admit(class)
    }
    pub fn try_admit(&self, class: RequestClass) -> Result<AttemptGuard, AdmissionError> {
        let mut state = self.0.state.lock().unwrap();
        let now = self.now(&mut state);
        while state
            .attempts
            .front()
            .is_some_and(|at| now.saturating_sub(*at) >= WINDOW)
        {
            state.attempts.pop_front();
        }
        if now < state.cooldown_until {
            return Err(AdmissionError::Cooldown {
                retry_after: state.cooldown_until - now,
            });
        }
        if state.attempts.len() >= self.0.config.attempts_per_minute {
            return Err(AdmissionError::RateLimited {
                retry_after: state.attempts[0].saturating_add(WINDOW).saturating_sub(now),
            });
        }
        if state.active >= self.0.config.max_active {
            return Err(AdmissionError::Busy);
        }
        let heavy = class == RequestClass::Heavy;
        if heavy && state.heavy >= self.0.config.max_heavy {
            return Err(AdmissionError::HeavyBusy);
        }
        state.attempts.push_back(now);
        state.active += 1;
        if heavy {
            state.heavy += 1;
        }
        Ok(AttemptGuard {
            budget: self.clone(),
            heavy,
            successful: false,
        })
    }
    /// Call on headers, before body parsing. Caller converts Retry-After seconds
    /// or HTTP-date to Duration. 429/503 affect every endpoint. Missing/zero uses
    /// 5s/1s; values clamp to 5 minutes. Existing cooldown only extends.
    /// No jitter: denied work is never automatically rescheduled by this module.
    pub fn observe_response(&self, status: u16, retry_after: Option<Duration>) {
        let fallback = match status {
            429 => Duration::from_secs(5),
            503 => Duration::from_secs(1),
            _ => return,
        };
        let delay = retry_after
            .filter(|d| !d.is_zero())
            .unwrap_or(fallback)
            .min(MAX_COOLDOWN);
        let mut state = self.0.state.lock().unwrap();
        let now = self.now(&mut state);
        state.cooldown_until = state.cooldown_until.max(now.saturating_add(delay));
    }
    /// Aggregate only: no URLs, headers, account ids, or body data.
    pub fn failed_attempts(&self) -> u64 {
        self.0.state.lock().unwrap().failed
    }
}
/// Drop releases concurrency, never refunds rate charge. Default outcome is
/// failed, so transport errors and dropped/cancelled futures are counted too.
/// Deliberately neither Debug nor Serialize.
pub struct AttemptGuard {
    budget: RequestBudget,
    heavy: bool,
    successful: bool,
}
impl AttemptGuard {
    pub fn observe_response(&self, status: u16, retry_after: Option<Duration>) {
        self.budget.observe_response(status, retry_after);
    }
    /// Consume only after the full intended response is successfully validated.
    pub fn finish_success(mut self) {
        self.successful = true;
    }
}
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        let mut state = self.budget.0.state.lock().unwrap();
        state.active -= 1;
        if self.heavy {
            state.heavy -= 1;
        }
        if !self.successful {
            state.failed = state.failed.saturating_add(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[derive(Default)]
    struct FakeClock(AtomicU64);
    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::SeqCst))
        }
    }
    fn fixture() -> (RequestBudget, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock::default());
        (
            RequestBudget::with_clock(Config::default(), clock.clone()).unwrap(),
            clock,
        )
    }
    #[tokio::test]
    async fn sixty_attempts_include_failures_and_cancellation_without_refunds() {
        let (budget, clock) = fixture();
        for i in 0..60 {
            let guard = budget.admit(RequestClass::Standard).await.unwrap();
            if i % 2 == 0 {
                guard.finish_success();
            }
        }
        assert_eq!(budget.failed_attempts(), 30);
        assert!(matches!(
            budget.admit(RequestClass::Standard).await,
            Err(AdmissionError::RateLimited { .. })
        ));
        clock.0.store(59_999, Ordering::SeqCst);
        assert!(budget.try_admit(RequestClass::Standard).is_err());
        clock.0.store(60_000, Ordering::SeqCst);
        assert!(budget.try_admit(RequestClass::Standard).is_ok());
    }
    #[test]
    fn clones_share_active_four_heavy_two_and_drop_releases() {
        let (budget, _) = fixture();
        let clone = budget.clone();
        let a = budget.try_admit(RequestClass::Heavy).unwrap();
        let b = clone.try_admit(RequestClass::Heavy).unwrap();
        assert!(matches!(
            budget.try_admit(RequestClass::Heavy),
            Err(AdmissionError::HeavyBusy)
        ));
        let c = clone.try_admit(RequestClass::Standard).unwrap();
        let d = budget.try_admit(RequestClass::Standard).unwrap();
        assert!(matches!(
            clone.try_admit(RequestClass::Standard),
            Err(AdmissionError::Busy)
        ));
        drop(a);
        let e = clone.try_admit(RequestClass::Heavy).unwrap();
        drop((b, c, d, e));
        assert_eq!(budget.failed_attempts(), 5);
        assert!(budget.try_admit(RequestClass::Standard).is_ok());
    }
    #[test]
    fn account_cooldown_extends_is_bounded_and_survives_guard_drop() {
        let (budget, clock) = fixture();
        let guard = budget.try_admit(RequestClass::Standard).unwrap();
        guard.observe_response(429, Some(Duration::from_secs(600)));
        drop(guard);
        budget.observe_response(503, Some(Duration::from_secs(1)));
        assert!(
            matches!(budget.clone().try_admit(RequestClass::Heavy), Err(AdmissionError::Cooldown { retry_after }) if retry_after == MAX_COOLDOWN)
        );
        clock.0.store(299_999, Ordering::SeqCst);
        assert!(budget.try_admit(RequestClass::Standard).is_err());
        clock.0.store(300_000, Ordering::SeqCst);
        budget
            .try_admit(RequestClass::Standard)
            .unwrap()
            .finish_success();
        assert_eq!(budget.failed_attempts(), 1);
    }
    #[test]
    fn defaults_unrelated_responses_and_clock_regression_do_not_reset() {
        let (budget, clock) = fixture();
        budget.observe_response(503, None);
        assert!(
            matches!(budget.try_admit(RequestClass::Standard), Err(AdmissionError::Cooldown { retry_after }) if retry_after == Duration::from_secs(1))
        );
        clock.0.store(1000, Ordering::SeqCst);
        budget.observe_response(429, Some(Duration::ZERO));
        budget.observe_response(200, None);
        clock.0.store(500, Ordering::SeqCst);
        assert!(
            matches!(budget.try_admit(RequestClass::Standard), Err(AdmissionError::Cooldown { retry_after }) if retry_after == Duration::from_secs(5))
        );
        clock.0.store(6000, Ordering::SeqCst);
        assert!(budget.try_admit(RequestClass::Standard).is_ok());
    }
    #[test]
    fn rolling_window_expires_individual_attempts() {
        let clock = Arc::new(FakeClock::default());
        let budget = RequestBudget::with_clock(
            Config {
                attempts_per_minute: 2,
                ..Config::default()
            },
            clock.clone(),
        )
        .unwrap();
        budget
            .try_admit(RequestClass::Standard)
            .unwrap()
            .finish_success();
        clock.0.store(30_000, Ordering::SeqCst);
        budget
            .try_admit(RequestClass::Standard)
            .unwrap()
            .finish_success();
        clock.0.store(60_000, Ordering::SeqCst);
        budget
            .try_admit(RequestClass::Standard)
            .unwrap()
            .finish_success();
        assert!(
            matches!(budget.try_admit(RequestClass::Standard), Err(AdmissionError::RateLimited { retry_after }) if retry_after == Duration::from_secs(30))
        );
    }
}
