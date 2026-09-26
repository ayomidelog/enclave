use anyhow::{bail, Result};

pub const DEFAULT_CPU_CGROUP_PERIOD_US: u64 = 100_000;

/// The smallest memory limit Enclave will accept.
///
/// A limit this low is not a limit anyone means: the workspace runtime cannot load
/// its own shared libraries inside it, so the session fails to start with a loader
/// error rather than a limit that is merely tight. Refusing it here turns a confusing
/// start failure into a number the operator has to correct.
pub const MIN_MEMORY_BYTES: u64 = 16 * 1024 * 1024;

/// Whether a memory limit is one Enclave will set.
///
/// `None` is unlimited and always allowed. A value below the floor is refused with
/// the floor named, because the alternative is a runtime that cannot start.
pub fn validate_memory_bytes(value: Option<u64>) -> Result<()> {
    let Some(bytes) = value else {
        return Ok(());
    };
    if bytes < MIN_MEMORY_BYTES {
        bail!(
            "memory limit must be at least {} MiB",
            MIN_MEMORY_BYTES / (1024 * 1024)
        );
    }
    Ok(())
}

pub fn validate_cpu_percent(value: f64) -> Result<()> {
    if !value.is_finite() {
        bail!("cpu_percent must be a finite number");
    }
    if value <= 0.0 {
        bail!("cpu_percent must be greater than 0");
    }
    if value > 100.0 {
        bail!("cpu_percent must be less than or equal to 100");
    }
    Ok(())
}

pub fn cpu_quota_from_machine_percent(percent: f64, period_us: u64) -> Result<u64> {
    validate_cpu_percent(percent)?;
    let cpu_count = std::thread::available_parallelism()
        .map(|count| count.get() as f64)
        .unwrap_or(1.0);
    let quota = ((percent / 100.0) * cpu_count * period_us as f64).round();
    Ok(quota.max(1.0) as u64)
}

pub fn format_cpu_percent(percent: f64) -> String {
    let rounded = (percent * 100.0).round() / 100.0;
    if (rounded.fract()).abs() < f64::EPSILON {
        format!("{rounded:.0}%")
    } else {
        format!("{rounded:.2}%")
    }
}
