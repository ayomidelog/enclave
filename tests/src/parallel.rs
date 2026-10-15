use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn results_come_back_in_the_order_the_items_were_given() {
    // The items are deliberately not in the order the work finishes, so a result
    // list that happened to be right by luck would fail here.
    let items: Vec<u32> = (0..64).collect();
    let results = map_bounded(items.clone(), 4, |item| item * 2);

    assert_eq!(results.len(), items.len());
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result.as_ref().copied(), Some(index as u32 * 2));
    }
}

#[test]
fn an_empty_batch_returns_no_results() {
    let results = map_bounded(Vec::<u32>::new(), 4, |item| item);
    assert!(results.is_empty());
}

/// Every item is run exactly once, whatever the worker count.
#[test]
fn every_item_runs_once_at_any_worker_count() {
    for workers in [1, 2, 3, 8, 64] {
        let runs = AtomicUsize::new(0);
        let results = map_bounded((0..16).collect::<Vec<u32>>(), workers, |item| {
            runs.fetch_add(1, Ordering::Relaxed);
            item
        });
        assert_eq!(runs.load(Ordering::Relaxed), 16, "workers={workers}");
        assert!(
            results.iter().all(Option::is_some),
            "a result is missing at workers={workers}"
        );
    }
}

/// A batch never spawns more threads than it has jobs.
#[test]
fn the_worker_count_is_never_more_than_the_jobs() {
    assert_eq!(worker_count("ENCLAVE_TEST_UNSET_WORKERS", 2, 64), 2);
    assert_eq!(worker_count("ENCLAVE_TEST_UNSET_WORKERS", 0, 64), 0);
    assert_eq!(worker_count("ENCLAVE_TEST_UNSET_WORKERS", 8, 4), 4);
}

/// An override is honoured only when it is a count in range.
///
/// The variable is process-wide, so this test sets and clears it around its own
/// assertions; the value is chosen so no other test could be reading it.
#[test]
fn an_override_is_used_only_when_it_is_a_usable_count() {
    let variable = "ENCLAVE_TEST_PARALLEL_WORKERS";

    std::env::set_var(variable, "3");
    assert_eq!(worker_count(variable, 10, 4), 3);

    // Above the cap, zero, and not a number all fall back to the default.
    for unusable in ["0", "65", "lots", "", "-1"] {
        std::env::set_var(variable, unusable);
        assert_eq!(
            worker_count(variable, 10, 4),
            4,
            "{unusable:?} must be ignored"
        );
    }

    std::env::remove_var(variable);
    assert_eq!(worker_count(variable, 10, 4), 4);
}

/// The host always reports at least one worker.
#[test]
fn parallelism_is_at_least_one() {
    assert!(parallelism() >= 1);
}
