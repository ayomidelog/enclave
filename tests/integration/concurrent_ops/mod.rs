//! What two lifecycle operations racing on one workspace are allowed to leave behind.
//!
//! The daemon serializes these pairs with a workspace lease, so the library only owes
//! callers the narrower guarantee the registry lock gives it: the mutations are ordered,
//! the host work is not. Each pair is driven from two threads over several rounds,
//! because which of the two settles is decided by the scheduler and one round only ever
//! shows one of the orders. Neither is required to succeed; what is required is that the
//! state left behind is a settled one that agrees with the host.
//!
//! The two pairs are separate modules because they are about different invariants. A
//! resize racing a stop has to leave the image and the filesystem inside it agreeing; a
//! restore racing a destroy has to leave either a workspace that works or no workspace at
//! all. The helpers they share are here.
//!
//! A third pair is here for the same reason: a wipe destroys every workspace while a
//! start brings one up, and the outcome has to be a workspace that is either fully gone
//! or fully running rather than one caught between the two.

mod resize_stop;
mod restore_destroy;
mod wipe_start;

use std::fs;
use std::path::Path;

use enclave::sandbox::{create_sandbox, start_sandbox, BootstrapMethod};

use super::support::{ext4_filesystem_size, prepare_cached_rootfs, SandboxCleanup};

/// The size of the workspace image on disk, and of the filesystem inside it.
///
/// The two are separate facts. Growing the image is a truncate and cannot fail; growing
/// the filesystem inside it is a resize of a live ext4 and can. A workspace left with an
/// image larger than its filesystem reports readiness failures on every later start,
/// which is the failure this pair of numbers exists to catch.
fn image_and_filesystem_bytes(workspace_path: &str) -> (u64, u64) {
    let image = Path::new(workspace_path).join("fs.img");
    let image_bytes = fs::metadata(&image)
        .expect("read the disk image metadata")
        .len();
    let filesystem_bytes = ext4_filesystem_size(&image).expect("read the ext4 filesystem size");
    (image_bytes, filesystem_bytes)
}

/// A sandbox with a cached rootfs, which both scenarios build their workspace in.
fn cached_rootfs_sandbox(state: &Path, name: &str, cleanup: &mut SandboxCleanup) -> String {
    prepare_cached_rootfs(state, "bookworm");
    let sandbox = create_sandbox(
        state,
        "debootstrap",
        name,
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(state, &sandbox.id).expect("start sandbox");
    sandbox.id
}
