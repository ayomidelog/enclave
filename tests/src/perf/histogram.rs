use super::{
    bucket_index, histogram_values, percentile_of_buckets, record_histogram, LATENCY_BUCKETS_US,
    PHASE_BUCKETS_US, REQUEST_LATENCY,
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
