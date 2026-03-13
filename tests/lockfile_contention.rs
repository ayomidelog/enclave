use std::process::Command;

mod common;

#[test]
fn sequential_sandbox_list_no_lock_contention() {
    for i in 0..3 {
        let output = Command::new(common::enclave_bin())
            .args(["list"])
            .output()
            .unwrap_or_else(|e| panic!("iteration {i}: {e}"));

        let _code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "iteration {i}: should not panic: {stderr}"
        );
        assert!(
            !stderr.contains("failed to lock"),
            "iteration {i}: should not have lock contention: {stderr}"
        );
    }
}

#[test]
fn sequential_workspace_list_no_lock_contention() {
    for i in 0..3 {
        let output = Command::new(common::enclave_bin())
            .args(["workspace", "list"])
            .output()
            .unwrap_or_else(|e| panic!("iteration {i}: {e}"));

        let _code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "iteration {i}: should not panic: {stderr}"
        );
    }
}

#[test]
fn rapid_doctor_calls_no_lock_issue() {
    for i in 0..3 {
        let output = Command::new(common::enclave_bin())
            .args(["doctor"])
            .output()
            .unwrap_or_else(|e| panic!("iteration {i}: {e}"));

        let _code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "iteration {i}: doctor should not panic: {stderr}"
        );
    }
}

#[test]
fn rapid_health_calls_no_lock_issue() {
    for i in 0..3 {
        let output = Command::new(common::enclave_bin())
            .args(["health"])
            .output()
            .unwrap_or_else(|e| panic!("iteration {i}: {e}"));

        let _code = output.status.code();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "iteration {i}: health should not panic: {stderr}"
        );
    }
}

#[test]
fn concurrent_sandbox_list_is_safe() {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                Command::new(common::enclave_bin())
                    .args(["list"])
                    .output()
                    .expect("failed to run command")
            })
        })
        .collect();

    for (i, handle) in handles.into_iter().enumerate() {
        let output = handle
            .join()
            .unwrap_or_else(|e| panic!("thread {i} panicked: {e:?}"));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "thread {i}: should not panic: {stderr}"
        );
    }
}

#[test]
fn concurrent_doctor_is_safe() {
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                Command::new(common::enclave_bin())
                    .args(["doctor"])
                    .output()
                    .expect("failed to run command")
            })
        })
        .collect();

    for (i, handle) in handles.into_iter().enumerate() {
        let output = handle
            .join()
            .unwrap_or_else(|e| panic!("thread {i} panicked: {e:?}"));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "thread {i}: should not panic: {stderr}"
        );
    }
}

#[test]
fn registry_repair_help_available() {
    let output = Command::new(common::enclave_bin())
        .args(["registry", "repair", "--help"])
        .output()
        .expect("failed to run command");

    assert!(
        output.status.success(),
        "registry repair --help should succeed: {:?}",
        output.status
    );
}
