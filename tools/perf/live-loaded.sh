#!/usr/bin/env bash
# What a sandbox full of busy workspaces costs, and what it must still get right.
#
# The eight-workspace lifecycle benchmark measures a sandbox whose workspaces are
# idle: their run command has already exited, so the only work in the sandbox is
# the lifecycle request itself. That is the number a start should be compared
# against, but it is not the situation a start actually happens in on a machine
# that is being used. This run measures the other case: twelve workspaces, each
# running a CPU loop, all of them live at once, inside a sandbox capped at a
# quarter of the host.
#
# Two things make generating that load safe on a host that is doing other work.
# The workspaces ask for more CPU than the sandbox is allowed, so the cap binds
# and the demand is refused rather than served, and the run reads the sandbox's
# own throttle counter and CPU accounting to show that the refusal happened. It
# also prints the whole host's busy fraction over the loaded window, so the load
# this run put on the machine is a published number rather than an assumption.
#
# The load is also the point. A start that has to place twelve runtimes while
# every one of them is competing for one CPU is the start whose tail a regression
# appears in, and the control plane has to keep answering while it happens. Every
# assertion below is made while the load is running: the workspaces are running,
# each one can be entered, each one's /tmp is a linked and writable directory of
# its own, the published ports are bound, and a stop of one workspace still
# completes and is still verifiable. The teardown then has to leave nothing
# behind, which is a stronger claim with twelve runtimes to release than with
# one.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=common.sh
source "$script_dir/common.sh"

binary=${ENCLAVE_BINARY:-$repo_dir/target/release/enclave}
rootfs_source=${ENCLAVE_LIVE_ROOTFS:-/root/.local/state/enclave/sandboxes/rootfs-cache/bookworm}
# The twelve-workspace fixture is the default, and it is the one whose numbers are
# published. A caller with a larger host points this at another file rather than
# editing the published fixture, so the numbers that were measured stay attached
# to the fixture that produced them.
fixture=${ENCLAVE_LOADED_ENCLAVEFILE:-$script_dir/live-loaded.Enclavefile}
sandbox_name=loaded-live

if [[ $(id -u) -eq 0 ]]; then runner=(); else runner=(sudo -n); fi
if ! "${runner[@]}" test -x "$binary"; then
  printf 'error: build the binary first: %s (cargo build --release)\n' "$binary" >&2
  exit 1
fi
if ! "${runner[@]}" test -d "$rootfs_source"; then
  printf 'error: cached rootfs not found: %s\n' "$rootfs_source" >&2
  exit 1
fi

work_dir=$(mktemp -d /tmp/enclave-loaded.XXXXXX)
socket_dir=$(mktemp -d /tmp/enclave-loaded-socket.XXXXXX)
state_dir="$work_dir/state"
socket_path="$socket_dir/manager.sock"
pid_file="$socket_dir/manager.pid"

run() { "${runner[@]}" "$binary" --socket "$socket_path" "$@"; }

