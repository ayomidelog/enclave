use super::*;

#[test]
fn allows_requests_within_limit() {
    let limiter = RateLimiter::new(Duration::from_secs(60), 3);
    assert!(limiter.allow(1000));
    assert!(limiter.allow(1000));
    assert!(limiter.allow(1000));
}

#[test]
fn rejects_requests_exceeding_limit() {
    let limiter = RateLimiter::new(Duration::from_secs(60), 2);
    assert!(limiter.allow(1000));
    assert!(limiter.allow(1000));
    assert!(!limiter.allow(1000));
}

#[test]
fn tracks_uids_independently() {
    let limiter = RateLimiter::new(Duration::from_secs(60), 1);
    assert!(limiter.allow(1000));
    assert!(limiter.allow(1001));
    assert!(!limiter.allow(1000));
    assert!(!limiter.allow(1001));
}

#[test]
fn resets_after_window_expires() {
    let limiter = RateLimiter::new(Duration::from_millis(100), 1);
    assert!(limiter.allow(1000));
    assert!(!limiter.allow(1000));
    std::thread::sleep(Duration::from_millis(200));
    assert!(limiter.allow(1000));
}

#[test]
fn global_limit_rejects_when_total_exceeded() {
    let limiter = RateLimiter::with_global_limit(Duration::from_secs(60), 5, 3);
    assert!(limiter.allow(1000));
    assert!(limiter.allow(1001));
    assert!(limiter.allow(1002));
    assert!(!limiter.allow(1003));
}

#[test]
fn global_limit_resets_after_window() {
    let limiter = RateLimiter::with_global_limit(Duration::from_millis(100), 10, 2);
    assert!(limiter.allow(1000));
    assert!(limiter.allow(1001));
    assert!(!limiter.allow(1002));
    std::thread::sleep(Duration::from_millis(200));
    assert!(limiter.allow(1000));
}

#[test]
fn per_uid_limit_still_enforced_with_global() {
    let limiter = RateLimiter::with_global_limit(Duration::from_secs(60), 2, 100);
    assert!(limiter.allow(1000));
    assert!(limiter.allow(1000));
    assert!(!limiter.allow(1000));
    assert!(limiter.allow(1001));
}

#[test]
fn recovers_from_poisoned_mutex() {
    let limiter = RateLimiter::new(Duration::from_secs(60), 2);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = limiter.global.lock().expect("lock global");
        panic!("poison global lock");
    }));
    assert!(limiter.allow(1000));
}
