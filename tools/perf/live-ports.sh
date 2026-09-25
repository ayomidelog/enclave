#!/usr/bin/env bash
# What several published ports of one workspace do across its lifecycle.
#
# A workspace publishes a set of ports, and the daemon holds the host listeners for it.
# The lifecycle has to keep that set consistent with the workspace: a pause withdraws the
# listeners because the runtime is frozen and cannot answer, a resume republishes them,
# and a stop and a destroy leave none behind. Each of those is a different code path in
# the daemon, and the only authority on whether a port is published is whether the host
# port can be bound, which is also what a client would find.
#
# This runs against a daemon the script starts in its own state directory, so it does not
# touch any sandbox that is already running.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
source "$script_dir/common.sh"

binary=${ENCLAVE_BINARY:-$repo_dir/target/release/enclave}
rootfs_source=${ENCLAVE_LIVE_ROOTFS:-/root/.local/state/enclave/sandboxes/rootfs-cache/bookworm}
fixture=$script_dir/live-ports.Enclavefile

if [[ $(id -u) -eq 0 ]]; then runner=(); else runner=(sudo -n); fi
if ! "${runner[@]}" test -x "$binary"; then
  printf 'error: build the binary first: %s (cargo build --release)\n' "$binary" >&2
  exit 1
fi
if ! "${runner[@]}" test -d "$rootfs_source"; then
  printf 'error: cached rootfs not found: %s\n' "$rootfs_source" >&2
  exit 1
fi

work_dir=$(mktemp -d /tmp/enclave-ports.XXXXXX)
socket_dir=$(mktemp -d /tmp/enclave-ports-socket.XXXXXX)
state_dir="$work_dir/state"
socket_path="$socket_dir/manager.sock"
pid_file="$socket_dir/manager.pid"

cleanup() {
  "${runner[@]}" "$binary" --socket "$socket_path" down >/dev/null 2>&1 || true
  "${runner[@]}" "$binary" --socket "$socket_path" destroy --force ports-live >/dev/null 2>&1 || true
  "${runner[@]}" "$binary" --socket "$socket_path" daemon stop >/dev/null 2>&1 || true
  if [[ -f "$pid_file" ]]; then "${runner[@]}" kill "$(cat "$pid_file")" >/dev/null 2>&1 || true; fi
  "${runner[@]}" umount -l "$state_dir"/sandboxes/*/runtime/rootfs.mnt >/dev/null 2>&1 || true
  "${runner[@]}" rm -rf "$work_dir" "$socket_dir"
}
trap cleanup EXIT INT TERM

# Three free host ports, held until all of them have been read. Asking one at a time
# returns a port the kernel has just been told to release, so two calls can be handed the
# same number.
mapfile -t host_ports < <(python3 -c '
import socket
listeners = []
for _ in range(3):
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    listeners.append(s)
for s in listeners:
    print(s.getsockname()[1])
')
printf 'host_ports=%s\n' "${host_ports[*]}"

# Whether anything is bound to a host port right now.
port_is_held() {
  python3 -c '
import socket, sys
s = socket.socket()
try:
    s.bind(("127.0.0.1", int(sys.argv[1])))
except OSError:
    sys.exit(0)
sys.exit(1)
' "$1"
}

assert_ports_held() {
  local label=$1 port
  for port in "${host_ports[@]}"; do
    if ! port_is_held "$port"; then
      printf 'FAIL: %s: host port %s is not bound\n' "$label" "$port" >&2
      exit 1
    fi
  done
  printf 'ports_held_%s=yes\n' "$label"
}

assert_ports_released() {
  local label=$1 port
  for port in "${host_ports[@]}"; do
    if port_is_held "$port"; then
      printf 'FAIL: %s: host port %s is still held\n' "$label" "$port" >&2
      exit 1
    fi
  done
  printf 'ports_released_%s=yes\n' "$label"
}

"${runner[@]}" mkdir -p "$state_dir/sandboxes/rootfs-cache"
"${runner[@]}" cp -a "$rootfs_source" "$state_dir/sandboxes/rootfs-cache/"
sed -e "s/__PORT_A__/${host_ports[0]}/" \
    -e "s/__PORT_B__/${host_ports[1]}/" \
    -e "s/__PORT_C__/${host_ports[2]}/" "$fixture" > "$work_dir/Enclavefile"

"${runner[@]}" "$binary" --socket "$socket_path" daemon start \
  --state-dir "$state_dir" --pid-file "$pid_file" --wait-secs 20 >/dev/null
"${runner[@]}" "$binary" --socket "$socket_path" create ports-live \
  --suite bookworm --bootstrap-method cached_rootfs >/dev/null

run() { (cd "$work_dir" && "${runner[@]}" "$binary" --socket "$socket_path" "$@"); }

run up >/dev/null
assert_ports_held started

run pause ports-live >/dev/null
assert_ports_released paused

run resume ports-live >/dev/null
assert_ports_held resumed

run workspace stop ports-live dev >/dev/null
assert_ports_released stopped

run workspace start ports-live dev >/dev/null
assert_ports_held restarted

run workspace destroy ports-live dev >/dev/null
assert_ports_released destroyed

printf 'LIVE_PORT_LIFECYCLE_TEST=passed\n'
