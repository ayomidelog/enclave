//! Bootstrap from a cached rootfs, and publishing one into the cache.

use super::*;

use super::copy::copy_dir_recursive;

pub(crate) fn bootstrap_cached_rootfs(
    name: &str,
    suite: &str,
    state_dir: &Path,
) -> Result<BootstrapOutcome> {
    let cache_dir = rootfs_cache_dir(state_dir);
    cache::ensure(&cache_dir)?;
    let suite_cache = cache_dir.join(suite);
    let suite_verdict = cache::evaluate(&cache_dir, suite, &suite_cache);
    if suite_verdict.is_hit() {
        tracing::info!(
            "sandbox '{}' bootstrap: using cached rootfs for suite '{}' from {} ({})",
            name,
            suite,
            suite_cache.display(),
            suite_verdict.describe()
        );
        return Ok(BootstrapOutcome::shared(suite_cache));
    }
    tracing::info!(
        "sandbox '{}' bootstrap: the suite cache for '{}' at {} will not be reused: {}",
        name,
        suite,
        suite_cache.display(),
        suite_verdict.describe()
    );

    let generic_cache = cache_dir.join("base");
    let generic_verdict = cache::evaluate(&cache_dir, "base", &generic_cache);
    if generic_verdict.is_hit() {
        tracing::info!(
            "sandbox '{}' bootstrap: using generic cached rootfs from {} ({})",
            name,
            generic_cache.display(),
            generic_verdict.describe()
        );
        return Ok(BootstrapOutcome::shared(generic_cache));
    }
    tracing::info!(
        "sandbox '{}' bootstrap: the generic cache at {} will not be reused: {}",
        name,
        generic_cache.display(),
        generic_verdict.describe()
    );

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

pub(crate) fn cache_rootfs_suite(rootfs_dir: &Path, state_dir: &Path, suite: &str) -> Result<()> {
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
    cache::register(&cache_dir, suite, &suite_cache)?;

    tracing::info!(
        "rootfs cached for suite '{}' at {}",
        suite,
        suite_cache.display()
    );
    Ok(())
}
