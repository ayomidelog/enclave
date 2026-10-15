//! Running independent jobs on a bounded pool of threads.
//!
//! Four places in the tree fan a list of jobs out over a few threads: starting many
//! workspaces, tearing many workspace networks down, listing process status, and
//! collecting workspace stats. They are all the same shape — the work is dominated by
//! host commands and mounts, so a handful of threads overlaps the waiting without
//! oversubscribing a small host — and they had each grown their own copy of the queue,
//! the result collection, and the worker count. The copies had drifted: three different
//! worker-count policies, two different result carriers, and three different answers to
//! a worker that panics (one panicked the caller, one fabricated a failure, one dropped
//! the job). This module is the one implementation.

use std::collections::VecDeque;
use std::sync::Mutex;

/// The largest worker count any of the batches accepts from its environment.
///
/// A batch is bounded work on one host, and a count above this is a caller who
/// mistyped rather than a host that needs it.
const MAX_WORKERS: usize = 64;

/// Run `job` for every item, on at most `workers` threads, and collect the results.
///
/// The results come back in the order the items were given, which is what lets a
/// caller report a per-item outcome against the item the caller named. An entry is
/// `None` only if the worker that claimed it did not store a result, which a caller
/// should report rather than treat as success.
///
/// A poisoned queue is taken back rather than abandoned: the jobs in it are plain
/// data, and leaving them unclaimed would give the caller holes it can only describe.
/// The threads are scoped, so nothing outlives the call.
pub(crate) fn map_bounded<T, R, F>(items: Vec<T>, workers: usize, job: F) -> Vec<Option<R>>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    let count = items.len();
    if count == 0 {
        return Vec::new();
    }
    let workers = workers.clamp(1, count);
    let queue = Mutex::new(VecDeque::from_iter(items.into_iter().enumerate()));
    let results = Mutex::new(
        std::iter::repeat_with(|| None)
            .take(count)
            .collect::<Vec<Option<R>>>(),
    );

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let next = queue
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pop_front();
                let Some((index, item)) = next else {
                    return;
                };
                let result = job(item);
                let mut results = results
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                results[index] = Some(result);
            });
        }
    });

    results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The count `variable` names, or `default` when it names nothing usable.
///
/// A value that does not parse, or that is outside 1 to [`MAX_WORKERS`], is ignored
/// rather than honoured: a mistyped override that silently became one worker or a
/// thousand is worse than one that falls back to the count the code chose.
pub(crate) fn env_worker_count(variable: &str, default: usize) -> usize {
    std::env::var(variable)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=MAX_WORKERS).contains(value))
        .unwrap_or(default)
}

/// The number of workers a batch should use.
///
/// `variable` overrides `default` when it names a count in range, which is what lets
/// a caller who has measured their own host say so. The result is never more than the
/// number of jobs, so a batch of two does not spawn four threads.
pub(crate) fn worker_count(variable: &str, jobs: usize, default: usize) -> usize {
    if jobs == 0 {
        return 0;
    }
    env_worker_count(variable, default).clamp(1, jobs)
}

/// The host's parallelism, or one when the kernel will not say.
///
/// The batches that use this want "one worker per core" as their default, and a host
/// that cannot report a count gets the single worker that is always correct.
pub(crate) fn parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
}

#[cfg(test)]
#[path = "../tests/src/parallel.rs"]
mod tests;
