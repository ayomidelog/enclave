use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::cli::{RootfsCommands, RootfsExportArgs, RootfsFetchArgs, RootfsImportArgs};

pub(crate) fn run_rootfs_command(command: RootfsCommands) -> Result<()> {
    match command {
        RootfsCommands::Export(args) => run_rootfs_export(args),
        RootfsCommands::Import(args) => run_rootfs_import(args),
        RootfsCommands::Fetch(args) => run_rootfs_fetch(args),
    }
}

fn run_rootfs_export(args: RootfsExportArgs) -> Result<()> {
    let target = resolve_cache_target(&args.state_dir, args.suite.as_deref(), args.base)?;
    let source = target.cache_path.clone();
    if !source.is_dir() || !crate::sandbox::has_rootfs_content(&source) {
        bail!(
            "rootfs cache '{}' does not exist or is incomplete at {}",
            target.label,
            source.display()
        );
    }

    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    export_rootfs_archive(&source, &target.archive_entry, &args.output)?;
    println!(
        "exported rootfs cache '{}' from {} to {}",
        target.label,
        source.display(),
        args.output.display()
    );
    Ok(())
}

fn run_rootfs_import(args: RootfsImportArgs) -> Result<()> {
    let target = resolve_cache_target(&args.state_dir, args.suite.as_deref(), args.base)?;
    import_rootfs_archive(&args.archive, &target.cache_path, args.replace)?;
    println!(
        "imported rootfs cache '{}' into {}",
        target.label,
        target.cache_path.display()
    );
    Ok(())
}

fn run_rootfs_fetch(args: RootfsFetchArgs) -> Result<()> {
    let temp_dir = temporary_workspace("rootfs-fetch");
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create {}", temp_dir.display()))?;
    let temp_archive = temp_dir.join(file_name_from_url(&args.url));
    let fetch_result = download_archive(&args.url, &temp_archive);
    let outcome = match fetch_result {
        Ok(()) => run_rootfs_import(RootfsImportArgs {
            state_dir: args.state_dir,
            suite: args.suite,
            base: args.base,
            replace: args.replace,
            archive: temp_archive.clone(),
        }),
        Err(err) => Err(err),
    };

    let _ = fs::remove_dir_all(&temp_dir);
    outcome
}

#[derive(Debug, Clone)]
struct CacheTarget {
    label: String,
    archive_entry: String,
    cache_path: PathBuf,
}

fn resolve_cache_target(state_dir: &Path, suite: Option<&str>, base: bool) -> Result<CacheTarget> {
    let has_suite = suite.is_some();
    if has_suite == base {
        bail!("choose exactly one of --suite <name> or --base");
    }

    let cache_root = crate::sandbox::ensure_rootfs_cache(state_dir)?;
    let (label, archive_entry) = if base {
        ("base".to_string(), "base".to_string())
    } else {
        let suite = suite.expect("suite presence checked above");
        validate_cache_key(suite)?;
        (suite.to_string(), suite.to_string())
    };
    Ok(CacheTarget {
        label,
        archive_entry: archive_entry.clone(),
        cache_path: cache_root.join(archive_entry),
    })
}

fn validate_cache_key(value: &str) -> Result<()> {
    if value.is_empty() || value.contains('/') || value == "." || value == ".." {
        bail!("invalid cache key '{}'", value);
    }
    if value.chars().any(|c| c.is_control()) {
        bail!("invalid cache key '{}'", value);
    }
    Ok(())
}

fn export_rootfs_archive(source: &Path, archive_entry: &str, output: &Path) -> Result<()> {
    let parent = source
        .parent()
        .ok_or_else(|| anyhow::anyhow!("rootfs cache path {} has no parent", source.display()))?;
    let mut cmd = Command::new("tar");
    cmd.arg("-C").arg(parent);
    if output_uses_gzip(output) {
        cmd.arg("-czf");
    } else {
        cmd.arg("-cf");
    }
    cmd.arg(output).arg(archive_entry);
    let output = cmd
        .output()
        .with_context(|| format!("failed to run tar for {}", source.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to export rootfs archive from {} ({}): {}",
            source.display(),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn import_rootfs_archive(archive: &Path, destination: &Path, replace: bool) -> Result<()> {
    if !archive.is_file() {
        bail!(
            "archive {} does not exist or is not a regular file",
            archive.display()
        );
    }
    if destination.exists() {
        if !replace {
            bail!(
                "rootfs cache destination {} already exists; pass --replace to overwrite it",
                destination.display()
            );
        }
        fs::remove_dir_all(destination)
            .with_context(|| format!("failed to remove {}", destination.display()))?;
    }

    let parent = destination.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "rootfs cache destination {} has no parent directory",
            destination.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;

    let temp_dir = temporary_workspace("rootfs-import");
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create {}", temp_dir.display()))?;

    let result = (|| {
        extract_archive(archive, &temp_dir)?;
        let extracted_root = locate_extracted_rootfs(&temp_dir)?;
        copy_extracted_rootfs(&extracted_root, destination)?;
        Ok(())
    })();

    let _ = fs::remove_dir_all(&temp_dir);
    result
}

fn extract_archive(archive: &Path, target_dir: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(target_dir)
        .output()
        .with_context(|| format!("failed to run tar -xf {}", archive.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to extract archive {} ({}): {}",
            archive.display(),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn locate_extracted_rootfs(extract_dir: &Path) -> Result<PathBuf> {
    if crate::sandbox::has_rootfs_content(extract_dir) {
        return Ok(extract_dir.to_path_buf());
    }

    let mut candidates = Vec::new();
    for entry in fs::read_dir(extract_dir)
        .with_context(|| format!("failed to read {}", extract_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() && crate::sandbox::has_rootfs_content(&entry.path()) {
            candidates.push(entry.path());
        }
    }

    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => bail!(
            "archive did not contain a recognizable rootfs layout under {}",
            extract_dir.display()
        ),
        _ => bail!(
            "archive contained multiple rootfs-like directories under {}; keep a single rootfs payload per archive",
            extract_dir.display()
        ),
    }
}

fn copy_extracted_rootfs(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    let src_arg = format!("{}/.", source.display());
    let output = Command::new("cp")
        .args(["-a", &src_arg, destination.to_string_lossy().as_ref()])
        .output()
        .with_context(|| {
            format!(
                "failed to run cp -a {} {}",
                source.display(),
                destination.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to copy extracted rootfs {} -> {} ({}): {}",
            source.display(),
            destination.display(),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn download_archive(url: &str, destination: &Path) -> Result<()> {
    let output = Command::new("curl")
        .arg("-fL")
        .arg(url)
        .arg("-o")
        .arg(destination)
        .output()
        .with_context(|| format!("failed to run curl for {}", url))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to download {} ({}): {}",
            url,
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn output_uses_gzip(path: &Path) -> bool {
    matches!(path.extension().and_then(OsStr::to_str), Some("gz" | "tgz"))
}

fn file_name_from_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let name = trimmed
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("rootfs.tar.gz");
    name.to_string()
}

fn temporary_workspace(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("enclave-{label}-{}-{}", std::process::id(), nanos))
}

#[cfg(test)]
#[path = "../../tests/src/commands/rootfs.rs"]
mod tests;
