#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
binary=${ENCLAVE_BINARY:-$repo_root/target/release/enclave}

require_binary() {
  if [[ ! -x "$binary" ]]; then
    printf 'binary not found: %s\n' "$binary" >&2
    printf 'build it with: cargo build --release\n' >&2
    exit 2
  fi
}

host_metadata() {
  printf 'date=%s\n' "$(date -Iseconds)"
  printf 'kernel=%s\n' "$(uname -srmo)"
  printf 'cpu=%s\n' "$(lscpu 2>/dev/null | awk -F: '/Model name/ {gsub(/^ +/, "", $2); print $2; exit}' || printf unknown)"
  printf 'rust=%s\n' "$(rustc --version 2>/dev/null || printf unknown)"
  printf 'filesystem=%s\n' "$(stat -f -c '%T' "$repo_root")"
  printf 'uid=%s\n' "$(id -u)"
}

percentile() {
  local percentile=$1
  awk -v p="$percentile" 'BEGIN { count=0 } { values[++count]=$1 } END { if (count == 0) exit 1; idx=int((p/100)*(count-1))+1; print values[idx] }'
}

run_timed_iterations() {
  local iterations=$1
  shift
  local samples
  samples=$(mktemp)
  trap 'rm -f "$samples"' RETURN
  for _ in $(seq 1 "$iterations"); do
    /usr/bin/time -f '%e' "$@" >/dev/null 2>>"$samples"
  done
  sort -n "$samples" >"$samples.sorted"
  printf 'min_seconds=%s\n' "$(head -n1 "$samples.sorted")"
  printf 'p50_seconds=%s\n' "$(percentile 50 <"$samples.sorted")"
  printf 'p95_seconds=%s\n' "$(percentile 95 <"$samples.sorted")"
  printf 'max_seconds=%s\n' "$(tail -n1 "$samples.sorted")"
  rm -f "$samples" "$samples.sorted"
}
