use super::*;

/// A counting semaphore over one connection budget.
///
/// A permit is released when it drops, so a connection worker that panics or
/// returns early cannot leak its slot.
pub(crate) struct ConnectionLimiter {
    state: Mutex<ConnectionState>,
    limit: usize,
}

#[derive(Default)]
pub(super) struct ConnectionState {
    active: usize,
}

/// One acquired slot in a single budget.
pub(crate) struct Permit {
    limiter: Arc<ConnectionLimiter>,
}

impl ConnectionLimiter {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(ConnectionState::default()),
            limit,
        }
    }

    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<Permit> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.active >= self.limit {
            return None;
        }
        state.active += 1;
        Some(Permit {
            limiter: Arc::clone(self),
        })
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
    }

    /// The counter mutex, so a test can prove the release path survives a poison.
    #[cfg(test)]
    pub(crate) fn state_mutex(&self) -> &Mutex<ConnectionState> {
        &self.state
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        // A panic here would be a panic inside a destructor, which aborts the
        // whole daemon when it happens during unwinding. Recovering from a
        // poisoned lock keeps releasing the slot, which is the only thing this
        // has to do.
        let mut state = self
            .limiter
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active = state.active.saturating_sub(1);
    }
}

/// The two budgets a proxied connection has to fit into.
///
/// A published port gets its own budget so a single busy port cannot consume
/// every slot, and every port also draws from one daemon-wide budget so the
/// total number of proxied connections stays bounded no matter how many ports
/// are published. Both slots are held for the life of the connection and
/// released together.
pub(crate) struct ConnectionBudget {
    publisher: Arc<ConnectionLimiter>,
    global: Arc<ConnectionLimiter>,
}

pub(crate) struct ConnectionPermit {
    _publisher: Permit,
    _global: Permit,
}

impl ConnectionBudget {
    pub(crate) fn new(publisher: Arc<ConnectionLimiter>, global: Arc<ConnectionLimiter>) -> Self {
        Self { publisher, global }
    }

    pub(crate) fn try_acquire(&self) -> Option<ConnectionPermit> {
        let publisher = self.publisher.try_acquire()?;
        // Returning here drops the publisher slot, so a connection rejected by
        // the daemon-wide budget does not hold a per-port slot either.
        let global = self.global.try_acquire()?;
        Some(ConnectionPermit {
            _publisher: publisher,
            _global: global,
        })
    }
}
