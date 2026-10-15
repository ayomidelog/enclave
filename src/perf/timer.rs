//! The one type callers use: a timer that records itself when it is dropped.
//!
//! Which histogram a sample lands in is a property of the timer, not of its name.
//! A timer that wrapped a whole request says so through [`Timer::request`], and the
//! name is then only a label for the per-phase breakdown and the diagnostic line.
//! Deciding it from the name was a trap: a rename would have silently stopped
//! populating the request histogram, and a second timer that happened to reuse the
//! name would have been counted as a request.

use std::time::Instant;

use super::counters::{record_phase_latency, record_request_latency};
use super::enabled;
use super::histogram::record_named_phase_latency;

#[derive(Debug)]
pub(crate) struct Timer {
    name: &'static str,
    started: Instant,
    /// Whether dropping this timer also records one request's latency.
    records_request: bool,
}

impl Timer {
    /// A timer for one phase of an operation.
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            started: Instant::now(),
            records_request: false,
        }
    }

    /// A timer for one request, which is also one sample of request latency.
    pub(crate) fn request(name: &'static str) -> Self {
        Self {
            records_request: true,
            ..Self::new(name)
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let elapsed_us = self.started.elapsed().as_micros() as u64;
        record_phase_latency(elapsed_us);
        record_named_phase_latency(self.name, elapsed_us);
        if self.records_request {
            record_request_latency(elapsed_us);
        }
        if enabled() {
            // The operation id is part of the line so a phase timing can be tied
            // to the lifecycle operation that produced it. Without it, a slow
            // phase in the log cannot be attributed to the request that paid it,
            // which is what makes a phase trace actionable rather than only
            // interesting.
            let operation = crate::operation::current().unwrap_or_default();
            eprintln!(
                "timing operation={} phase={} elapsed_us={}",
                operation, self.name, elapsed_us
            );
            tracing::info!(
                target: "enclave::perf",
                operation_id = %operation,
                phase = self.name,
                elapsed_us,
            );
        }
    }
}
