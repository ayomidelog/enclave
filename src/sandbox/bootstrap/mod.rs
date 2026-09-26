use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::cache;
use crate::hostcmd::HostCommand;

use std::time::Duration;

// Copying a rootfs tree is metadata bound and can take minutes on a large
// image, so it gets a longer deadline than the default while staying bounded.
const ROOTFS_COPY_TIMEOUT: Duration = Duration::from_secs(900);
use super::features;
use super::types::BootstrapMethod;
use super::util::{command_failure_detail, run_command_with_live_log, validate_debootstrap_binary};

pub struct BootstrapParams<'a> {
    pub method: &'a BootstrapMethod,
    pub rootfs_dir: &'a Path,
    pub sandbox_dir: &'a Path,
    pub debootstrap_binary: &'a str,
    pub suite: &'a str,
    pub mirror: &'a str,
    pub name: &'a str,
    pub state_dir: &'a Path,
}

mod cached;
mod copy;
mod paths;

pub(crate) use copy::{copy_cached_rootfs, has_rootfs_content};
pub(crate) use paths::{ensure_rootfs_cache, rootfs_cache_dir};

pub(crate) use cached::{bootstrap_cached_rootfs, cache_rootfs_suite};

// The tests reach the copy helper directly, because it is the part of this module
// that has edge cases worth pinning: symlinks, fifos, and an empty source.
#[cfg(test)]
pub(crate) use copy::copy_dir_recursive;
/// How the sandbox rootfs was produced.
#[derive(Debug)]
pub struct BootstrapOutcome {
    /// Cache directory to use as an immutable OverlayFS lower layer.
    ///
    /// `None` means the rootfs directory already holds the complete tree, so
    /// the sandbox can be used without an overlay mount.
    pub shared_lower: Option<PathBuf>,
}

impl BootstrapOutcome {
    fn local() -> Self {
        Self { shared_lower: None }
    }

    fn shared(path: PathBuf) -> Self {
        Self {
            shared_lower: Some(path),
        }
    }
}

pub fn bootstrap_rootfs(params: &BootstrapParams<'_>) -> Result<BootstrapOutcome> {
    features::validate_platform()?;

    match params.method {
        BootstrapMethod::Debootstrap => bootstrap_debootstrap(
            params.rootfs_dir,
            params.sandbox_dir,
            params.debootstrap_binary,
            params.suite,
            params.mirror,
            params.name,
            params.state_dir,
        ),
        BootstrapMethod::CachedRootfs => {
            bootstrap_cached_rootfs(params.name, params.suite, params.state_dir)
        }
    }
}

fn bootstrap_debootstrap(
    rootfs_dir: &Path,
    sandbox_dir: &Path,
    debootstrap_binary: &str,
    suite: &str,
    mirror: &str,
    name: &str,
    state_dir: &Path,
) -> Result<BootstrapOutcome> {
    validate_debootstrap_binary(debootstrap_binary)?;

    let cache_dir = rootfs_cache_dir(state_dir);
    cache::ensure(&cache_dir)?;
    let suite_cache = cache_dir.join(suite);
    let verdict = cache::evaluate(&cache_dir, suite, &suite_cache);
    if verdict.is_hit() {
        tracing::info!(
            "sandbox '{}': using cached rootfs for suite '{}' from {} ({})",
            name,
            suite,
            suite_cache.display(),
            verdict.describe()
        );
        return Ok(BootstrapOutcome::shared(suite_cache));
    }
    tracing::info!(
        "sandbox '{}': the cached rootfs for suite '{}' at {} will not be reused: {}",
        name,
        suite,
        suite_cache.display(),
        verdict.describe()
    );

    let log_path = sandbox_dir.join("debootstrap.log");
    tracing::info!(
        "sandbox '{}' bootstrap started; live log: {}",
        name,
        log_path.display()
    );

    let mut bootstrap = Command::new(debootstrap_binary);
    bootstrap
        .arg("--variant=minbase")
        .arg(suite)
        .arg(rootfs_dir)
        .arg(mirror);
    let output = run_command_with_live_log(&mut bootstrap, &log_path, "debootstrap")
        .with_context(|| format!("failed to execute '{}'", debootstrap_binary))?;

    tracing::info!(
        "sandbox '{}' bootstrap finished with status {}; log: {}",
        name,
        output.status,
        log_path.display()
    );

    if !output.status.success() {
        let detail = command_failure_detail(&output);
        bail!("debootstrap failed ({}): {}", output.status, detail);
    }

    if let Err(err) = cache_rootfs_suite(rootfs_dir, state_dir, suite) {
        tracing::warn!("failed to cache rootfs for suite '{}': {err:#}", suite);
    }

    Ok(BootstrapOutcome::local())
}

#[cfg(test)]
#[path = "../../../tests/src/sandbox/bootstrap.rs"]
mod tests;
