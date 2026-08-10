#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "$0")/common.sh"
require_binary

usage() {
  cat <<'USAGE'
Usage: bench.sh <command> [options]

Commands:
  host       Print benchmark host metadata.
  ping       Measure daemon ping CLI latency.
  health     Measure daemon health CLI latency.
  registry   Measure registry read throughput with the unit benchmark.
  archive    Measure single-file archive creation overhead.
  stress     Exercise concurrent daemon control requests.
  cp         Run the namespace-dependent copy benchmark (requires root and selectors).
USAGE
}

command_name=${1:-}
shift || true
iterations=30
fixture_dir=
sandbox_selector=
workspace_selector=
source_path=
destination_path=
case "$command_name" in
  host)
    host_metadata
    exit 0
    ;;
  ping|health|registry|archive|stress|cp) ;;
  *) usage; exit 2 ;;
esac

while (($#)); do
  case "$1" in
    --iterations) iterations=$2; shift 2 ;;
    --fixture-dir) fixture_dir=$2; shift 2 ;;
    --sandbox) sandbox_selector=$2; shift 2 ;;
    --workspace) workspace_selector=$2; shift 2 ;;
    --src) source_path=$2; shift 2 ;;
    --dst) destination_path=$2; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done

host_metadata
printf 'command=%s\niterations=%s\n' "$command_name" "$iterations"

case "$command_name" in
  ping|health)
    if [[ $(id -u) -ne 0 ]] && ! sudo -n true 2>/dev/null; then
      printf 'status=SKIP reason=daemon benchmark requires root\n'
      exit 0
    fi
    tmp_dir=$(sudo -n mktemp -d)
    socket="$tmp_dir/enclave.sock"
    state_dir="$tmp_dir/state"
    pid_file="$tmp_dir/enclave.pid"
    log_file=$(mktemp)
    if [[ $(id -u) -eq 0 ]]; then
      runner=()
    else
      runner=(sudo -n)
    fi
    "${runner[@]}" "$binary" --socket "$socket" daemon run \
      --state-dir "$state_dir" --pid-file "$pid_file" >"$log_file" 2>&1 &
    daemon_pid=$!
    cleanup_ping() {
      "${runner[@]}" "$binary" --socket "$socket" daemon stop >/dev/null 2>&1 || true
      "${runner[@]}" kill "$daemon_pid" >/dev/null 2>&1 || true
      wait "$daemon_pid" 2>/dev/null || true
      "${runner[@]}" rm -rf "$tmp_dir" >/dev/null 2>&1 || true
      rm -f "$log_file"
    }
    trap cleanup_ping EXIT
    for _ in $(seq 1 200); do
      if "${runner[@]}" test -S "$socket" && "${runner[@]}" "$binary" --socket "$socket" ping >/dev/null 2>&1; then
        break
      fi
      sleep .01
    done
    if ! "${runner[@]}" test -S "$socket"; then
      cat "$log_file" >&2
      printf 'status=FAIL reason=daemon did not become ready\n'
      exit 1
    fi
    printf 'status=PASS\n'
    run_timed_iterations "$iterations" "${runner[@]}" "$binary" --socket "$socket" "$command_name"
    ;;
  registry)
    cargo test --test perf_suite registry_read_cache_benchmark -- --ignored --nocapture
    ;;
  archive)
    cargo test --test perf_suite single_file_archive_benchmark -- --ignored --nocapture
    ;;
  stress)
    if [[ $(id -u) -ne 0 ]] && ! sudo -n true 2>/dev/null; then
      printf 'status=SKIP reason=daemon benchmark requires root\n'
      exit 0
    fi
    tmp_dir=$(sudo -n mktemp -d)
    socket="$tmp_dir/enclave.sock"
    state_dir="$tmp_dir/state"
    pid_file="$tmp_dir/enclave.pid"
    log_file=$(mktemp)
    if [[ $(id -u) -eq 0 ]]; then runner=(); else runner=(sudo -n); fi
    "${runner[@]}" "$binary" --socket "$socket" daemon run \
      --state-dir "$state_dir" --pid-file "$pid_file" >"$log_file" 2>&1 &
    daemon_pid=$!
    cleanup_stress() {
      "${runner[@]}" "$binary" --socket "$socket" daemon stop >/dev/null 2>&1 || true
      wait "$daemon_pid" 2>/dev/null || true
      "${runner[@]}" kill "$daemon_pid" >/dev/null 2>&1 || true
      "${runner[@]}" rm -rf "$tmp_dir" >/dev/null 2>&1 || true
      rm -f "$log_file"
    }
    trap cleanup_stress EXIT
    for _ in $(seq 1 200); do
      if "${runner[@]}" test -S "$socket" && "${runner[@]}" "$binary" --socket "$socket" ping >/dev/null 2>&1; then break; fi
      sleep .01
    done
    start_ns=$(date +%s%N)
    seq "$iterations" | xargs -P 16 -n 1 sh -c "${runner[*]} '$binary' --socket '$socket' ping >/dev/null" _
    elapsed_ns=$(( $(date +%s%N) - start_ns ))
    printf 'status=PASS\nrequests=%s\ncontrol_workers=6\ntransfer_workers=2\nparallelism=16\nelapsed_seconds=%.6f\n' \
      "$iterations" "$((elapsed_ns / 1000))e-6"
    ;;
  cp)
    if [[ $(id -u) -ne 0 ]]; then
      printf 'status=SKIP reason=workspace copy benchmark requires root\n'
      exit 0
    fi
    if [[ -z "$sandbox_selector" || -z "$workspace_selector" || -z "$source_path" || -z "$destination_path" ]]; then
      printf 'status=SKIP reason=--sandbox --workspace --src and --dst are required\n'
      exit 0
    fi
    printf 'status=PASS\n'
    run_timed_iterations "$iterations" "$binary" workspace cp \
      "$sandbox_selector" "$workspace_selector" "$source_path" "$destination_path"
    ;;
esac
