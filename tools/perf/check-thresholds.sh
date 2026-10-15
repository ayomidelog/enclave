#!/usr/bin/env bash
# The performance gate: the one benchmark whose result is a ratio the code has to beat.
#
# The benchmark is timed against the external tool it replaces, so the assertion is on
# the speedup rather than on an absolute duration, which would only describe the runner.
#
# Two benchmarks in `tests/perf_suite.rs` are deliberately not gated, and it is worth
# saying why rather than leaving them to look forgotten:
#
# `many_file_archive_benchmark` measures the same Rust implementation against the same
# `tar` binary, but over a thousand small files instead of one large one. The Rust path
# loses that comparison — measured on this host, 0.51x, because `tar` batches the
# metadata work that the `tar` crate does per entry — so there is no floor above 1.0 to
# assert. Gating it would fail on a correct build. It is still published, because a
# change that makes the ratio worse is worth seeing even when the ratio is below one.
#
# `registry_read_cache_benchmark` compares the cached read path against re-parsing the
# file on every access. The cache is a fingerprint check that is correct by construction
# rather than fast by a margin, so there is no ratio a regression would push below; the
# number is published for a reader who wants it.
#
# Both are runnable through `tools/perf/bench.sh`, which is where a number nobody
# asserts on belongs.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo_root"

minimum_speedup=1.05

# Run one benchmark and require its reported speedup to clear the floor.
check_speedup() {
  local benchmark=$1
  local output
  output=$(cargo test --test perf_suite "$benchmark" -- --ignored --nocapture 2>&1)
  printf '%s\n' "$output"

  local speedup
  speedup=$(printf '%s\n' "$output" | awk -F= '/speedup=/{gsub(/x/, "", $2); print $2; exit}')
  if [[ -z $speedup ]]; then
    printf 'performance gate: %s reported no speedup\n' "$benchmark" >&2
    return 1
  fi

  awk -v speedup="$speedup" -v floor="$minimum_speedup" -v benchmark="$benchmark" \
    'BEGIN { if (speedup < floor) { printf "performance gate: %s speedup %.2fx is below %sx\n", benchmark, speedup, floor > "/dev/stderr"; exit 1 } }'
  printf '%s_speedup=%sx\n' "$benchmark" "$speedup"
}

check_speedup single_file_archive_benchmark
printf 'performance_gate=PASS\n'
