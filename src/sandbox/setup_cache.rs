use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use super::types::SandboxMetadata;

/// Version tag for the setup cache key scheme.
///
/// Bump this when the set of inputs below changes, so markers written by an
/// older key can never be mistaken for a hit under the new one.
const KEY_SCHEME: &str = "enclave-setup-key-v2";

/// The cache key for one sandbox's ordered setup command list.
///
/// The key covers every input that can change what a setup command does: the
/// sandbox definition, the base rootfs the command runs against, the command
/// list in order, and the Enclave version that runs it. Leaving any of them out
/// means a change to that input silently reuses a result computed under the old
/// one, which is the failure mode a setup cache is supposed to prevent.
///
/// It is computed by the daemon rather than the caller because the daemon owns
/// the sandbox metadata and the rootfs, and neither is reliably known to a CLI
/// that is talking to a sandbox it did not create.
pub(crate) fn key_for(state_dir: &Path, metadata: &SandboxMetadata, commands: &[String]) -> String {
    let mut digest = Sha256::new();
    for field in [
        KEY_SCHEME,
        env!("CARGO_PKG_VERSION"),
        metadata.name.as_str(),
        metadata.suite.as_str(),
        metadata.mirror.as_str(),
        &metadata.bootstrap_method.to_string(),
    ] {
        digest.update(field.as_bytes());
        digest.update([0]);
    }
    digest.update(rootfs_identity(state_dir, metadata).as_bytes());
    digest.update([0]);
    for command in commands {
        digest.update([0xff]);
        digest.update(command.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

/// Identity of the rootfs a setup command runs against.
///
/// For a shared-base sandbox the writable view is an overlay whose lower layer
/// is the cached rootfs, so the lower layer is what identifies the content. The
/// cache index records a content digest for each entry when it is registered, so
/// that digest is the identity whenever it is available: it survives an entry
/// being replaced in place, which a device and inode pair does not.
fn rootfs_identity(state_dir: &Path, metadata: &SandboxMetadata) -> String {
    if let Some(lower) = metadata.rootfs_lower_path.as_deref() {
        let cache_root = crate::sandbox::rootfs_cache_dir(state_dir);
        if let Some(digest) =
            crate::sandbox::rootfs_cache_content_identity(&cache_root, Path::new(lower))
        {
            return format!("cache:{digest}");
        }
    }
    let path = metadata
        .rootfs_lower_path
        .as_deref()
        .unwrap_or(metadata.rootfs_path.as_str());
    directory_identity(Path::new(path))
}

/// A best-effort identity for a rootfs directory that the cache index does not
/// describe.
///
/// A device and inode pair is not enough on its own: removing a directory and
/// recreating it can reuse both, and the kernel's coarse timestamp granularity
/// means a fast replace can even land on the same modification time. The
/// top-level entries are hashed alongside it — name, inode, size, and timestamp —
/// so a tree that changed is very unlikely to look unchanged. This is a fallback
/// for a rootfs the cache index cannot describe; a shared cache layer uses its
/// recorded content digest instead.
fn directory_identity(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let Ok(stat) = fs::metadata(path) else {
        // An unreadable rootfs is a different failure reported by the setup
        // itself; the path alone still keeps two sandboxes from sharing a key.
        return path.to_string_lossy().into_owned();
    };
    let mut digest = Sha256::new();
    digest.update(format!(
        "{}:{:x}:{:x}:{}:{}:{}",
        path.display(),
        stat.dev(),
        stat.ino(),
        stat.len(),
        stat.mtime(),
        stat.mtime_nsec()
    ));
    if let Ok(entries) = fs::read_dir(path) {
        let mut listed: Vec<(String, u64, u64, i64, i64)> = entries
            .flatten()
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some((
                    entry.file_name().to_string_lossy().into_owned(),
                    metadata.ino(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                ))
            })
            .collect();
        listed.sort();
        for (name, inode, size, seconds, nanos) in listed {
            digest.update(format!("{name}:{inode:x}:{size}:{seconds}:{nanos}"));
        }
    }
    format!("{:x}", digest.finalize())
}

/// Where the completion marker for one setup command lives.
///
/// The marker belongs to the sandbox, not to the rootfs path, because the rootfs
/// path is the mounted root while the sandbox is active and the plain directory
/// while it is stopped. Anchoring the marker to the sandbox directory means the
/// same result is found whichever state the sandbox is in.
pub(crate) fn marker_path(sandbox_path: &Path, digest: &str, index: u64) -> Result<PathBuf> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("setup digest must be a 64-character hexadecimal value");
    }
    Ok(sandbox_path
        .join("runtime/setup-cache")
        .join(format!("{digest}-{index}.done")))
}

pub(crate) fn is_complete(sandbox_path: &Path, digest: &str, index: u64) -> Result<bool> {
    Ok(marker_path(sandbox_path, digest, index)?.is_file())
}

pub(crate) fn mark_complete(sandbox_path: &Path, digest: &str, index: u64) -> Result<()> {
    let marker = marker_path(sandbox_path, digest, index)?;
    let parent = marker
        .parent()
        .ok_or_else(|| anyhow::anyhow!("setup marker has no parent"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create setup cache {}", parent.display()))?;
    crate::fsutil::write_file_atomic(&marker, b"completed\n", 0o600)
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/setup_cache.rs"]
mod tests;
