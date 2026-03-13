use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct RateLimiter {
    window: Duration,
    max_requests: usize,
    per_uid: Mutex<HashMap<u32, RateLimitState>>,
    global: Mutex<RateLimitState>,
    global_max_requests: usize,
}

#[derive(Debug, Clone)]
struct RateLimitState {
    window_started: Instant,
    count: usize,
}

impl RateLimiter {
    #[cfg(test)]
    pub(crate) fn new(window: Duration, max_requests: usize) -> Self {
        Self::with_global_limit(window, max_requests, max_requests * 10)
    }

    pub(crate) fn with_global_limit(
        window: Duration,
        max_requests: usize,
        global_max_requests: usize,
    ) -> Self {
        Self {
            window,
            max_requests,
            per_uid: Mutex::new(HashMap::new()),
            global: Mutex::new(RateLimitState {
                window_started: Instant::now(),
                count: 0,
            }),
            global_max_requests,
        }
    }

    pub(crate) fn allow(&self, uid: u32) -> bool {
        if !self.allow_global() {
            return false;
        }
        self.allow_per_uid(uid)
    }

    fn allow_global(&self) -> bool {
        let now = Instant::now();
        let mut state = match self.global.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                tracing::warn!("global rate limiter mutex was poisoned; recovering state");
                poisoned.into_inner()
            }
        };
        if now.duration_since(state.window_started) >= self.window {
            state.window_started = now;
            state.count = 0;
        }
        if state.count >= self.global_max_requests {
            return false;
        }
        state.count += 1;
        true
    }

    fn allow_per_uid(&self, uid: u32) -> bool {
        let now = Instant::now();
        let mut map = match self.per_uid.lock() {
            Ok(map) => map,
            Err(poisoned) => {
                tracing::warn!("per-uid rate limiter mutex was poisoned; recovering state");
                poisoned.into_inner()
            }
        };
        let entry = map.entry(uid).or_insert_with(|| RateLimitState {
            window_started: now,
            count: 0,
        });

        if now.duration_since(entry.window_started) >= self.window {
            entry.window_started = now;
            entry.count = 0;
        }
        if entry.count >= self.max_requests {
            return false;
        }
        entry.count += 1;
        true
    }
}

#[cfg(test)]
#[path = "../../tests/src/daemon/rate_limiter.rs"]
mod tests;
