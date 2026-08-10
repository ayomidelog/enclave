use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static ENABLED: OnceLock<bool> = OnceLock::new();
static VERBOSE: AtomicU64 = AtomicU64::new(0);
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
static REGISTRY_LOCK_WAIT_US: AtomicU64 = AtomicU64::new(0);
static REQUEST_LATENCY: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static PHASE_LATENCY: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static NAMED_PHASE_LATENCY: OnceLock<Mutex<BTreeMap<&'static str, [u64; 8]>>> = OnceLock::new();

const LATENCY_BUCKETS_US: [u64; 8] = [100, 500, 1_000, 5_000, 10_000, 50_000, 250_000, u64::MAX];

pub(crate) fn enabled() -> bool {
    VERBOSE.load(Ordering::Relaxed) != 0
        || *ENABLED.get_or_init(|| {
            std::env::var("ENCLAVE_PERF")
                .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
                .unwrap_or(false)
        })
}

pub(crate) fn set_verbose(enabled: bool) {
    VERBOSE.store(enabled as u64, Ordering::Relaxed);
}

pub(crate) fn record_request() {
    REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_request_latency(elapsed_us: u64) {
    record_histogram(&REQUEST_LATENCY, elapsed_us);
}

pub(crate) fn record_phase_latency(elapsed_us: u64) {
    record_histogram(&PHASE_LATENCY, elapsed_us);
}

pub(crate) fn record_named_phase_latency(name: &'static str, elapsed_us: u64) {
    let bucket = LATENCY_BUCKETS_US
        .iter()
        .position(|limit| elapsed_us <= *limit)
        .unwrap_or(LATENCY_BUCKETS_US.len() - 1);
    if let Ok(mut phases) = NAMED_PHASE_LATENCY
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
    {
        phases.entry(name).or_insert([0; 8])[bucket] += 1;
    }
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
        "registry_lock_wait_us": REGISTRY_LOCK_WAIT_US.load(Ordering::Relaxed),
        "request_latency_us": histogram_values(&REQUEST_LATENCY),
        "phase_latency_us": histogram_values(&PHASE_LATENCY),
        "phase_latency_by_name": named_phase_values(),
    })
}

fn record_histogram(histogram: &[AtomicU64; 8], elapsed_us: u64) {
    let bucket = LATENCY_BUCKETS_US
        .iter()
        .position(|limit| elapsed_us <= *limit)
        .unwrap_or(LATENCY_BUCKETS_US.len() - 1);
    histogram[bucket].fetch_add(1, Ordering::Relaxed);
}

fn histogram_values(histogram: &[AtomicU64; 8]) -> Vec<u64> {
    histogram
        .iter()
        .map(|bucket| bucket.load(Ordering::Relaxed))
        .collect()
}

fn named_phase_values() -> BTreeMap<&'static str, [u64; 8]> {
    NAMED_PHASE_LATENCY
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map(|phases| phases.clone())
        .unwrap_or_default()
}

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
            eprintln!("timing phase={} elapsed_us={}", self.name, elapsed_us);
            tracing::info!(
                target: "enclave::perf",
                phase = self.name,
                elapsed_us,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{histogram_values, record_histogram, LATENCY_BUCKETS_US, REQUEST_LATENCY};

    #[test]
    fn latency_histogram_places_samples_in_bounded_buckets() {
        let before = histogram_values(&REQUEST_LATENCY);
        record_histogram(&REQUEST_LATENCY, LATENCY_BUCKETS_US[0]);
        record_histogram(&REQUEST_LATENCY, LATENCY_BUCKETS_US[7]);
        let after = histogram_values(&REQUEST_LATENCY);
        assert_eq!(after[0], before[0] + 1);
        assert_eq!(after[7], before[7] + 1);
    }
}
