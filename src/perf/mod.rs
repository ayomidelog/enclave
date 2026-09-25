//! What the daemon measures about itself, and how it reports it.
//!
//! Two kinds of number live here. Counters answer how much of a thing happened:
//! requests, transfers, process spawns, mounts, cleanup retries. Histograms answer
//! how long a thing took, in bounded buckets. Both are cheap enough to leave in the
//! shipping binary, and both are off the decision path: nothing in the daemon
//! branches on a metric, so a metric that is wrong is a wrong number rather than a
//! wrong outcome.
//!
//! The modules are split by what they hold: `counters` is the totals and the
//! functions that increment them, `histogram` is the latency buckets and the
//! percentile estimate, and `timer` is the one type the rest of the codebase calls,
//! which records itself when it goes out of scope.

mod counters;
mod histogram;
mod timer;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

pub(crate) use counters::*;
pub(crate) use timer::Timer;

static ENABLED: OnceLock<bool> = OnceLock::new();
static VERBOSE: AtomicU64 = AtomicU64::new(0);

/// Whether per-operation timing is being printed as well as counted.
///
/// This is the switch for the diagnostic lines, not for the metrics: the counters
/// and histograms are always collected, because a number that is only collected
/// when someone expected to need it is not there when they do.
///
/// The answer is a property of how the process was started, so it is read from the
/// environment once and then remembered. The one exception is the verbose flag,
/// which a command can turn on for its own lifetime.
///
/// This is a relaxed load on the hot path of every host command and every phase, so
/// it is deliberately not an atomic with a stronger ordering.
pub(crate) fn enabled() -> bool {
    VERBOSE.load(Ordering::Relaxed) != 0
        || *ENABLED.get_or_init(|| {
            std::env::var("ENCLAVE_PERF")
                .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
                .unwrap_or(false)
        })
}

/// Turn the diagnostic lines on or off for this process.
pub(crate) fn set_verbose(enabled: bool) {
    VERBOSE.store(enabled as u64, Ordering::Relaxed);
}
