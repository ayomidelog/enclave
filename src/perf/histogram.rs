//! Latency histograms: the buckets, the percentile estimate, and the per-phase
//! breakdown.
//!
//! Samples are kept in bounded buckets rather than a list, so the memory a
//! histogram uses does not grow with the number of operations measured. The price
//! is that a percentile is known only to within its bucket, which is what the
//! comment on the percentile function is about.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub(super) fn record_named_phase_latency(name: &'static str, elapsed_us: u64) {
    let bucket = bucket_index(elapsed_us, &PHASE_BUCKETS_US);
    if let Ok(mut phases) = NAMED_PHASE_LATENCY
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
    {
        phases.entry(name).or_insert([0; 8])[bucket] += 1;
    }
}

// The histograms live here because only this module reads their buckets. The
// recorders in `counters` write them through the functions below.
pub(super) static REQUEST_LATENCY: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
pub(super) static PHASE_LATENCY: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
pub(super) static NAMED_PHASE_LATENCY: OnceLock<Mutex<BTreeMap<&'static str, [u64; 8]>>> =
    OnceLock::new();

/// Bucket bounds for request latency, where sub-millisecond resolution matters.
pub(super) const LATENCY_BUCKETS_US: [u64; 8] =
    [100, 500, 1_000, 5_000, 10_000, 50_000, 250_000, u64::MAX];

/// Bucket bounds for phase latency, where a phase is milliseconds to hundreds of
/// milliseconds.
///
/// The request buckets are wrong for phases: a 63 ms network phase and a 240 ms
/// cleanup would both land in the same 250 ms bucket, so the percentile would
/// report the same upper bound for both and say nothing about either. These bounds
/// put the resolution where the phases actually are.
pub(super) const PHASE_BUCKETS_US: [u64; 8] = [
    1_000,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    u64::MAX,
];

/// p50, p95, and p99 of a histogram, as the upper bound of the bucket each falls
/// into.
///
/// The samples are kept in bounded buckets rather than a list, so a percentile is
/// only known to within its bucket. Reporting the bucket's upper bound is
/// therefore an upper estimate, not a measurement: the buckets are chosen so that
/// the estimate is useful (100 µs, 500 µs, 1 ms, 5 ms, 10 ms, 50 ms, 250 ms), and
/// the open-ended top bucket is reported as the bound below it, meaning "at least
/// this". A caller that needs exact values reads the phase timing lines, which
/// carry the operation id and the exact microsecond count.
pub(super) fn percentile_values(
    histogram: &[AtomicU64; 8],
    buckets: &[u64; 8],
) -> serde_json::Value {
    serde_json::json!({
        "p50_us": percentile_us(histogram, 50, buckets),
        "p95_us": percentile_us(histogram, 95, buckets),
        "p99_us": percentile_us(histogram, 99, buckets),
    })
}

pub(super) fn percentile_us(
    histogram: &[AtomicU64; 8],
    percent: u64,
    buckets: &[u64; 8],
) -> Option<u64> {
    percentile_of_buckets(&histogram_values(histogram), percent, buckets)
}

pub(super) fn percentile_of_buckets(
    counts: &[u64],
    percent: u64,
    buckets: &[u64; 8],
) -> Option<u64> {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    // The smallest sample count whose bucket already contains the percentile.
    let target = (total.saturating_mul(percent)).div_ceil(100).max(1);
    let mut cumulative = 0u64;
    for (index, count) in counts.iter().enumerate() {
        cumulative += count;
        if cumulative >= target {
            return Some(bucket_upper_bound(index, buckets));
        }
    }
    None
}

pub(super) fn bucket_upper_bound(index: usize, buckets: &[u64; 8]) -> u64 {
    // The top bucket has no upper bound, so the bound below it is the smallest
    // value the samples in it are known to exceed.
    match buckets.get(index) {
        Some(&limit) if limit != u64::MAX => limit,
        _ => buckets[buckets.len() - 2],
    }
}

pub(super) fn bucket_index(elapsed_us: u64, buckets: &[u64; 8]) -> usize {
    buckets
        .iter()
        .position(|limit| elapsed_us <= *limit)
        .unwrap_or(buckets.len() - 1)
}

pub(super) fn record_histogram(histogram: &[AtomicU64; 8], elapsed_us: u64, buckets: &[u64; 8]) {
    let bucket = bucket_index(elapsed_us, buckets);
    histogram[bucket].fetch_add(1, Ordering::Relaxed);
}

