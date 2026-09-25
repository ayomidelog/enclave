#!/usr/bin/env bash
# The lifecycle of a quota-backed workspace.
#
# This is a separate benchmark from live-lifecycle.sh because it exercises a
# different code path: a workspace with a disk quota gets its own ext4 image,
# which is attached to a loop device and mounted on every start and released on
# every stop. That path costs tens of milliseconds of mount and loop work, and it
# appears nowhere in the eight-workspace benchmark, whose workspaces are not
# quota-backed. Publishing the two together is what keeps a change to one of them
# from being read as a change to the other.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
source "$script_dir/common.sh"

binary=${ENCLAVE_BINARY:-$repo_dir/target/release/enclave}
disk_mb=${ENCLAVE_QUOTA_MB:-256}
rootfs_source=${ENCLAVE_LIVE_ROOTFS:-/root/.local/state/enclave/sandboxes/rootfs-cache/bookworm}

if [[ $(id -u) -eq 0 ]]; then runner=(); else runner=(sudo -n); fi
if ! "${runner[@]}" test -x "$binary"; then
  printf 'error: build the binary first: %s (cargo build --release)\n' "$binary" >&2
  exit 1
fi
if ! "${runner[@]}" test -d "$rootfs_source"; then
  printf 'error: cached rootfs not found: %s\n' "$rootfs_source" >&2
  exit 1
fi

work_dir=$(mktemp -d /tmp/enclave-quota.XXXXXX)
socket_dir=$(mktemp -d /tmp/enclave-quota-socket.XXXXXX)
state_dir="$work_dir/state"
socket_path="$socket_dir/manager.sock"
pid_file="$socket_dir/manager.pid"

run() { "${runner[@]}" "$binary" --socket "$socket_path" "$@"; }

cleanup() {
  run destroy --force quota-live >/dev/null 2>&1 || true
  run daemon stop >/dev/null 2>&1 || true
  if [[ -f "$pid_file" ]]; then "${runner[@]}" kill "$(cat "$pid_file")" >/dev/null 2>&1 || true; fi
  sleep 1
  for target in "$state_dir"/sandboxes/*/workspaces/*/fs \
                "$state_dir"/sandboxes/*/rootfs \
                "$state_dir"/sandboxes/*/runtime/rootfs.mnt; do
    [[ -e "$target" ]] && "${runner[@]}" umount -l "$target" 2>/dev/null || true
  done
  # A loop device can outlive its mount, so detach any that still backs an image
  # this run created rather than leaving the host holding it.
  for device in /dev/loop*; do
    backing=$(cat "/sys/class/block/$(basename "$device")/loop/backing_file" 2>/dev/null) || continue
    case "$backing" in "$state_dir"*) "${runner[@]}" losetup -d "$device" 2>/dev/null || true ;; esac
  done
  "${runner[@]}" rm -rf "$work_dir" "$socket_dir"
}
trap cleanup EXIT INT TERM

"${runner[@]}" mkdir -p "$state_dir/sandboxes/rootfs-cache"
"${runner[@]}" cp -a "$rootfs_source" "$state_dir/sandboxes/rootfs-cache/"
"${runner[@]}" "$binary" --socket "$socket_path" daemon start \
  --state-dir "$state_dir" --pid-file "$pid_file" --wait-secs 20 >/dev/null
run create quota-live --suite bookworm --bootstrap-method cached_rootfs >/dev/null
run workspace create quota-live quota --disk-mb "$disk_mb" >/dev/null

image=$(echo "$state_dir"/sandboxes/*/workspaces/*/fs.img)
printf 'binary=%s disk_mb=%s image=%s\n' "$binary" "$disk_mb" "$image"
host_metadata

# /usr/bin/time cannot run a shell function, so the timing is taken around the
# call rather than by wrapping it.
measure() {
  local started finished
  started=$(date +%s%N)
  "$@" >/dev/null
  finished=$(date +%s%N)
  awk -v s="$started" -v f="$finished" 'BEGIN { printf "%.2f", (f - s) / 1000000000 }'
}

# `workspace create` mounts the image to lay out the root overlay and the
# workspace /tmp inside it, and it leaves the image mounted. The first start
# therefore finds the image already attached and skips the mount, which is not
# what a restart costs, so it is measured on its own and reported under its own
# name. Every start after a stop attaches the image again, and those are the two
# numbers worth comparing.
after_create_start=$(measure run workspace start quota-live quota)
after_create_stop=$(measure run workspace stop quota-live quota)
cold_start=$(measure run workspace start quota-live quota)
cold_stop=$(measure run workspace stop quota-live quota)
warm_start=$(measure run workspace start quota-live quota)
warm_stop=$(measure run workspace stop quota-live quota)

printf 'QUOTA_BOOT_AFTER_CREATE_SECONDS=%s\n' "$after_create_start"
printf 'QUOTA_SHUTDOWN_AFTER_CREATE_SECONDS=%s\n' "$after_create_stop"
printf 'QUOTA_BOOT_COLD_SECONDS=%s\n' "$cold_start"
printf 'QUOTA_SHUTDOWN_COLD_SECONDS=%s\n' "$cold_stop"
printf 'QUOTA_BOOT_WARM_SECONDS=%s\n' "$warm_start"
printf 'QUOTA_SHUTDOWN_WARM_SECONDS=%s\n' "$warm_stop"

if find /sys/class/block -path '*/loop/backing_file' -exec grep -l "$state_dir" {} + 2>/dev/null | grep -q .; then
  printf 'QUOTA_LOOP_RELEASED=no\n'
  exit 1
fi
printf 'QUOTA_LOOP_RELEASED=yes\n'
printf 'QUOTA_LIFECYCLE_TEST=passed\n'
