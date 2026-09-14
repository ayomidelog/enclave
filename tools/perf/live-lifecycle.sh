#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
binary=${ENCLAVE_BINARY:-$repo_dir/target/debug/enclave}
workers=${ENCLAVE_UP_WORKERS:-1}
cleanup_workers=${ENCLAVE_CLEANUP_WORKERS:-4}
rootfs_source=${ENCLAVE_LIVE_ROOTFS:-/root/.local/state/enclave/sandboxes/rootfs-cache/bookworm}

if [[ $(id -u) -eq 0 ]]; then runner=(); else runner=(sudo -n); fi
if ! "${runner[@]}" test -x "$binary"; then
  printf 'error: build the binary first: %s\n' "$binary" >&2
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
  "${runner[@]}" umount -l "$state_dir"/sandboxes/*/runtime/rootfs.mnt >/dev/null 2>&1 || true
  "${runner[@]}" rm -rf "$work_dir" "$socket_dir"
}
trap cleanup EXIT INT TERM

"${runner[@]}" mkdir -p "$state_dir/sandboxes/rootfs-cache"
"${runner[@]}" cp -a "$rootfs_source" "$state_dir/sandboxes/rootfs-cache/"
cp "$script_dir/live-heavy.Enclavefile" "$work_dir/Enclavefile"
"${runner[@]}" "$binary" --socket "$socket_path" daemon start \
  --state-dir "$state_dir" --pid-file "$pid_file" --wait-secs 20
"${runner[@]}" "$binary" --socket "$socket_path" create heavy-live \
  --suite bookworm --bootstrap-method cached_rootfs >/dev/null

cd "$work_dir"
printf 'workers=%s cleanup_workers=%s rootfs_source=%s\n' "$workers" "$cleanup_workers" "$rootfs_source"
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
