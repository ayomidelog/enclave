use anyhow::{bail, Result};

pub const DEFAULT_CPU_CGROUP_PERIOD_US: u64 = 100_000;

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