# The workspaces run detached CPU loops, so a run that fails half way can leave
# processes behind that outlive the daemon. They are all in the sandbox cgroup,
# which is named after the sandbox, so they can be found and killed without
# touching any other sandbox on the host.
cleanup() {
  run down >/dev/null 2>&1 || true
  run destroy --force "$sandbox_name" >/dev/null 2>&1 || true
  run daemon stop >/dev/null 2>&1 || true
  if [[ -f "$pid_file" ]]; then "${runner[@]}" kill "$(cat "$pid_file")" >/dev/null 2>&1 || true; fi
  for proc in /proc/[0-9]*; do
    pid=${proc##*/}
    cgroup=$(cat "$proc/cgroup" 2>/dev/null || true)
    case "$cgroup" in
      *"enclave-sb-$sandbox_name-"*) "${runner[@]}" kill -KILL "$pid" >/dev/null 2>&1 || true ;;
    esac
  done
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

# The ports are held until all of them have been read. Asking one at a time
# returns a port the kernel has just been told to release, so two calls can be
# handed the same number.
mapfile -t host_ports < <(python3 -c '
import socket
listeners = []
for _ in range(2):
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    listeners.append(s)
for s in listeners:
    print(s.getsockname()[1])
')
printf 'host_ports=%s\n' "${host_ports[*]}"

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

# The names of every interface on the host, so the teardown can be checked for
# having added or removed one. The set is taken before the run and compared
# after it rather than counted, because the host this runs on may be carrying
# interfaces that belong to something else.
interface_names() { ip -o link show | awk -F': ' '{ print $2 }' | sed 's/@.*//' | sort; }

# The workspaces of the fixture, in the order the fixture declares them.
mapfile -t workspaces < <(sed -n 's/^\[workspace\.\(.*\)\]$/\1/p' "$fixture")

# Which of them are on the quota tier. A quota-backed workspace mounts its /tmp
# from a directory inside its image rather than from a fresh tmpfs, so the probe
# has to answer differently for the two tiers; reading the tier out of the
# fixture keeps the two in step.
mapfile -t quota_workspaces < <(awk '
  /^\[workspace\./ { name = $0; sub(/^\[workspace\./, "", name); sub(/\]$/, "", name) }
  /^disk_mb[[:space:]]*=/ { if (name != "") print name }
' "$fixture")
"${runner[@]}" mkdir -p "$state_dir/sandboxes/rootfs-cache"
"${runner[@]}" cp -a "$rootfs_source" "$state_dir/sandboxes/rootfs-cache/"
sed -e "s/__PORT_A__/${host_ports[0]}/" \
    -e "s/__PORT_B__/${host_ports[1]}/" "$fixture" > "$work_dir/Enclavefile"
interfaces_before=$(interface_names)

"${runner[@]}" "$binary" --socket "$socket_path" daemon start \
  --state-dir "$state_dir" --pid-file "$pid_file" --wait-secs 20 >/dev/null
cd "$work_dir"
run create "$sandbox_name" --suite bookworm --bootstrap-method cached_rootfs >/dev/null

sandbox_id=$(basename "$(ls -d "$state_dir"/sandboxes/"$sandbox_name"-* 2>/dev/null | head -1)")
sandbox_cgroup=/sys/fs/cgroup/enclave-sb-$sandbox_id

measure_seconds() {
  local started finished
  started=$(date +%s%N)
  "$@" >&2
  finished=$(date +%s%N)
  awk -v s="$started" -v f="$finished" 'BEGIN { printf "%.3f", (f - s) / 1000000000 }'
}

# The eight counters of the whole host's CPU, so the load this run generated can
# be reported as the fraction of the machine it actually used.
host_cpu_sample() { awk '/^cpu / { print $2, $3, $4, $5, $6, $7, $8, $9 }' /proc/stat; }

cpu_busy_percent() {
  python3 -c '
import sys
before = [int(value) for value in sys.argv[1].split()]
after = [int(value) for value in sys.argv[2].split()]
delta = [b - a for a, b in zip(before, after)]
total = sum(delta)
if total <= 0:
    print("unavailable")
else:
    idle = delta[3] + delta[4]
    print("%.1f" % (100 * (total - idle) / total))
' "$1" "$2"
}

cgroup_counter() {
  # The value of one counter in the sandbox cgroup's cpu.stat, which is where the
  # kernel reports how much CPU the sandbox used and how often it was refused.
  awk -v key="$2" '$1 == key { print $2; exit }' "$1/cpu.stat" 2>/dev/null || printf 'unavailable'
}

cpu_seconds_from_usec() {
  awk -v usec="$1" 'BEGIN { printf "%.3f", usec / 1000000 }'
}

printf 'binary=%s fixture=%s workspaces=%s sandbox=%s\n' \
  "$binary" "$fixture" "${#workspaces[@]}" "$sandbox_id"
host_metadata

# The loaded window starts here and ends just before the teardown, so the CPU
# accounting below covers exactly the part of the run that carried the load.
host_cpu_before=$(host_cpu_sample)
window_started=$(date +%s%N)
boot_seconds=$(measure_seconds run up --cache-setup)
interfaces_loaded=$(interface_names)
throttled_before=$(cgroup_counter "$sandbox_cgroup" nr_throttled)
usage_before=$(cgroup_counter "$sandbox_cgroup" usage_usec)
printf 'LOADED_BOOT_SECONDS=%s\n' "$boot_seconds"
printf 'LOADED_SANDBOX_CPU_MAX=%s\n' "$(cat "$sandbox_cgroup/cpu.max" 2>/dev/null || printf unavailable)"

# Every workspace has to be up and have a runtime, because everything below is
# measured against a sandbox that is actually carrying the load the fixture
# declares. A run command that failed to launch would leave the sandbox looking
# idle, and an idle sandbox would make the rest of this report describe nothing.
# The run has to have created interfaces of its own, or the check that none
# were left behind afterwards would pass on a run that never made one.
interfaces_added=$(comm -13 <(printf '%s\n' "$interfaces_before") <(printf '%s\n' "$interfaces_loaded"))
printf 'LOADED_INTERFACES_ADDED=%s\n' "$(printf '%s\n' "$interfaces_added" | tr '\n' ' ')"
if [[ -z "$interfaces_added" ]]; then
  printf 'FAIL: the run created no interface, so the teardown check is vacuous\n' >&2
  exit 1
fi

ps_output=$(run ps)
running=$(printf '%s\n' "$ps_output" | grep -c ' running ' || true)
printf 'LOADED_WORKSPACES_RUNNING=%s\n' "$running"
if [[ "$running" -ne "${#workspaces[@]}" ]]; then
  printf 'FAIL: %s workspaces are running, expected %s\n' "$running" "${#workspaces[@]}" >&2
  printf '%s\n' "$ps_output" >&2
  exit 1
fi

# The load is not just declared, it is happening. A workspace whose run command
# started and exited would leave the sandbox idle, and the throttle counter would
# stay at zero. Waiting for the counter to move is what makes the rest of this
# run a measurement of a loaded sandbox rather than of a quiet one.
for _ in $(seq 1 40); do
  throttled_now=$(cgroup_counter "$sandbox_cgroup" nr_throttled)
  if [[ "$throttled_now" != unavailable && "$throttled_now" -gt "$throttled_before" ]]; then
    break
  fi
  sleep 0.5
done

# Each workspace is entered and asked about its own /tmp. The bug this guards
# against is a /tmp that is mounted from a directory with no link in its parent,
# which reaches the workspace by path but cannot hold a new file; the counters
# that catch it are the link count, whether a write succeeds, and whether the
# mount's root inside its backing filesystem is the unlinked marker.
# The command is one line because the argument parser refuses a control
# character, so the probe is written readably and then folded onto one line.
probe=$(cat <<'PROBE' | tr '\n' ';'
set -u
n=$(stat -c %h /tmp)
r=$(awk '$5 == "/tmp" { print $4; exit }' /proc/self/mountinfo)
if touch /tmp/.enclave-loaded-write-probe; then w=ok; else w=fail; fi
rm -f /tmp/.enclave-loaded-write-probe
if m=$(mktemp -p /tmp); then k=ok; else k=fail; fi
rm -f "$m"
printf 'probe_nlink=%s probe_root=%s probe_write=%s probe_mktemp=%s\n' "$n" "$r" "$w" "$k"
PROBE
)

tmp_writable=0
tmp_linked=0
tmp_mounted=0
tmp_quota_tier=0
for workspace in "${workspaces[@]}"; do
  status_output=$(run workspace status "$sandbox_name" "$workspace")
  if ! printf '%s\n' "$status_output" | grep -q '^status: running$'; then
    printf 'FAIL: workspace %s is not running\n' "$workspace" >&2
    printf '%s\n' "$status_output" >&2
    exit 1
  fi
  probe_output=$(run workspace exec "$sandbox_name" "$workspace" -- sh -c "$probe")
  printf 'probe_%s %s\n' "$workspace" "$(printf '%s' "$probe_output" | tr '\n' ' ')"
  # The probe answers on one line, so each value is picked out of it by its
  # label rather than by its line.
  probe_write=$(printf '%s' "$probe_output" | sed -n 's/.*probe_write=\([a-z]*\).*/\1/p')
  probe_mktemp=$(printf '%s' "$probe_output" | sed -n 's/.*probe_mktemp=\([a-z]*\).*/\1/p')
  probe_nlink=$(printf '%s' "$probe_output" | sed -n 's/.*probe_nlink=\([0-9]*\).*/\1/p')
  probe_root=$(printf '%s' "$probe_output" | sed -n 's/.*probe_root=\([^ ]*\).*/\1/p')
  [[ "$probe_write" == ok ]] && tmp_writable=$((tmp_writable + 1))
  if [[ "$probe_mktemp" != ok ]]; then
    printf 'FAIL: mktemp in %s /tmp did not succeed\n' "$workspace" >&2
    exit 1
  fi
  [[ "$probe_nlink" =~ ^[0-9]+$ ]] && [[ "$probe_nlink" -ge 2 ]] && tmp_linked=$((tmp_linked + 1))
  [[ -n "$probe_root" && "$probe_root" != *deleted* ]] && tmp_mounted=$((tmp_mounted + 1))
  if [[ " ${quota_workspaces[*]} " == *" $workspace "* ]]; then
    # Inside the image the /tmp source is the workspace tmp directory, so a mount
    # root that names anything else means /tmp is not the directory the image holds.
    if [[ "$probe_root" != *".enclave-tmp"* && "$probe_root" != "tmp" ]]; then
      printf 'FAIL: quota workspace %s has /tmp mounted from %s, not from its workspace tmp directory\n' "$workspace" "$probe_root" >&2
      exit 1
    fi
    tmp_quota_tier=$((tmp_quota_tier + 1))
  elif [[ "$probe_root" != "/" ]]; then
    printf 'FAIL: workspace %s has /tmp mounted from %s, not from a private tmpfs\n' "$workspace" "$probe_root" >&2
    exit 1
  fi
done
printf 'LOADED_TMP_WRITABLE=%s/%s\n' "$tmp_writable" "${#workspaces[@]}"
printf 'LOADED_TMP_LINKED=%s/%s\n' "$tmp_linked" "${#workspaces[@]}"
printf 'LOADED_TMP_MOUNT_NOT_UNLINKED=%s/%s\n' "$tmp_mounted" "${#workspaces[@]}"
if [[ "$tmp_writable" -ne "${#workspaces[@]}" || "$tmp_linked" -ne "${#workspaces[@]}" || "$tmp_mounted" -ne "${#workspaces[@]}" ]]; then
  printf 'FAIL: a workspace /tmp is not a linked, writable, non-dangling directory\n' >&2
  exit 1
fi

# Each workspace leaves a file of its own in its own /tmp and then reads /tmp
# back. Seeing exactly its own file is the isolation claim in both directions:
# it cannot see another workspace's file, and no other workspace can have put
# one there.
for workspace in "${workspaces[@]}"; do
  run workspace exec "$sandbox_name" "$workspace" -- sh -c "printf 'owner' > /tmp/enclave-owner-$workspace" >/dev/null
done
tmp_isolated=0
for workspace in "${workspaces[@]}"; do
  listing=$(run workspace exec "$sandbox_name" "$workspace" -- sh -c 'ls -1 /tmp')
  if [[ "$listing" == "enclave-owner-$workspace" ]]; then
    tmp_isolated=$((tmp_isolated + 1))
  else
    printf 'FAIL: %s sees a /tmp that is not its own: %s\n' "$workspace" "$listing" >&2
  fi
done
printf 'LOADED_TMP_ISOLATED=%s/%s\n' "$tmp_isolated" "${#workspaces[@]}"
printf 'LOADED_TMP_QUOTA_TIER=%s/%s\n' "$tmp_quota_tier" "${#quota_workspaces[@]}"
if [[ "$tmp_isolated" -ne "${#workspaces[@]}" ]]; then
  printf 'FAIL: workspace /tmp directories are shared or stale\n' >&2
  exit 1
fi

assert_ports_held under_load

# A stop and a start of one workspace while the other eleven are running. This is
# the request whose latency a loaded host is most likely to change, and it is also
# the one with the most to get wrong, because releasing one workspace's mounts and
# cgroup happens while its neighbours keep the sandbox busy.
stop_under_load=$(measure_seconds run workspace stop "$sandbox_name" ws01)
start_under_load=$(measure_seconds run workspace start "$sandbox_name" ws01)
printf 'LOADED_STOP_UNDER_LOAD_SECONDS=%s\n' "$stop_under_load"
printf 'LOADED_START_UNDER_LOAD_SECONDS=%s\n' "$start_under_load"
run workspace status "$sandbox_name" ws01 | grep -q '^status: running$' || {
  printf 'FAIL: ws01 did not come back up under load\n' >&2
  exit 1
}
printf 'LOADED_RESTART_UNDER_LOAD=yes\n'

# The loaded window ends here. Everything above happened while twelve workspaces
# were competing for the sandbox's one CPU; everything below is the teardown.
window_finished=$(date +%s%N)
host_cpu_after=$(host_cpu_sample)
throttled_after=$(cgroup_counter "$sandbox_cgroup" nr_throttled)
usage_after=$(cgroup_counter "$sandbox_cgroup" usage_usec)
window_seconds=$(awk -v s="$window_started" -v f="$window_finished" 'BEGIN { printf "%.3f", (f - s) / 1000000000 }')
sandbox_cpu_seconds=$(cpu_seconds_from_usec "$((usage_after - usage_before))")

printf 'LOADED_HOST_BUSY_PERCENT=%s\n' "$(cpu_busy_percent "$host_cpu_before" "$host_cpu_after")"
printf 'LOADED_WINDOW_SECONDS=%s\n' "$window_seconds"
printf 'LOADED_SANDBOX_CPU_SECONDS=%s\n' "$sandbox_cpu_seconds"
printf 'LOADED_SANDBOX_THROTTLED=%s\n' "$((throttled_after - throttled_before))"
# The one claim that has to hold for this run to be safe to take on a host that is
# doing other work: the sandbox spent no more CPU than its cap allows, however much
# the workspaces inside it asked for. The cap is a quota per period, so the budget
# over the window is the cap multiplied by the wall time of the window. The
# bound is a tenth above the cap rather than at it because the controller
# enforces a quota per period and the counter it reports is coarser than the
# wall clock this window is measured with; the claim being made is that the
# sandbox stayed near its cap instead of using the 4.8 CPUs its workspaces
# asked for.
sandbox_cpus=$(awk -v seconds="$sandbox_cpu_seconds" -v wall="$window_seconds" 'BEGIN { printf "%.3f", seconds / wall }')
printf 'LOADED_SANDBOX_CPUS=%s\n' "$sandbox_cpus"
quota_cpus=$(awk -v cap="$(cat "$sandbox_cgroup/cpu.max" 2>/dev/null || printf 'max 100000')" 'BEGIN {
  split(cap, parts, " ");
  if (parts[1] == "max") { print "unbounded" } else { printf "%.3f", parts[1] / parts[2] }
}')
printf 'LOADED_SANDBOX_QUOTA_CPUS=%s\n' "$quota_cpus"
if [[ "$quota_cpus" != unbounded ]]; then
  if awk -v used="$sandbox_cpus" -v quota="$quota_cpus" 'BEGIN { exit !(used > quota * 1.10) }'; then
    printf 'FAIL: the sandbox used %s CPUs against a quota of %s\n' "$sandbox_cpus" "$quota_cpus" >&2
    exit 1
  fi
  printf 'LOADED_SANDBOX_CAP_HELD=yes\n'
fi
if [[ "$throttled_after" != unavailable && "$throttled_before" != unavailable && "$throttled_after" -le "$throttled_before" ]]; then
  printf 'FAIL: the sandbox was never throttled, so the workspaces were not asking for more than they were allowed\n' >&2
  exit 1
fi

shutdown_seconds=$(measure_seconds run down)
printf 'LOADED_SHUTDOWN_SECONDS=%s\n' "$shutdown_seconds"
assert_ports_released after_shutdown

# The teardown has to be complete, and with twelve runtimes to release that is a
# stronger claim than the one-workspace lifecycle makes. The sandbox cgroup is the
# load-bearing check: a child cgroup cannot be removed while any of its own
# children remain, so the sandbox cgroup being gone is evidence that all twelve
# workspace cgroups went with it.
if [[ -e "$sandbox_cgroup" ]]; then
  printf 'FAIL: sandbox cgroup %s survived the shutdown\n' "$sandbox_cgroup" >&2
  exit 1
fi
printf 'LOADED_SANDBOX_CGROUP_RELEASED=yes\n'

runtimes_left=0
for proc in /proc/[0-9]*; do
  pid=${proc##*/}
  proc_exe=$(readlink "$proc/exe" 2>/dev/null || true)
  [[ "$proc_exe" == "$binary" ]] || continue
  proc_cmd=$(tr '\0' ' ' < "$proc/cmdline" 2>/dev/null || true)
  case "$proc_cmd" in
    *"$state_dir"*) runtimes_left=$((runtimes_left + 1)) ;;
  esac
done
printf 'LOADED_RUNTIMES_LEFT=%s\n' "$runtimes_left"
if [[ "$runtimes_left" -ne 0 ]]; then
  printf 'FAIL: %s runtimes of this run survived the shutdown\n' "$runtimes_left" >&2
  exit 1
fi

interfaces_after=$(interface_names)
# The comparison is one-directional on purpose: this host may be carrying
# interfaces that belong to another sandbox, and one of those disappearing
# during the run is not this run's doing. An interface that appears and stays
# is.
left_behind=$(comm -13 <(printf '%s\n' "$interfaces_before") <(printf '%s\n' "$interfaces_after"))
printf 'LOADED_INTERFACES_LEFT_BEHIND=%s\n' "${left_behind:-none}"
if [[ -n "$left_behind" ]]; then
  printf 'FAIL: the run left interfaces on the host\n' >&2
  exit 1
fi
printf 'LOADED_INTERFACES_UNCHANGED=yes\n'

printf 'LIVE_LOADED_TEST=passed\n'
