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


# Bind a host port and keep it bound until the returned pid is killed, so a
# start that has to publish that port cannot.
# The exec matters: without it the background job is the shell running the
# function, so killing that job leaves the python process holding the port and
# the recovery start below would fail for the wrong reason.
hold_host_port() {
  exec python3 -c '
import socket, sys, time
s = socket.socket()
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen(1)
print("held", flush=True)
time.sleep(600)
' "$1"
}

# How many workspace cgroups the sandbox has right now. A workspace cgroup
# cannot be removed while a process of that workspace is still in it, so a
# count that came back down is evidence that the runtime and its descendants
# are gone rather than merely signalled.
count_workspace_cgroups() {
  local directory=/sys/fs/cgroup/enclave-sb-$1 count=0 path
  if [[ ! -d "$directory" ]]; then
    printf '0'
    return
  fi
  for path in "$directory"/enclave-ws-*; do
    [[ -d "$path" ]] && count=$((count + 1))
  done
  printf '%s' "$count"
}

# The names of every interface on the host, so a failed start can be checked for
# having left one behind.
interface_names() { ip -o link show | awk -F': ' '{ print $2 }' | sed 's/@.*//' | sort; }
run up >/dev/null
assert_ports_held started

run pause ports-live >/dev/null
assert_ports_released paused

# A paused sandbox must keep its ports withdrawn across a daemon restart. Pausing
# withdraws the listeners so the operator can bind the port the sandbox declared;
# the workspaces keep their running status, so a restart that republished every
# running workspace would take the port back with no command having asked for it.
# The daemon is restarted rather than a second pause issued because the republish
# only happens at startup.
run daemon stop >/dev/null 2>&1 || true
for _ in $(seq 1 100); do
  [[ ! -S "$socket_path" ]] && break
  sleep 0.1
done
"${runner[@]}" "$binary" --socket "$socket_path" daemon start \
  --state-dir "$state_dir" --pid-file "$pid_file" --wait-secs 20 >/dev/null
assert_ports_released paused_across_restart

run resume ports-live >/dev/null
assert_ports_held resumed

run workspace stop ports-live dev >/dev/null
assert_ports_released stopped

# A start that cannot publish its ports has to undo the start rather than leave a
# running workspace with no listeners. The publisher binds the host ports after the
# runtime is up, so a port the host is already using is the failure the rollback
# exists for. The port is held here rather than made to fail some other way because
# holding it is exactly what a client on the host does.
sandbox_id=$(basename "$(ls -d "$state_dir"/sandboxes/ports-live-* 2>/dev/null | head -1)")
cgroups_before=$(count_workspace_cgroups "$sandbox_id")
interfaces_before=$(interface_names)
if [[ "$cgroups_before" != "0" ]]; then
  printf 'FAIL: the workspace already had a cgroup before the start that should fail\n' >&2
  exit 1
fi

hold_host_port "${host_ports[0]}" &
holder=$!
for _ in $(seq 1 50); do
  port_is_held "${host_ports[0]}" && break
  sleep 0.1
done
if ! port_is_held "${host_ports[0]}"; then
  printf 'FAIL: could not hold host port %s to force the publication failure\n' "${host_ports[0]}" >&2
  exit 1
fi

# The two checks below are about what a failed start leaves behind, so a failed
# start that never got as far as creating either resource would pass them for the
# wrong reason. The watcher samples both while the start runs, and the run fails
# unless it saw each one exist.
# The watcher runs in its own process, so what it saw is recorded in files
# rather than in variables it could not set in this shell.
seen_dir=$(mktemp -d "$work_dir/seen.XXXXXX")
(
  while :; do
    [[ $(count_workspace_cgroups "$sandbox_id") != '0' ]] && : > "$seen_dir/cgroup"
    [[ $(interface_names) != "$interfaces_before" ]] && : > "$seen_dir/interface"
    sleep 0.05
  done
) &
watcher=$!

if run workspace start ports-live dev >/dev/null 2>&1; then
  printf 'FAIL: a start reported success while one of its ports was already held\n' >&2
  exit 1
fi
kill "$watcher" >/dev/null 2>&1 || true
wait "$watcher" 2>/dev/null || true
printf 'start_refused_when_a_port_is_held=yes\n'
if [[ ! -e "$seen_dir/cgroup" ]]; then
  printf 'FAIL: the failed start never created a workspace cgroup, so the check that it removed one is vacuous\n' >&2
  exit 1
fi
if [[ ! -e "$seen_dir/interface" ]]; then
  printf 'FAIL: the failed start never created an interface, so the check that it removed one is vacuous\n' >&2
  exit 1
fi
printf 'failed_start_created_both_resources=yes\n'

status=$(run workspace status ports-live dev)
if ! printf '%s\n' "$status" | grep -q '^status: stopped$'; then
  printf 'FAIL: a start that could not publish left the workspace in this state:\n%s\n' "$status" >&2
  exit 1
fi
printf 'workspace_stopped_after_failed_publish=yes\n'

# The ports that were not contended must not have been left bound either: the
# rollback clears the whole set rather than only the one that failed.
for port in "${host_ports[1]}" "${host_ports[2]}"; do
  if port_is_held "$port"; then
    printf 'FAIL: host port %s was left bound by a start that failed\n' "$port" >&2
    exit 1
  fi
done
printf 'uncontended_ports_not_left_bound=yes\n'

cgroups_after=$(count_workspace_cgroups "$sandbox_id")
printf 'workspace_cgroups_before=%s after=%s\n' "$cgroups_before" "$cgroups_after"
if [[ "$cgroups_after" != "$cgroups_before" ]]; then
  printf 'FAIL: the failed start left a workspace cgroup behind\n' >&2
  exit 1
fi
printf 'no_cgroup_left_after_failed_start=yes\n'

# Three independent signals, none of which is the registry record the request
# itself wrote: the workspace cgroup is kernel state that cannot exist without a
# live process in it, the interface is kernel state that cannot exist without a
# veth, and the port is kernel state that cannot be bound without a listener. A
# rollback that only reset the record would leave all three behind.
interfaces_after=$(interface_names)
left_behind=$(comm -13 <(printf '%s\n' "$interfaces_before") <(printf '%s\n' "$interfaces_after"))
if [[ -n "$left_behind" ]]; then
  printf 'FAIL: the failed start left an interface behind: %s\n' "$left_behind" >&2
  exit 1
fi
printf 'no_interface_left_after_failed_start=yes\n'

kill "$holder" >/dev/null 2>&1 || true
wait "$holder" 2>/dev/null || true
for _ in $(seq 1 50); do
  port_is_held ${host_ports[0]} || break
  sleep 0.1
done
if port_is_held ${host_ports[0]}; then
  printf 'FAIL: the holder did not release host port %s\n' ${host_ports[0]} >&2
  exit 1
fi

# The port is free again, and the start that could not publish before has to
# publish now. Without this the assertions above would pass on a workspace that
# could no longer start at all.
run workspace start ports-live dev >/dev/null
assert_ports_held restarted

run workspace destroy ports-live dev >/dev/null
assert_ports_released destroyed

printf 'LIVE_PORT_LIFECYCLE_TEST=passed\n'
