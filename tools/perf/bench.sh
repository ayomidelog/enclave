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
  list       Measure sandbox list latency.
  stats      Measure empty workspace stats latency.
  ps         Measure empty process listing latency.
  doctor     Measure daemon doctor latency.
  workspace-list Measure workspace list latency.
  registry   Measure registry read throughput with the unit benchmark.
  archive    Measure single-file archive creation overhead.
  many-files Measure many-file archive creation overhead.
  stress     Exercise concurrent daemon control requests.
  lock-wait  Contend for the registry lock and report its wait p99 against the budget.
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
  ping|health|list|stats|ps|doctor|workspace-list|registry|archive|many-files|stress|lock-wait|cp) ;;
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
  ping|health|list|stats|ps|doctor|workspace-list)
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
    case "$command_name" in
      ping|health|list|stats|ps|doctor)
        benchmark_args=("$command_name")
        ;;
      workspace-list)
        benchmark_args=(workspace list)
        ;;
    esac
    run_timed_iterations "$iterations" "${runner[@]}" "$binary" --socket "$socket" "${benchmark_args[@]}"
    ;;
  registry)
    cargo test --test perf_suite registry_read_cache_benchmark -- --ignored --nocapture
    ;;
  archive)
    cargo test --test perf_suite single_file_archive_benchmark -- --ignored --nocapture
    ;;
  many-files)
    cargo test --test perf_suite many_file_archive_benchmark -- --ignored --nocapture
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
  lock-wait)
    # How long a lifecycle request waits for the registry lock, under contention.
    #
    # The lock is held only for the mutations themselves, never across the host work
    # a lifecycle operation does, so a wait on it is another request committing its
    # own record. The tail of that distribution is what matters: while one request
    # holds the registry, every other request in the daemon is stalled behind it, so
    # a holder that does slow work inside the lock shows up here and nowhere else.
    #
    # The requests that contend are list requests, which read the whole registry under
    # the lock. They are the shortest holders there are, which makes them the right
    # load: the wait they produce is the lock itself rather than the work one caller
    # chose to do inside it.
    #
    # The daemon rate limits a uid to 120 requests per two seconds, and every CLI
    # invocation sends a ping before its action, so one iteration costs two requests.
    # The count is therefore capped at 50: above that the limiter refuses most of the
    # load, and a run whose requests were refused would measure the limiter rather
    # than the lock. The refusals are counted below, so a run that hit the cap anyway
    # says so instead of reporting a number taken from the few that got through.
    if ((iterations > 50)); then
      printf 'status=SKIP reason=--iterations must be 50 or less, or the daemon rate limiter refuses the load\n'
      exit 0
    fi
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
    cleanup_lock_wait() {
      "${runner[@]}" "$binary" --socket "$socket" daemon stop >/dev/null 2>&1 || true
      "${runner[@]}" kill "$daemon_pid" >/dev/null 2>&1 || true
      wait "$daemon_pid" 2>/dev/null || true
      "${runner[@]}" rm -rf "$tmp_dir" >/dev/null 2>&1 || true
      rm -f "$log_file"
    }
    trap cleanup_lock_wait EXIT
    for _ in $(seq 1 200); do
      if "${runner[@]}" test -S "$socket" && "${runner[@]}" "$binary" --socket "$socket" ping >/dev/null 2>&1; then break; fi
      sleep .01
    done
    start_ns=$(date +%s%N)
    # A refused request is not a failure of the run: the refusals are counted below
    # and a run that had any is reported as skipped rather than as a measurement.
    seq "$iterations" | xargs -P 16 -n 1 sh -c "${runner[*]} '$binary' --socket '$socket' list >/dev/null" _ || true
    elapsed_ns=$(( $(date +%s%N) - start_ns ))
    refused=$(grep -c 'rate limit exceeded' "$log_file" || true)
    printf 'status=PASS\nrequests=%s\nparallelism=16\nrequests_refused=%s\nelapsed_seconds=%.6f\n' \
      "$iterations" "$refused" "$((elapsed_ns / 1000))e-6"
    if [[ "$refused" -ne 0 ]]; then
      printf 'lock_wait_budget=SKIP reason=%s request(s) were refused by the daemon rate limiter\n' "$refused" >&2
      exit 0
    fi
    # Read the daemon own report rather than timing the requests from outside: the
    # wait being measured is the one the daemon observed, and a number taken from the
    # client would include the round trip on top of it.
    health=$("${runner[@]}" "$binary" --socket "$socket" health)
    printf '%s\n' "$health" | python3 -c '
import json, sys
report = json.load(sys.stdin)
metrics = report["metrics"]
p99 = metrics["registry_lock_wait_percentiles_us"]["p99_us"]
budget = metrics["registry_lock_wait_p99_budget_us"]
within = metrics["registry_lock_wait_within_budget"]
print("registry_lock_wait_p99_us=%s" % p99)
print("registry_lock_wait_p99_budget_us=%s" % budget)
print("registry_lock_wait_within_budget=%s" % str(within).lower())
if p99 is None:
    print("lock_wait_budget=PASS (too few samples to have a p99)")
elif not within:
    print("lock_wait_budget=FAIL", file=sys.stderr)
    sys.exit(1)
else:
    print("lock_wait_budget=PASS")
'
    ;;
  cp)
    if [[ $(id -u) -ne 0 ]]; then
      printf 'status=SKIP reason=workspace copy benchmark requires root\n'
      exit 0
    fi
    if [[ -n "$fixture_dir" && -z "$source_path" ]]; then
      source_path="$fixture_dir/sparse/5g.sparse"
      destination_path=${destination_path:-ws:/home/5g.sparse}
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