pub(super) fn histogram_values(histogram: &[AtomicU64; 8]) -> Vec<u64> {
    histogram
        .iter()
        .map(|bucket| bucket.load(Ordering::Relaxed))
        .collect()
}

/// Per-phase sample counts and percentiles.
///
/// This is the critical-path breakdown: each named phase reports how long it took
/// across every operation, so a phase that dominates one operation is visible
/// against a phase that dominates another. The counts are kept alongside the
/// percentiles because a percentile of four samples is not the same evidence as a
/// percentile of four hundred.
pub(super) fn named_phase_values() -> serde_json::Value {
    let phases = NAMED_PHASE_LATENCY
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map(|phases| phases.clone())
        .unwrap_or_default();
    let mut object = serde_json::Map::new();
    for (name, counts) in phases {
        object.insert(
            name.to_string(),
            serde_json::json!({
                "samples": counts.iter().sum::<u64>(),
                "buckets_us": counts,
                "p50_us": percentile_of_buckets(&counts, 50, &PHASE_BUCKETS_US),
                "p95_us": percentile_of_buckets(&counts, 95, &PHASE_BUCKETS_US),
                "p99_us": percentile_of_buckets(&counts, 99, &PHASE_BUCKETS_US),
            }),
        );
    }
    serde_json::Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::{
        bucket_index, histogram_values, percentile_of_buckets, record_histogram,
        LATENCY_BUCKETS_US, PHASE_BUCKETS_US, REQUEST_LATENCY,
    };

    #[test]
    fn latency_histogram_places_samples_in_bounded_buckets() {
        let before = histogram_values(&REQUEST_LATENCY);
        record_histogram(&REQUEST_LATENCY, LATENCY_BUCKETS_US[0], &LATENCY_BUCKETS_US);
        record_histogram(&REQUEST_LATENCY, LATENCY_BUCKETS_US[7], &LATENCY_BUCKETS_US);
        let after = histogram_values(&REQUEST_LATENCY);
        assert_eq!(after[0], before[0] + 1);
        assert_eq!(after[7], before[7] + 1);
    }

    #[test]
    fn a_percentile_reports_the_bound_of_the_bucket_it_falls_into() {
        // Nine samples at or below 100 µs and one at or below 5 ms: the median is
        // in the first bucket and the p99 in the fourth.
        let mut counts = [0u64; 8];
        counts[0] = 9;
        counts[3] = 1;
        assert_eq!(
            percentile_of_buckets(&counts, 50, &LATENCY_BUCKETS_US),
            Some(100)
        );
        assert_eq!(
            percentile_of_buckets(&counts, 95, &LATENCY_BUCKETS_US),
            Some(5_000)
        );
        assert_eq!(
            percentile_of_buckets(&counts, 99, &LATENCY_BUCKETS_US),
            Some(5_000)
        );
    }

    #[test]
    fn the_open_ended_bucket_reports_the_bound_below_it() {
        // A sample in the top bucket is only known to be at least the bound below
        // it, so the estimate must not claim an upper bound that does not exist.
        let mut counts = [0u64; 8];
        counts[7] = 1;
        assert_eq!(
            percentile_of_buckets(&counts, 50, &LATENCY_BUCKETS_US),
            Some(250_000)
        );
    }

    #[test]
    fn a_histogram_with_no_samples_has_no_percentile() {
        assert_eq!(
            percentile_of_buckets(&[0u64; 8], 50, &LATENCY_BUCKETS_US),
            None
        );
    }

    #[test]
    fn phase_buckets_separate_the_phases_the_request_buckets_lumped_together() {
        // A 63 ms network phase and a 240 ms cleanup must not report the same
        // estimate: that is what the request ladder did, and it made the phase
        // percentile useless.
        let mut counts = [0u64; 8];
        counts[bucket_index(63_000, &PHASE_BUCKETS_US)] += 1;
        assert_eq!(
            percentile_of_buckets(&counts, 50, &PHASE_BUCKETS_US),
            Some(100_000)
        );
        let mut counts = [0u64; 8];
        counts[bucket_index(240_000, &PHASE_BUCKETS_US)] += 1;
        assert_eq!(
            percentile_of_buckets(&counts, 50, &PHASE_BUCKETS_US),
            Some(250_000)
        );
    }
}
