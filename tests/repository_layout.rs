use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn scripts_directory_contains_only_release_scripts() {
    let scripts_dir = repo_root().join("scripts");
    let mut names = fs::read_dir(scripts_dir)
        .expect("read scripts dir")
        .map(|entry| {
            entry
                .expect("script entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "install-remote.sh".to_string(),
            "install.sh".to_string(),
            "uninstall.sh".to_string(),
            "update.sh".to_string()
        ]
    );
}

#[test]
fn install_remote_targets_release_repository() {
    let installer = fs::read_to_string(repo_root().join("scripts/install-remote.sh"))
        .expect("read install-remote.sh");
    assert!(installer.contains("https://github.com/ayomidelog/enclave.git"));
    assert!(!installer.contains("Enclave-Beta"));
    assert!(installer.contains("cargo build --release --manifest-path"));
    assert!(!installer.contains("| grep"));
}

#[test]
fn test_suite_entrypoints_exist() {
    for path in [
        repo_root().join("tests/unit_suite.rs"),
        repo_root().join("tests/integration_suite.rs"),
        repo_root().join("tests/stress_suite.rs"),
    ] {
        assert!(
            path.is_file(),
            "missing test suite entrypoint {}",
            path.display()
        );
    }
}

#[test]
fn rust_and_script_files_have_no_unresolved_todo_markers() {
    let roots = [
        (repo_root().join("src"), "rs"),
        (repo_root().join("tests"), "rs"),
        (repo_root().join("scripts"), "sh"),
        (repo_root().join(".github/workflows"), "yml"),
    ];
    for (root, extension) in roots {
        for path in collect_files(&root, extension) {
            let text = fs::read_to_string(&path).expect("read file");
            for (line_number, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                let is_comment = if extension == "rs" {
                    trimmed.starts_with("//")
                        || trimmed.starts_with("/*")
                        || trimmed.starts_with("*/")
                } else {
                    trimmed.starts_with('#') && !trimmed.starts_with("#!")
                };
                let has_unresolved_marker = trimmed.contains("TODO")
                    || trimmed.contains("FIXME")
                    || trimmed.contains("XXX");
                assert!(
                    !(is_comment && has_unresolved_marker),
                    "unresolved marker left in {}:{}",
                    path.display(),
                    line_number + 1
                );
            }
        }
    }
}

#[test]
fn release_workflow_runs_verification_before_release() {
    let workflow = fs::read_to_string(repo_root().join(".github/workflows/release.yml"))
        .expect("read release workflow");
    assert!(workflow.contains("workflow_dispatch:"));
    assert!(workflow.contains("cargo fmt --all -- --check"));
    assert!(workflow.contains("cargo check --all-targets"));
    assert!(workflow.contains("cargo clippy --all-targets -- -D warnings"));
    assert!(workflow.contains("cargo test --all-targets -- --skip integration:: --skip stress::"));
    assert!(workflow.contains("cargo build --release --locked"));
    assert!(workflow.contains("softprops/action-gh-release@v2"));
}

/// The privileged suite is wired into CI, and the wiring stays complete.
///
/// The suite is the only coverage of what a lifecycle operation does to the
/// host, and it is `#[ignore]`d so `cargo test` never runs it. That makes the
/// workflow the only thing that does, and a step removed from it would take the
/// coverage away without failing anything else. This pins the parts that matter:
/// the runner installs the tools the suite shells out to, and the suite is run
/// through the script that checks for them first.
#[test]
fn continuous_integration_runs_the_privileged_suite() {
    let workflow = fs::read_to_string(repo_root().join(".github/workflows/rust.yml"))
        .expect("read the rust workflow");
    assert!(
        workflow.contains("tools/ci/privileged-suite.sh"),
        "the privileged suite must be run through the script that checks the host first"
    );
    // The suite shells out to the same tools the daemon does, and builds its
    // fixture rootfs from a static shell, so a runner without them fails in the
    // middle of a test rather than before it.
    for tool in [
        "busybox-static",
        "iproute2",
        "iptables",
        "util-linux",
        "e2fsprogs",
    ] {
        assert!(
            workflow.contains(tool),
            "the privileged job must install {tool}"
        );
    }
    // sudo resets PATH, so the toolchain the repository pins has to be passed
    // through explicitly or the job builds with a cargo too old for the lockfile.
    assert!(
        workflow.contains("sudo -E env"),
        "the privileged job must run with the pinned toolchain on PATH"
    );
}

/// The script that runs the privileged suite exists and is executable.
///
/// A workflow referencing a script that is not committed, or is committed without
/// its executable bit, fails on the runner rather than here.
#[test]
fn the_privileged_suite_runner_is_committed_and_executable() {
    use std::os::unix::fs::PermissionsExt;

    let path = repo_root().join("tools/ci/privileged-suite.sh");
    let metadata = fs::metadata(&path)
        .unwrap_or_else(|error| panic!("the privileged suite runner is missing: {error}"));
    assert!(
        metadata.permissions().mode() & 0o111 != 0,
        "{} must be executable",
        path.display()
    );
    let text = fs::read_to_string(&path).expect("read the privileged suite runner");
    // The script is what turns a missing tool into a statement about the host,
    // so it has to name the packages rather than only the binaries.
    for expected in ["busybox", "iproute2", "iptables", "e2fsprogs"] {
        assert!(
            text.contains(expected),
            "the runner must name {expected} as a host requirement"
        );
    }
}
