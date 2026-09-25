//! The counters the daemon reports, and the functions that increment them.
//!
//! Each counter is a relaxed atomic. They are read to answer how much of a thing
//! happened, never to make a decision, so a lost update under contention is not a
//! correctness problem and a stronger ordering on every one of them would be a
//! cost on the paths they measure.

use std::sync::atomic::{AtomicU64, Ordering};

use super::histogram::{
    histogram_values, named_phase_values, percentile_values, record_histogram, LATENCY_BUCKETS_US,
    PHASE_BUCKETS_US, PHASE_LATENCY, REQUEST_LATENCY,
};

static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);

static TRANSFER_COUNT: AtomicU64 = AtomicU64::new(0);

static TRANSFER_BYTES: AtomicU64 = AtomicU64::new(0);

static TRANSFER_FILES: AtomicU64 = AtomicU64::new(0);

static CACHE_HITS: AtomicU64 = AtomicU64::new(0);

static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

static NAMESPACE_CACHE_HITS: AtomicU64 = AtomicU64::new(0);

static NAMESPACE_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

static PROCESS_SPAWNS: AtomicU64 = AtomicU64::new(0);

static MOUNT_COUNT: AtomicU64 = AtomicU64::new(0);

static UNMOUNT_COUNT: AtomicU64 = AtomicU64::new(0);

static CLEANUP_RETRIES: AtomicU64 = AtomicU64::new(0);

static CLEANUP_RETRY_DELAY_US: AtomicU64 = AtomicU64::new(0);

static HOST_COMMANDS: AtomicU64 = AtomicU64::new(0);

static HOST_COMMAND_TIMEOUTS: AtomicU64 = AtomicU64::new(0);

static HOST_COMMAND_FAILURES: AtomicU64 = AtomicU64::new(0);

static REGISTRY_LOCK_WAIT_US: AtomicU64 = AtomicU64::new(0);

pub(crate) fn record_request() {
    REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_request_latency(elapsed_us: u64) {
    record_histogram(&REQUEST_LATENCY, elapsed_us, &LATENCY_BUCKETS_US);
}

pub(crate) fn record_phase_latency(elapsed_us: u64) {
    record_histogram(&PHASE_LATENCY, elapsed_us, &PHASE_BUCKETS_US);
}

pub(crate) fn record_transfer(bytes: u64) {
    TRANSFER_COUNT.fetch_add(1, Ordering::Relaxed);
    TRANSFER_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub(crate) fn record_transfer_files(files: u64) {
    TRANSFER_FILES.fetch_add(files, Ordering::Relaxed);
}

pub(crate) fn record_cache_hit() {
    CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_cache_miss() {
    CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_namespace_cache_hit() {
    NAMESPACE_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_namespace_cache_miss() {
    NAMESPACE_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_process_spawn() {
    PROCESS_SPAWNS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_mount() {
    MOUNT_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_unmount() {
    UNMOUNT_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_cleanup_retry() {
    CLEANUP_RETRIES.fetch_add(1, Ordering::Relaxed);
}

/// Record the time a cleanup retry waited before trying again.
///
/// The attempt count alone does not say whether retries cost a millisecond or a
/// second, and that difference is what decides whether a retry policy is the
/// reason a stop is slow.
pub(crate) fn record_cleanup_retry_delay(elapsed_us: u64) {
    CLEANUP_RETRY_DELAY_US.fetch_add(elapsed_us, Ordering::Relaxed);
}

/// Record one bounded host command and how it ended.
pub(crate) fn record_host_command(timed_out: bool, failed: bool) {
    HOST_COMMANDS.fetch_add(1, Ordering::Relaxed);
    if timed_out {
        HOST_COMMAND_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
    }
    if failed {
        HOST_COMMAND_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_registry_lock_wait(elapsed_us: u64) {
    REGISTRY_LOCK_WAIT_US.fetch_add(elapsed_us, Ordering::Relaxed);
}

pub(crate) fn metrics() -> serde_json::Value {
    serde_json::json!({
        "requests": REQUEST_COUNT.load(Ordering::Relaxed),
        "transfers": TRANSFER_COUNT.load(Ordering::Relaxed),
        "transfer_bytes": TRANSFER_BYTES.load(Ordering::Relaxed),
        "transfer_files": TRANSFER_FILES.load(Ordering::Relaxed),
        "cache_hits": CACHE_HITS.load(Ordering::Relaxed),
        "cache_misses": CACHE_MISSES.load(Ordering::Relaxed),
        "namespace_cache_hits": NAMESPACE_CACHE_HITS.load(Ordering::Relaxed),
        "namespace_cache_misses": NAMESPACE_CACHE_MISSES.load(Ordering::Relaxed),
        "process_spawns": PROCESS_SPAWNS.load(Ordering::Relaxed),
        "mounts": MOUNT_COUNT.load(Ordering::Relaxed),
        "unmounts": UNMOUNT_COUNT.load(Ordering::Relaxed),
        "cleanup_retries": CLEANUP_RETRIES.load(Ordering::Relaxed),
        "cleanup_retry_delay_us": CLEANUP_RETRY_DELAY_US.load(Ordering::Relaxed),
        "host_commands": HOST_COMMANDS.load(Ordering::Relaxed),
        "host_command_timeouts": HOST_COMMAND_TIMEOUTS.load(Ordering::Relaxed),
        "host_command_failures": HOST_COMMAND_FAILURES.load(Ordering::Relaxed),
        "registry_lock_wait_us": REGISTRY_LOCK_WAIT_US.load(Ordering::Relaxed),
        "request_latency_us": histogram_values(&REQUEST_LATENCY),
        "request_latency_percentiles_us":
            percentile_values(&REQUEST_LATENCY, &LATENCY_BUCKETS_US),
        "phase_latency_us": histogram_values(&PHASE_LATENCY),
        "phase_latency_percentiles_us": percentile_values(&PHASE_LATENCY, &PHASE_BUCKETS_US),
        "phase_latency_by_name": named_phase_values(),
    })
}
