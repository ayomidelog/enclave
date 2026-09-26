//! The one type callers use: a timer that records itself when it is dropped.

use std::time::Instant;

use super::counters::{record_phase_latency, record_request_latency};
use super::enabled;
use super::histogram::record_named_phase_latency;

#[derive(Debug)]
pub(crate) struct Timer {
    name: &'static str,
    started: Instant,
}

impl Timer {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            started: Instant::now(),
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let elapsed_us = self.started.elapsed().as_micros() as u64;
        record_phase_latency(elapsed_us);
        record_named_phase_latency(self.name, elapsed_us);
        if self.name == "daemon.request" {
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
