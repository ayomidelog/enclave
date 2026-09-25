#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
# For host_metadata: a lifecycle number is only comparable against another one
# taken on a host of the same kernel, CPU, and filesystem, so the run publishes
# what it was measured on.
source "$script_dir/common.sh"
# Prefer the release profile, for the same reason common.sh uses it: the debug
# binary is more than ten times the size, and the cost of starting it three times
# per workspace start lands entirely in the session phase of the measurement. A
# debug build is still accepted so a developer without one can run this, but the
# numbers are labelled so they are not compared with a release build.
binary=${ENCLAVE_BINARY:-}
if [[ -z "$binary" ]]; then
  binary=$repo_dir/target/release/enclave
  if [[ ! -x "$binary" && -x "$repo_dir/target/debug/enclave" ]]; then
    binary=$repo_dir/target/debug/enclave
    printf 'warning: no release binary; using the debug build, whose session phase includes the cost of a much larger executable\n' >&2
  fi
fi
# A binary older than the tree it is meant to describe produces numbers that do
# not describe the current code, which is worse than no number at all.
stale_source=$(find "$repo_dir/src" "$repo_dir/Cargo.toml" -newer "$binary" -print -quit 2>/dev/null || true)
if [[ -n "$stale_source" ]]; then
  printf 'warning: %s is older than %s; these numbers do not describe the current tree\n' \
    "$binary" "$stale_source" >&2
fi
workers=${ENCLAVE_UP_WORKERS:-1}
cleanup_workers=${ENCLAVE_CLEANUP_WORKERS:-4}
rootfs_source=${ENCLAVE_LIVE_ROOTFS:-/root/.local/state/enclave/sandboxes/rootfs-cache/bookworm}
# The eight-workspace fixture is the default, and it is the one whose numbers are
# published. A caller who wants a different number of workspaces points this at
# another file rather than editing the published fixture, so the numbers that
# were measured stay attached to the fixture that produced them.
fixture=${ENCLAVE_LIVE_ENCLAVEFILE:-$script_dir/live-heavy.Enclavefile}

if [[ $(id -u) -eq 0 ]]; then runner=(); else runner=(sudo -n); fi
if ! "${runner[@]}" test -x "$binary"; then
  printf 'error: build the binary first: %s (cargo build --release)\n' "$binary" >&2
  exit 1
fi
if ! "${runner[@]}" test -d "$rootfs_source"; then
  printf 'error: cached rootfs not found: %s\n' "$rootfs_source" >&2
  exit 1
fi

work_dir=$(mktemp -d /tmp/enclave-live.XXXXXX)
socket_dir=$(mktemp -d /tmp/enclave-live-socket.XXXXXX)
state_dir="$work_dir/state"
socket_path="$socket_dir/manager.sock"
pid_file="$socket_dir/manager.pid"

cleanup() {
  "${runner[@]}" "$binary" --socket "$socket_path" down >/dev/null 2>&1 || true
  "${runner[@]}" "$binary" --socket "$socket_path" destroy heavy-live >/dev/null 2>&1 || true
  "${runner[@]}" "$binary" --socket "$socket_path" daemon stop >/dev/null 2>&1 || true
  if [[ -f "$pid_file" ]]; then "${runner[@]}" kill "$(cat "$pid_file")" >/dev/null 2>&1 || true; fi
  for proc in /proc/[0-9]*; do
    pid=${proc##*/}
    proc_exe=$(readlink "$proc/exe" 2>/dev/null || true)
    [[ "$proc_exe" == "$binary" ]] || continue
    proc_cmd=$(tr '\0' ' ' < "$proc/cmdline" 2>/dev/null || true)
    case "$proc_cmd" in
      *"$work_dir"*) "${runner[@]}" kill -KILL "$pid" >/dev/null 2>&1 || true ;;
    esac
  done
  "${runner[@]}" umount -l "$state_dir"/sandboxes/*/runtime/rootfs.mnt >/dev/null 2>&1 || true
  "${runner[@]}" rm -rf "$work_dir" "$socket_dir"
}
trap cleanup EXIT INT TERM

"${runner[@]}" mkdir -p "$state_dir/sandboxes/rootfs-cache"
"${runner[@]}" cp -a "$rootfs_source" "$state_dir/sandboxes/rootfs-cache/"
cp "$fixture" "$work_dir/Enclavefile"
"${runner[@]}" env \
  ENCLAVE_UP_WORKERS="$workers" ENCLAVE_CLEANUP_WORKERS="$cleanup_workers" \
  "$binary" --socket "$socket_path" daemon start \
  --state-dir "$state_dir" --pid-file "$pid_file" --wait-secs 20
"${runner[@]}" "$binary" --socket "$socket_path" create heavy-live \
  --suite bookworm --bootstrap-method cached_rootfs >/dev/null

cd "$work_dir"
printf 'binary=%s workers=%s cleanup_workers=%s fixture=%s rootfs_source=%s\n' \
  "$binary" "$workers" "$cleanup_workers" "$fixture" "$rootfs_source"
# The host the numbers came from, so a saved result can be compared with
# another one rather than only read.
host_metadata
ENCLAVE_UP_WORKERS="$workers" ENCLAVE_CLEANUP_WORKERS="$cleanup_workers" \
  /usr/bin/time -f 'WORKSPACE_BOOT_COLD_SECONDS=%e' \
  "${runner[@]}" "$binary" --socket "$socket_path" up --cache-setup
"${runner[@]}" "$binary" --socket "$socket_path" stats
ENCLAVE_CLEANUP_WORKERS="$cleanup_workers" \
  /usr/bin/time -f 'SHUTDOWN_COLD_SECONDS=%e' \
  "${runner[@]}" "$binary" --socket "$socket_path" down
ENCLAVE_UP_WORKERS="$workers" ENCLAVE_CLEANUP_WORKERS="$cleanup_workers" \
  /usr/bin/time -f 'WORKSPACE_BOOT_WARM_SECONDS=%e' \
  "${runner[@]}" "$binary" --socket "$socket_path" up --cache-setup
ENCLAVE_CLEANUP_WORKERS="$cleanup_workers" \
  /usr/bin/time -f 'SHUTDOWN_WARM_SECONDS=%e' \
  "${runner[@]}" "$binary" --socket "$socket_path" down
printf 'LIVE_LIFECYCLE_TEST=passed\n'
