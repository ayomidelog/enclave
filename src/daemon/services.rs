//! The shared services a daemon request handler needs.
//!
//! These are created once in `run_daemon` and handed to every worker. Bundling
//! them keeps the worker and dispatch signatures stable as services are added,
//! and makes it explicit which state a request handler is allowed to touch.

use std::sync::Arc;

use super::active_operations::ActiveOperations;
use super::leases::LifecycleLeases;
use super::rate_limiter::RateLimiter;
use crate::network::publish::PortPublisher;

#[derive(Clone)]
pub(crate) struct DaemonServices {
    pub(crate) rate_limiter: Arc<RateLimiter>,
    pub(crate) port_publisher: Arc<PortPublisher>,
    pub(crate) leases: Arc<LifecycleLeases>,
    /// Lifecycle operations in flight, so shutdown can report what it
    /// interrupted instead of exiting silently.
    pub(crate) active_operations: Arc<ActiveOperations>,
}
