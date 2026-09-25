#!/usr/bin/env bash
# The pause/resume lifecycle tier.
#
# A pause freezes the workspaces in a sandbox cgroup and a resume thaws them.
# Unlike a stop, it does not release the runtime: the processes keep their pids,
# their memory, and their mounts, so the work they were doing continues where it
# left off. That is a different tier from a stop/start cycle, and it is the tier a
# caller reaches for when they want the sandbox out of the way without paying for
# a new runtime. It is published separately for the same reason the quota suite
# is: a number for one tier must not be read as a number for another.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
source "$script_dir/common.sh"

binary=${ENCLAVE_BINARY:-$repo_dir/target/release/enclave}
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

work_dir=$(mktemp -d /tmp/enclave-pause.XXXXXX)
socket_dir=$(mktemp -d /tmp/enclave-pause-socket.XXXXXX)
state_dir="$work_dir/state"
socket_path="$socket_dir/manager.sock"
pid_file="$socket_dir/manager.pid"

run() { "${runner[@]}" "$binary" --socket "$socket_path" "$@"; }

cleanup() {
  run destroy --force pause-live >/dev/null 2>&1 || true
  run daemon stop >/dev/null 2>&1 || true
  if [[ -f "$pid_file" ]]; then "${runner[@]}" kill "$(cat "$pid_file")" >/dev/null 2>&1 || true; fi
  sleep 1
  for target in "$state_dir"/sandboxes/*/workspaces/*/fs \
                "$state_dir"/sandboxes/*/rootfs \
                "$state_dir"/sandboxes/*/runtime/rootfs.mnt; do
    [[ -e "$target" ]] && "${runner[@]}" umount -l "$target" 2>/dev/null || true
  done
  for device in /dev/loop*; do
    backing=$(cat "/sys/class/block/$(basename "$device")/loop/backing_file" 2>/dev/null) || continue
    case "$backing" in "$state_dir"*) "${runner[@]}" losetup -d "$device" 2>/dev/null || true ;; esac
  done
  "${runner[@]}" rm -rf "$work_dir" "$socket_dir"
}
trap cleanup EXIT INT TERM

# The workspace filesystem is mounted at /home inside the workspace, so a file the
# runtime writes to /home appears at the root of the mounted image on the host.
counter_file() { printf '%s' "$state_dir"/sandboxes/*/workspaces/*/fs/counter; }
read_counter() {
  local file
  file=$(counter_file)
  if [[ -f "$file" ]]; then cat "$file"; else printf '0'; fi
}

"${runner[@]}" mkdir -p "$state_dir/sandboxes/rootfs-cache"
"${runner[@]}" cp -a "$rootfs_source" "$state_dir/sandboxes/rootfs-cache/"
"${runner[@]}" "$binary" --socket "$socket_path" daemon start \
  --state-dir "$state_dir" --pid-file "$pid_file" --wait-secs 20 >/dev/null
run create pause-live --suite bookworm --bootstrap-method cached_rootfs >/dev/null
run workspace create pause-live w1 --memory-mb 128 >/dev/null
run workspace start pause-live w1 >/dev/null
runtime_pid=$(cat "$state_dir"/sandboxes/*/workspaces/*/runtime/session.pid 2>/dev/null | head -1)

# The counter is what makes the freeze observable. Each iteration opens, writes,
# and closes the file, so the value on disk is the value at the last instruction
# the frozen cgroup was allowed to run.
run workspace exec pause-live w1 -- sh -c \
  'n=0; while :; do n=$((n+1)); echo $n > /home/counter; sleep 0.05; done' \
  >/dev/null 2>&1 &
sleeper=$!
sleep 2
before=$(read_counter)

measure() {
  local started finished
  started=$(date +%s%N)
  "$@" >/dev/null
  finished=$(date +%s%N)
  awk -v s="$started" -v f="$finished" 'BEGIN { printf "%.3f", (f - s) / 1000000000 }'
}

pause_seconds=$(measure run pause pause-live)
sleep 1
paused_a=$(read_counter)
sleep 1
paused_b=$(read_counter)

resume_seconds=$(measure run resume pause-live)
sleep 1
resumed=$(read_counter)

kill "$sleeper" >/dev/null 2>&1 || true

printf 'binary=%s runtime_pid=%s\n' "$binary" "${runtime_pid:-unknown}"
host_metadata
printf 'PAUSE_SECONDS=%s\n' "$pause_seconds"
printf 'RESUME_SECONDS=%s\n' "$resume_seconds"
printf 'COUNTER_BEFORE_PAUSE=%s\n' "$before"
printf 'COUNTER_WHILE_PAUSED_A=%s\n' "$paused_a"
printf 'COUNTER_WHILE_PAUSED_B=%s\n' "$paused_b"
printf 'COUNTER_AFTER_RESUME=%s\n' "$resumed"

status=passed
if [[ "$paused_a" != "$paused_b" ]]; then
  printf 'PAUSE_DID_NOT_FREEZE=yes\n'
  status=failed
else
  printf 'PAUSE_DID_NOT_FREEZE=no\n'
fi
if [[ "$resumed" -le "$paused_b" ]]; then
  printf 'RESUME_DID_NOT_CONTINUE=yes\n'
  status=failed
else
  printf 'RESUME_DID_NOT_CONTINUE=no\n'
fi
# The runtime must be the same process on both sides of the pause, or the counter
# above would describe a restart rather than a resume.
if [[ -n "$runtime_pid" ]] && ! "${runner[@]}" test -e "/proc/$runtime_pid"; then
  printf 'RUNTIME_DID_NOT_SURVIVE=yes\n'
  status=failed
else
  printf 'RUNTIME_DID_NOT_SURVIVE=no\n'
fi

run workspace stop pause-live w1 >/dev/null
printf 'PAUSE_LIFECYCLE_TEST=%s\n' "$status"
[[ "$status" == passed ]]
