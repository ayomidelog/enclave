#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo_root"

output=$(cargo test --test perf_suite single_file_archive_benchmark -- --ignored --nocapture 2>&1)
printf '%s\n' "$output"

speedup=$(printf '%s\n' "$output" | awk -F= '/speedup=/{gsub(/x/, "", $2); print $2; exit}')
if [[ -z "$speedup" ]]; then
  printf 'performance gate: missing archive speedup\n' >&2
  exit 1
fi

awk -v speedup="$speedup" 'BEGIN { if (speedup < 1.05) { printf "performance gate: archive speedup %.2fx is below 1.05x\n", speedup > "/dev/stderr"; exit 1 } }'
printf 'performance_gate=PASS\narchive_speedup=%sx\n' "$speedup"
