use std::fs::{self};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

pub fn canonicalize_within(base_dir: &Path, candidate: &Path, label: &str) -> Result<PathBuf> {
    ensure_path_within(base_dir, candidate, label)
}

pub fn ensure_path_within(base_dir: &Path, candidate: &Path, label: &str) -> Result<PathBuf> {
    let base = fs::canonicalize(base_dir)
        .with_context(|| format!("failed to canonicalize base {}", base_dir.display()))?;

    let absolute_candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    let target = if absolute_candidate.exists() {
        fs::canonicalize(&absolute_candidate)
            .with_context(|| format!("failed to canonicalize {} {}", label, candidate.display()))?
    } else {
        let parent = absolute_candidate.parent().ok_or_else(|| {
            anyhow!(
                "{} {} has no parent directory",
                label,
                absolute_candidate.display()
            )
        })?;
        let canonical_parent = fs::canonicalize(parent).with_context(|| {
            format!(
                "failed to canonicalize parent {} for {}",
                parent.display(),
                absolute_candidate.display()
            )
        })?;
        let file_name = absolute_candidate.file_name().ok_or_else(|| {
            anyhow!(
                "{} {} has no final path component",
                label,
                absolute_candidate.display()
            )
        })?;
        canonical_parent.join(file_name)
    };

    if !target.starts_with(&base) {
        bail!(
            "{} {} escapes base directory {}",
            label,
            target.display(),
            base.display()
        );
    }
    Ok(target)
}

pub fn slugify(input: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;

    for c in input.chars() {
        let normalized = c.to_ascii_lowercase();
        if normalized.is_ascii_alphanumeric() {
            out.push(normalized);
            previous_dash = false;
        } else if !previous_dash {
            out.push('-');
            previous_dash = true;
        }
    }

    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    trimmed
}
