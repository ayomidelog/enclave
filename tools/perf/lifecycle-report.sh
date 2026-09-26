#!/usr/bin/env bash
# Publish the lifecycle latency contract a release is measured against.
#
# The plan asks a release to carry comparable p50/p95/p99 numbers together with the
# host they were taken on, because a lifecycle number without its host is not
# comparable to anything: the same tree measures differently on a different kernel,
# CPU, or filesystem. live-lifecycle.sh already reports the distribution; this runs
# it and writes the result down, filling lifecycle-report.tpl.
#
# It has to run on a privileged host, so it is an operator command rather than a
# step the release workflow can take: a hosted runner has neither root nor the
# namespaces a sandbox needs. The workflow attaches the committed report, and the
# report says which host produced it.
#
# Usage: lifecycle-report.sh [OUTPUT.md]
#   ENCLAVE_LIVE_ITERATIONS  cycles to measure (default 12; the first is cold)
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
output=${1:-$repo_dir/docs/lifecycle-report.md}
iterations=${ENCLAVE_LIVE_ITERATIONS:-12}
rootfs_source=${ENCLAVE_LIVE_ROOTFS:-/root/.local/state/enclave/sandboxes/rootfs-cache/bookworm}

# Below four samples there is no tail to report, and a p95 of three samples is the
# largest of them wearing a percentile's name.
if ((iterations < 4)); then
  printf 'error: ENCLAVE_LIVE_ITERATIONS must be at least 4 to report a tail, got %s\n' "$iterations" >&2
  exit 2
fi

raw=$(mktemp /tmp/enclave-lifecycle-report.XXXXXX)
trap 'rm -f "$raw"' EXIT

ENCLAVE_LIVE_ITERATIONS="$iterations" "$script_dir/live-lifecycle.sh" >"$raw" 2>&1 || {
  printf 'error: the lifecycle benchmark failed; its output follows\n' >&2
  cat "$raw" >&2
  exit 1
}

field() {
  # Two shapes exist in the benchmark's output, and the number of `=` on the line is
  # what tells them apart. A single-pair line takes the whole rest, which is what
  # keeps a value with spaces in it (`kernel=Linux 6.8.0 ... x86_64 GNU/Linux`) and a
  # list of samples (`RAW_BOOT_SECONDS=1.5 1.6 ...`) intact. A line carrying several
  # pairs is split, which is how `workers` is found in `binary=... workers=1 ...`.
  awk -v key="$1" '
    {
      pairs = gsub(/=/, "=")
      if (pairs == 1) {
        if (index($0, key "=") == 1) { print substr($0, length(key) + 2); exit }
        next
      }
      for (i = 1; i <= NF; i++) {
        if (index($i, key "=") == 1) { print substr($i, length(key) + 2); exit }
      }
    }' "$raw"
}

require() {
  local value
  value=$(field "$1")
  if [[ -z "$value" ]]; then
    printf 'error: the benchmark did not report %s\n' "$1" >&2
    exit 1
  fi
  printf '%s' "$value"
}

if [[ "$(field LIVE_LIFECYCLE_TEST)" != passed ]]; then
  printf 'error: the lifecycle benchmark did not pass, so its numbers describe a broken run\n' >&2
  cat "$raw" >&2
  exit 1
fi

generated=$(require date)
kernel=$(require kernel)
cpu=$(require cpu)
load=$(require load)
rust=$(require rust)
filesystem=$(require filesystem)
binary=$(require binary)
workers=$(require workers)
fixture=$(require fixture)

# A binary older than the tree warns rather than fails, so the report carries the
# warning too: a number whose provenance is doubtful is worse than no number, and
# the reader is the one who has to decide.
provenance='the binary matched the tree it was measured from'
if grep -q 'do not describe the current tree' "$raw"; then
  provenance='WARNING: the binary was older than the tree, so these numbers do not describe it'
fi

boot_cold=$(require WORKSPACE_BOOT_COLD_SECONDS)
shutdown_cold=$(require SHUTDOWN_COLD_SECONDS)
boot_p50=$(require WORKSPACE_BOOT_WARM_SECONDS)
boot_p95=$(require WORKSPACE_BOOT_WARM_P95_SECONDS)
boot_p99=$(require WORKSPACE_BOOT_WARM_P99_SECONDS)
boot_max=$(require WORKSPACE_BOOT_WARM_MAX_SECONDS)
stop_p50=$(require SHUTDOWN_WARM_SECONDS)
stop_p95=$(require SHUTDOWN_WARM_P95_SECONDS)
stop_p99=$(require SHUTDOWN_WARM_P99_SECONDS)
stop_max=$(require SHUTDOWN_WARM_MAX_SECONDS)
boot_raw=$(require RAW_BOOT_SECONDS)
stop_raw=$(require RAW_SHUTDOWN_SECONDS)

# The template holds %PLACEHOLDER% names rather than shell expansions, so a backtick
# or a $ in its prose is just prose. Substituting here rather than expanding in the
# template is also what lets the report be read as a document on its own.
python3 - "$script_dir/lifecycle-report.tpl" "$output" "$generated" "$kernel" "$cpu" \
  "$load" "$filesystem" "$rust" "$binary" "$workers" "$fixture" "$iterations" "$provenance" \
  "$boot_p50" "$boot_p95" "$boot_p99" "$boot_max" "$stop_p50" "$stop_p95" \
  "$stop_p99" "$stop_max" "$boot_cold" "$shutdown_cold" "$boot_raw" "$stop_raw" \
  "$rootfs_source" <<'PY'
import sys

template, output = sys.argv[1], sys.argv[2]
(generated, kernel, cpu, load, filesystem, rust, binary, workers, fixture, iterations,
 provenance, boot_p50, boot_p95, boot_p99, boot_max, stop_p50, stop_p95, stop_p99,
 stop_max, boot_cold, shutdown_cold, boot_raw, stop_raw, rootfs) = sys.argv[3:]

values = {
    "GENERATED": generated,
    "KERNEL": kernel,
    "CPU": cpu,
    "LOAD": load,
    "FILESYSTEM": filesystem,
    "RUST": rust,
    "BINARY": binary,
    "WORKERS": workers,
    "FIXTURE": fixture,
    "ITERATIONS": iterations,
    "WARM_CYCLES": str(int(iterations) - 1),
    "PROVENANCE": provenance,
    "BOOT_P50": boot_p50,
    "BOOT_P95": boot_p95,
    "BOOT_P99": boot_p99,
    "BOOT_MAX": boot_max,
    "STOP_P50": stop_p50,
    "STOP_P95": stop_p95,
    "STOP_P99": stop_p99,
    "STOP_MAX": stop_max,
    "BOOT_COLD": boot_cold,
    "SHUTDOWN_COLD": shutdown_cold,
    "BOOT_RAW": boot_raw,
    "STOP_RAW": stop_raw,
    "ROOTFS": rootfs,
}

rendered = open(template).read()
for key, value in values.items():
    rendered = rendered.replace("%" + key + "%", value)
open(output, "w").write(rendered)
print("wrote " + output)
PY

printf 'warm boot p50=%s p95=%s p99=%s\n' "$boot_p50" "$boot_p95" "$boot_p99"
printf 'warm shutdown p50=%s p95=%s p99=%s\n' "$stop_p50" "$stop_p95" "$stop_p99"
