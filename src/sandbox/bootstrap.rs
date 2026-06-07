use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

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

pub fn bootstrap_rootfs(params: &BootstrapParams<'_>) -> Result<()> {
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
        BootstrapMethod::CachedRootfs => bootstrap_cached_rootfs(
            params.rootfs_dir,
            params.name,
            params.suite,
            params.state_dir,
        ),
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
) -> Result<()> {
    validate_debootstrap_binary(debootstrap_binary)?;

    let cache_dir = rootfs_cache_dir(state_dir);
    let suite_cache = cache_dir.join(suite);
    if suite_cache.is_dir() && has_rootfs_content(&suite_cache) {
        tracing::info!(
            "sandbox '{}': using cached rootfs for suite '{}' from {}",
            name,
            suite,
            suite_cache.display()
        );
        copy_dir_recursive(&suite_cache, rootfs_dir).with_context(|| {
            format!(
                "failed to copy cached rootfs from {} to {}",
                suite_cache.display(),
                rootfs_dir.display()
            )
        })?;
        return Ok(());
    }

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

    Ok(())
}

fn bootstrap_cached_rootfs(
    rootfs_dir: &Path,
    name: &str,
    suite: &str,
    state_dir: &Path,
) -> Result<()> {
    let cache_dir = rootfs_cache_dir(state_dir);
    let suite_cache = cache_dir.join(suite);
    if suite_cache.is_dir() && has_rootfs_content(&suite_cache) {
        tracing::info!(
            "sandbox '{}' bootstrap: copying cached rootfs for suite '{}' from {}",
            name,
            suite,
            suite_cache.display()
        );
        return copy_dir_recursive(&suite_cache, rootfs_dir).with_context(|| {
            format!(
                "failed to copy cached rootfs from {} to {}",
                suite_cache.display(),
                rootfs_dir.display()
            )
        });
    }

    let generic_cache = cache_dir.join("base");
    if generic_cache.is_dir() && has_rootfs_content(&generic_cache) {
        tracing::info!(
            "sandbox '{}' bootstrap: copying generic cached rootfs from {}",
            name,
            generic_cache.display()
        );
        return copy_dir_recursive(&generic_cache, rootfs_dir).with_context(|| {
            format!(
                "failed to copy cached rootfs from {} to {}",
                generic_cache.display(),
                rootfs_dir.display()
            )
        });
    }

    bail!(
        "cached rootfs not found. Populate either:\n  \
         - Suite cache: {}\n  \
         - Generic cache: {}\n\
         with a minimal rootfs (e.g., extract an alpine-minirootfs tarball) \
         before using the 'cached_rootfs' bootstrap method.",
        suite_cache.display(),
        generic_cache.display()
    )
}

fn cache_rootfs_suite(rootfs_dir: &Path, state_dir: &Path, suite: &str) -> Result<()> {
    let cache_dir = rootfs_cache_dir(state_dir);
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("failed to create rootfs cache at {}", cache_dir.display()))?;

    let suite_cache = cache_dir.join(suite);
    if suite_cache.exists() {
        return Ok(());
    }

    let tmp_cache = cache_dir.join(format!(".{}.{}.tmp", suite, std::process::id()));
    if tmp_cache.exists() {
        fs::remove_dir_all(&tmp_cache).with_context(|| {
            format!("failed to remove stale temp cache {}", tmp_cache.display())
        })?;
    }

    copy_dir_recursive(rootfs_dir, &tmp_cache)
        .with_context(|| format!("failed to copy rootfs to cache {}", tmp_cache.display()))?;

    fs::rename(&tmp_cache, &suite_cache).with_context(|| {
        format!(
            "failed to rename cache {} to {}",
            tmp_cache.display(),
            suite_cache.display()
        )
    })?;

    tracing::info!(
        "rootfs cached for suite '{}' at {}",
        suite,
        suite_cache.display()
    );
    Ok(())
}

pub(crate) fn has_rootfs_content(dir: &Path) -> bool {
    ["bin", "etc", "usr"]
        .iter()
        .all(|required| dir.join(required).is_dir())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    }

    validate_copy_source(src)?;

    let src_arg = format!("{}/.", src.display());
    let output = Command::new("cp")
        .args(["-a", &src_arg, dst.to_string_lossy().as_ref()])
        .output()
        .with_context(|| format!("failed to run cp -a {} {}", src.display(), dst.display()))?;
    if !output.status.success() {
        let detail = command_failure_detail(&output);
        bail!(
            "cp -a {} {} failed ({}): {}",
            src.display(),
            dst.display(),
            output.status,
            detail
        );
    }
    Ok(())
}

fn validate_copy_source(src: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();

        if file_type.is_dir() {
            validate_copy_source(&src_path)?;
        } else if file_type.is_symlink() {
            fs::read_link(&src_path)
                .with_context(|| format!("failed to read symlink {}", src_path.display()))?;
        } else if !(file_type.is_file()
            || file_type.is_fifo()
            || file_type.is_char_device()
            || file_type.is_block_device())
        {
            bail!(
                "unsupported filesystem entry type for {}",
                src_path.display()
            );
        }
    }

    Ok(())
}

pub(crate) fn rootfs_cache_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("sandboxes").join("rootfs-cache")
}

pub(crate) fn ensure_rootfs_cache(state_dir: &Path) -> Result<PathBuf> {
    let cache_dir = rootfs_cache_dir(state_dir);
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("failed to create rootfs cache at {}", cache_dir.display()))?;
    Ok(cache_dir)
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/bootstrap.rs"]
mod tests;
