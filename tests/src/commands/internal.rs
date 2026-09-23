use super::{
    open_ready_file_via_old_root, runtime_tmpfs_mount_flags, tmp_directory_is_usable,
    validate_workspace_file_target, verify_workspace_tmp_mount, workspace_old_root_path,
    DirectoryIdentity, RUNTIME_TMPFS_DATA, WORKSPACE_TMP_DATA,
};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

struct TempDir {
    path: PathBuf,
}

#[test]
fn workspace_old_root_path_is_unique_and_rejects_unsafe_ids() {
    let rootfs = Path::new("/state/rootfs");
    assert_eq!(
        workspace_old_root_path(rootfs, "workspace-abc_123").unwrap(),
        PathBuf::from("/state/rootfs/.old_root-workspace-abc_123")
    );
    assert!(workspace_old_root_path(rootfs, "../escape").is_err());
}

#[test]
fn root_overlay_put_old_path_matches_the_path_used_after_pivot() {
    let merged = Path::new("/state/workspaces/ws/root-merged");
    let host_old_root = workspace_old_root_path(merged, "workspace-abc_123").unwrap();
    let pivoted_old_root = Path::new("/").join(host_old_root.file_name().unwrap());
    assert_eq!(host_old_root, merged.join(".old_root-workspace-abc_123"));
    assert_eq!(pivoted_old_root, Path::new("/.old_root-workspace-abc_123"));
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
    assert_eq!(WORKSPACE_TMP_DATA, "mode=1777");
}

#[test]
fn workspace_tmp_probe_rejects_unlinked_or_non_sticky_directories() {
    let base = temp_dir("tmp-validation");
    let linked = base.path().join("linked");
    fs::create_dir(&linked).expect("create linked temp directory");
    fs::set_permissions(&linked, fs::Permissions::from_mode(0o1777)).expect("set sticky mode");
    assert!(tmp_directory_is_usable(&linked));

    fs::set_permissions(&linked, fs::Permissions::from_mode(0o0777)).expect("clear sticky mode");
    assert!(!tmp_directory_is_usable(&linked));

    fs::set_permissions(&linked, fs::Permissions::from_mode(0o1777)).expect("restore sticky mode");
    fs::remove_dir(&linked).expect("unlink temp directory");
    assert!(!tmp_directory_is_usable(&linked));
}

#[test]
fn workspace_tmp_mount_verification_checks_backing_inode() {
    let base = temp_dir("tmp-identity");
    let target = base.path().join("target");
    fs::create_dir(&target).expect("create temp target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o1777)).expect("set sticky mode");
    let metadata = fs::metadata(&target).expect("stat target");
    let identity = DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    verify_workspace_tmp_mount(&target, Some(identity)).expect("matching inode should pass");
    let wrong_identity = DirectoryIdentity {
        device: identity.device,
        inode: identity.inode.wrapping_add(1),
    };
    assert!(verify_workspace_tmp_mount(&target, Some(wrong_identity)).is_err());
}

#[test]
fn workspace_file_target_requires_safe_home_path() {
    assert!(validate_workspace_file_target("/home/stage/file").is_ok());
    assert!(validate_workspace_file_target("/tmp/file").is_err());
    assert!(validate_workspace_file_target("/home/../tmp/file").is_err());
}
