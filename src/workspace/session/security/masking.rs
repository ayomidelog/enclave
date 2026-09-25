//! Hiding host kernel information from a workspace session.
//!
//! Several paths under `/proc` and `/sys` describe the host: the kernel symbol
//! table, the loaded module list, the kernel keyring, and the debug and security
//! filesystems. A workspace does not need any of them, and leaving them readable
//! hands out information about the machine that would help an escape. Each one is
//! covered by a bind mount of an empty file or directory, remounted read-only so
//! the workspace cannot write through it.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use nix::mount::{mount, MsFlags};

/// Where the empty sources the masks bind from are created. It lives under
/// `/run/enclave`, which the session mounts as a private tmpfs, so the sources
/// are never visible on the host.
const MASK_ROOT: &str = "/run/enclave/masked";

pub(super) const MASK_FILE_TARGETS: &[&str] = &[
    "/proc/kallsyms",
    "/proc/kcore",
    "/proc/keys",
    "/proc/modules",
    "/proc/sched_debug",
    "/proc/timer_list",
];

pub(super) const MASK_DIR_TARGETS: &[&str] =
    &["/sys/kernel/debug", "/sys/kernel/security", "/sys/module"];

/// Bind an empty file over each file target and an empty directory over each
/// directory target.
///
/// A target that does not exist is skipped: the kernel may not have the
/// filesystem it belongs to mounted, and there is nothing to hide in that case.
pub(crate) fn mask_runtime_paths() -> Result<()> {
    let file_root = Path::new(MASK_ROOT).join("files");
    let dir_root = Path::new(MASK_ROOT).join("dirs");
    fs::create_dir_all(&file_root)
        .with_context(|| format!("failed to create {}", file_root.display()))?;
    fs::create_dir_all(&dir_root)
        .with_context(|| format!("failed to create {}", dir_root.display()))?;

    for target in MASK_FILE_TARGETS {
        let target = Path::new(target);
        if !target.exists() {
            continue;
        }
        let source = file_root.join(mask_name_for_path(target));
        fs::write(&source, b"").with_context(|| format!("failed to write {}", source.display()))?;
        bind_mask(&source, target)?;
    }

    for target in MASK_DIR_TARGETS {
        let target = Path::new(target);
        if !target.exists() {
            continue;
        }
        let source = dir_root.join(mask_name_for_path(target));
        fs::create_dir_all(&source)
            .with_context(|| format!("failed to create {}", source.display()))?;
        bind_mask(&source, target)?;
    }

    Ok(())
}

/// Bind `source` over `target`, then remount it read-only.
///
/// The first mount is a plain bind, so the target is covered by the empty
/// source. The second is a bind remount that adds `MS_RDONLY` to the existing
/// mount, which is how a bind mount is made read-only: passing `MS_RDONLY` on the
/// first call would apply to the source filesystem instead.
fn bind_mask(source: &Path, target: &Path) -> Result<()> {
    mount(
        Some(source),
        target,
        Option::<&str>::None,
        MsFlags::MS_BIND,
        Option::<&str>::None,
    )
    .with_context(|| {
        format!(
            "failed to bind mask {} over {}",
            source.display(),
            target.display()
        )
    })?;
    mount(
        Option::<&str>::None,
        target,
        Option::<&str>::None,
        MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
        Option::<&str>::None,
    )
    .with_context(|| {
        format!(
            "failed to remount masked path {} read-only",
            target.display()
        )
    })?;
    Ok(())
}

/// A filesystem-safe name for the empty source backing a mask, derived from the
/// path being masked. `/proc/kallsyms` becomes `proc__kallsyms`.
fn mask_name_for_path(path: &Path) -> String {
    path.to_string_lossy()
        .trim_matches('/')
        .replace('/', "__")
        .replace('.', "_")
}
