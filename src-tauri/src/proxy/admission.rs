use axum::{body::Body, response::Response};
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::watch;

#[derive(Default)]
struct State {
    paused: bool,
    stopping: bool,
    active: usize,
    generation: u64,
}

struct Inner {
    state: Mutex<State>,
    active: watch::Sender<usize>,
}

#[derive(Clone)]
pub(super) struct Admission(Arc<Inner>);

impl Admission {
    #[cfg(test)]
    pub(super) fn active(&self) -> usize {
        self.0.state.lock().unwrap().active
    }
    #[cfg(test)]
    pub(super) fn paused(&self) -> bool {
        self.0.state.lock().unwrap().paused
    }
    pub(super) fn new() -> Self {
        let (active, _) = watch::channel(0);
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            active,
        }))
    }

    pub(super) fn enter(&self) -> Option<RequestGuard> {
        let mut state = self.0.state.lock().unwrap();
        if state.paused || state.stopping {
            return None;
        }
        state.active += 1;
        self.0.active.send_replace(state.active);
        Some(RequestGuard(self.clone()))
    }

    pub(super) fn stop(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.stopping = true;
        state.generation += 1;
    }

    pub(super) fn resume(&self) -> Result<(), ()> {
        let mut state = self.0.state.lock().unwrap();
        if state.stopping {
            return Err(());
        }
        state.paused = false;
        state.generation += 1;
        Ok(())
    }

    pub(super) async fn pause_and_wait(&self, timeout: Duration) -> Result<PauseLease, ()> {
        let generation = {
            let mut state = self.0.state.lock().unwrap();
            if state.stopping || state.paused {
                return Err(());
            }
            state.paused = true;
            state.generation += 1;
            state.generation
        };
        let rollback = PauseLease {
            admission: self.clone(),
            generation,
        };
        let mut active = self.0.active.subscribe();
        let wait = async {
            loop {
                if *active.borrow_and_update() == 0 {
                    return;
                }
                if active.changed().await.is_err() {
                    return;
                }
            }
        };
        tokio::time::timeout(timeout, wait).await.map_err(|_| ())?;
        // Resume or stop can race a waiter; never claim a pause that no longer exists.
        let state = self.0.state.lock().unwrap();
        if !state.paused || state.stopping || state.generation != generation {
            return Err(());
        }
        drop(state);
        Ok(rollback)
    }
}

pub(super) struct PauseLease {
    admission: Admission,
    generation: u64,
}
impl Drop for PauseLease {
    fn drop(&mut self) {
        let mut state = self.admission.0.state.lock().unwrap();
        if !state.stopping && state.generation == self.generation {
            state.paused = false;
        }
    }
}

pub(super) struct RequestGuard(Admission);
impl Drop for RequestGuard {
    fn drop(&mut self) {
        let mut state = self.0 .0.state.lock().unwrap();
        state.active -= 1;
        self.0 .0.active.send_replace(state.active);
    }
}

pub(super) fn track_response(response: Response, guard: RequestGuard) -> Response {
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(TrackedBody {
            body,
            guard: Some(guard),
        }),
    )
}

struct TrackedBody {
    body: Body,
    guard: Option<RequestGuard>,
}
impl http_body::Body for TrackedBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let result = Pin::new(&mut self.body).poll_frame(cx);
        if matches!(result, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            self.guard.take();
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn admission_waits_for_stream_and_resumes_after_timeout() {
        let admission = Admission::new();
        let guard = admission.enter().unwrap();
        let response = track_response(Response::new(Body::from("hello")), guard);
        assert!(admission
            .pause_and_wait(Duration::from_millis(10))
            .await
            .is_err());
        assert!(admission.enter().is_some());
        let waiting = {
            let admission = admission.clone();
            tokio::spawn(async move { admission.pause_and_wait(Duration::from_secs(1)).await })
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            while !admission.paused() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(admission.enter().is_none());
        response.into_body().collect().await.unwrap();
        let lease = waiting.await.unwrap().unwrap();
        assert!(admission.enter().is_none());
        admission.resume().unwrap();
        assert!(admission.enter().is_some());
        drop(lease);
    }

    #[tokio::test]
    async fn admission_cancellation_reopens_and_body_drop_releases() {
        let admission = Admission::new();
        let response = track_response(
            Response::new(Body::from("hello")),
            admission.enter().unwrap(),
        );
        let waiting = {
            let admission = admission.clone();
            tokio::spawn(async move { admission.pause_and_wait(Duration::from_secs(30)).await })
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            while !admission.paused() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(admission.enter().is_none());
        waiting.abort();
        let _ = waiting.await;
        assert!(admission.enter().is_some());
        drop(response);
        assert!(admission
            .pause_and_wait(Duration::from_secs(1))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn admission_lease_is_generation_bound_and_never_reopens_stopping() {
        let admission = Admission::new();
        let old = admission
            .pause_and_wait(Duration::from_secs(1))
            .await
            .unwrap();
        admission.resume().unwrap();
        let current = admission
            .pause_and_wait(Duration::from_secs(1))
            .await
            .unwrap();
        drop(old);
        assert!(admission.enter().is_none());
        admission.stop();
        drop(current);
        assert!(admission.enter().is_none());
    }

    #[tokio::test]
    async fn admission_waits_for_every_concurrent_response() {
        let admission = Admission::new();
        let first = track_response(Response::new(Body::empty()), admission.enter().unwrap());
        let second = track_response(Response::new(Body::empty()), admission.enter().unwrap());
        assert_eq!(admission.active(), 2);
        drop(first);
        assert_eq!(admission.active(), 1);
        assert!(admission
            .pause_and_wait(Duration::from_millis(10))
            .await
            .is_err());
        drop(second);
        let lease = admission
            .pause_and_wait(Duration::from_secs(1))
            .await
            .unwrap();
        assert!(admission.enter().is_none());
        drop(lease);
        assert!(admission.enter().is_some());
    }
}
