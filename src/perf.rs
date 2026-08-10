use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static ENABLED: OnceLock<bool> = OnceLock::new();
static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);
static TRANSFER_COUNT: AtomicU64 = AtomicU64::new(0);
static TRANSFER_BYTES: AtomicU64 = AtomicU64::new(0);
static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static PROCESS_SPAWNS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("ENCLAVE_PERF")
            .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    })
}

pub(crate) fn record_request() {
    REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_transfer(bytes: u64) {
    TRANSFER_COUNT.fetch_add(1, Ordering::Relaxed);
    TRANSFER_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

pub(crate) fn record_cache_hit() {
    CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_cache_miss() {
    CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_process_spawn() {
    PROCESS_SPAWNS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn metrics() -> serde_json::Value {
    serde_json::json!({
        "requests": REQUEST_COUNT.load(Ordering::Relaxed),
        "transfers": TRANSFER_COUNT.load(Ordering::Relaxed),
        "transfer_bytes": TRANSFER_BYTES.load(Ordering::Relaxed),
        "cache_hits": CACHE_HITS.load(Ordering::Relaxed),
        "cache_misses": CACHE_MISSES.load(Ordering::Relaxed),
        "process_spawns": PROCESS_SPAWNS.load(Ordering::Relaxed),
    })
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
        if enabled() {
            tracing::info!(
                target: "enclave::perf",
                phase = self.name,
                elapsed_us = self.started.elapsed().as_micros() as u64,
            );
        }
    }
}
