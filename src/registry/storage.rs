//! Registry persistence: schema version checks, the fingerprint-based cache,
//! and the atomic JSON read and write path.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{migrate, registry_path, MigrationStep, Registry, REGISTRY_VERSION};

pub(crate) fn validate_registry_version(version: u32) -> Result<()> {
    if version > REGISTRY_VERSION {
        bail!(
            "registry schema version {} is newer than this binary supports ({}); upgrade Enclave before using this state",
            version,
            REGISTRY_VERSION
        );
    }
    Ok(())
}

/// The schema version the registry file declares, when it can be read at all.
///
/// Two failures look alike to a caller that only sees an error, and they call for
/// opposite answers. A file that cannot be parsed is corrupt, and rebuilding it
/// from the sandboxes tree is the documented recovery. A file that parses and
/// declares a version this binary does not understand was written by a newer
/// Enclave, and rewriting it would drop whatever that version recorded. This
/// answers which one the file is: `None` means there is no version to refuse.
pub(crate) fn declared_registry_version(state_dir: &Path) -> Option<u32> {
    let raw = fs::read_to_string(registry_path(state_dir)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegistryFingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanos: i64,
}

#[derive(Debug)]
pub(super) struct RegistryCache {
    path: PathBuf,
    fingerprint: RegistryFingerprint,
    registry: Registry,
}

static REGISTRY_CACHE: OnceLock<Mutex<Option<RegistryCache>>> = OnceLock::new();

pub(crate) fn cache() -> &'static Mutex<Option<RegistryCache>> {
    REGISTRY_CACHE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn with_cached_registry<T, F>(path: &Path, operation: &F) -> Result<Option<T>>
where
    F: Fn(&Registry) -> Result<T>,
{
    let Some(fingerprint) = registry_fingerprint(path)? else {
        return Ok(None);
    };
    let guard = cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("registry cache lock poisoned"))?;
    let Some(cached) = guard.as_ref() else {
        return Ok(None);
    };
    if cached.path != path || cached.fingerprint != fingerprint {
        return Ok(None);
    }
    Ok(Some(operation(&cached.registry)?))
}

pub(crate) fn update_cache(state_dir: &Path, registry: Registry) {
    let path = registry_path(state_dir);
    let Ok(Some(fingerprint)) = registry_fingerprint(&path) else {
        return;
    };
    if let Ok(mut guard) = cache().lock() {
        *guard = Some(RegistryCache {
            path,
            fingerprint,
            registry,
        });
    }
}

pub(crate) fn registry_fingerprint(path: &Path) -> Result<Option<RegistryFingerprint>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to stat registry {}", path.display()))
        }
    };
    let modified = metadata
        .modified()
        .context("failed to read registry modification time")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("registry modification time predates unix epoch")?;
    Ok(Some(RegistryFingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        modified_seconds: modified.as_secs() as i64,
        modified_nanos: modified.subsec_nanos() as i64,
    }))
}

pub(crate) fn persist_metadata<T: Serialize>(path: &Path, metadata: &T) -> Result<()> {
    let payload = serde_json::to_string_pretty(metadata)?;
    crate::fsutil::write_file_atomic(path, payload.as_bytes(), 0o600)
        .with_context(|| format!("failed to persist normalized metadata {}", path.display()))
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read json {}", path.display()))?;
    let parsed = serde_json::from_str(&raw)
        .with_context(|| format!("invalid json metadata {}", path.display()))?;
    Ok(parsed)
}

pub(crate) fn load_registry_unlocked(state_dir: &Path) -> Result<Registry> {
    Ok(load_registry_with_migrations(state_dir)?.0)
}

/// Load the registry and bring it up to the schema this binary writes.
///
/// Returns the record and the schema steps that were applied to it. Migrating on
/// the read path is what keeps a mutation from writing an older version back: the
/// record is brought forward before any caller can see it. The steps are returned
/// as well as logged because a repair report is where an operator looks to find
/// out what changed underneath them.
pub(crate) fn load_registry_with_migrations(
    state_dir: &Path,
) -> Result<(Registry, Vec<MigrationStep>)> {
    let path = registry_path(state_dir);
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read registry {}", path.display()))?;
    let mut registry: Registry = serde_json::from_str(&raw)
        .with_context(|| format!("invalid registry {}", path.display()))?;
    validate_registry_version(registry.version)?;
    let steps = migrate(&mut registry)
        .with_context(|| format!("failed to migrate registry {}", path.display()))?;
    // Logged here rather than by each caller so every read path reports the same
    // thing, including the ones that discard the returned steps.
    for step in &steps {
        tracing::info!(
            "migrated registry schema from version {} to {}",
            step.from,
            step.to
        );
    }
    Ok((registry, steps))
}

pub(crate) fn save_registry_unlocked(state_dir: &Path, registry: &Registry) -> Result<()> {
    // A record is only ever written at the version this binary defines. Writing
    // anything else would leave a state file that this binary then refuses to
    // read, so the invariant is checked where it would be broken.
    if registry.version != REGISTRY_VERSION {
        bail!(
            "refusing to write registry schema version {}; this binary writes {}",
            registry.version,
            REGISTRY_VERSION
        );
    }
    let path = registry_path(state_dir);
    let payload = serde_json::to_vec(registry)?;
    crate::fsutil::write_file_atomic(&path, &payload, 0o600)
        .with_context(|| format!("failed to write registry {}", path.display()))?;
    Ok(())
}

pub(crate) fn registry_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join("registry.lock")
}
