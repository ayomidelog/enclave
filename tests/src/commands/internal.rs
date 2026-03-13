use super::{open_ready_file_via_old_root, runtime_tmpfs_mount_flags, RUNTIME_TMPFS_DATA};
use std::fs;
use std::path::{Path, PathBuf};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "enclave-internal-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        if path.exists() {
            fs::remove_dir_all(&path).expect("remove stale temp dir");
        }
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).expect("remove temp dir");
        }
    }
}

fn temp_dir(label: &str) -> TempDir {
    TempDir::new(label)
}

#[test]
fn open_ready_file_via_old_root_maps_absolute_path_under_old_root() {
    let base = temp_dir("ready-old-root");
    let old_root = base.path().join(".old_root");
    fs::create_dir_all(&old_root).expect("create old root");

    let ready_file = PathBuf::from("/tmp/enclave-ready/signal.txt");
    let mapped = old_root.join("tmp/enclave-ready/signal.txt");
    let outside = base.path().join("tmp/enclave-ready/signal.txt");

    let _handle =
        open_ready_file_via_old_root(&old_root, &ready_file).expect("open mapped ready file");

    assert!(mapped.exists(), "expected mapped file under old_root");
    assert!(
        !outside.exists(),
        "absolute ready path should be remapped through old_root"
    );
}

#[test]
fn open_ready_file_via_old_root_rejects_relative_paths() {
    let base = temp_dir("ready-relative");
    let old_root = base.path().join(".old_root");
    fs::create_dir_all(&old_root).expect("create old root");

    let err = open_ready_file_via_old_root(&old_root, Path::new("tmp/ready.txt"))
        .expect_err("relative ready path must fail");
    assert!(err.to_string().contains("must be absolute"));
}

#[test]
fn open_ready_file_via_old_root_rejects_parent_traversal() {
    let base = temp_dir("ready-traversal");
    let old_root = base.path().join(".old_root");
    fs::create_dir_all(&old_root).expect("create old root");

    let err = open_ready_file_via_old_root(&old_root, Path::new("/tmp/../escape/ready.txt"))
        .expect_err("traversal ready path must fail");
    assert!(err
        .to_string()
        .contains("must not contain traversal components"));
}

#[test]
fn runtime_tmpfs_mount_options_split_vfs_flags_from_fs_data() {
    let flags = runtime_tmpfs_mount_flags();
    assert!(flags.contains(nix::mount::MsFlags::MS_NODEV));
    assert!(flags.contains(nix::mount::MsFlags::MS_NOSUID));
    assert!(flags.contains(nix::mount::MsFlags::MS_NOEXEC));
    assert_eq!(RUNTIME_TMPFS_DATA, "mode=700");
}
