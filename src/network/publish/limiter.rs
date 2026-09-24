use super::*;

pub(crate) struct ConnectionLimiter {
    state: Mutex<ConnectionState>,
    limit: usize,
}

#[derive(Default)]
pub(super) struct ConnectionState {
    active: usize,
}

pub(crate) struct ConnectionPermit {
    limiter: Arc<ConnectionLimiter>,
}

impl ConnectionLimiter {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(ConnectionState::default()),
            limit,
        }
    }

    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        let mut state = self
            .state
            .lock()
            .expect("connection limiter mutex poisoned");
        if state.active >= self.limit {
            return None;
        }
        state.active += 1;
        Some(ConnectionPermit {
            limiter: Arc::clone(self),
        })
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> usize {
        self.state
            .lock()
            .expect("connection limiter mutex poisoned")
            .active
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut state = self
            .limiter
            .state
            .lock()
            .expect("connection limiter mutex poisoned");
        state.active = state.active.saturating_sub(1);
    }
}
