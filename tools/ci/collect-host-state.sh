#!/usr/bin/env bash
# Write down the host state a lifecycle run left behind.
#
# A lifecycle failure is a statement about host state, and that state is gone by
# the time anyone reads the log: a CI runner is destroyed when the job ends. This
# captures what the suite's own assertions are about — cgroups and what is in
# them, interfaces, firewall rules, mounts, loop devices, processes, and the
# daemon logs each test wrote — so a failure can be read rather than reproduced.
#
# `set -e` is deliberately absent: a probe that fails is itself a finding, and
# stopping at the first one would drop everything after it.
#
# Usage: collect-host-state.sh [OUTPUT_DIR]
set -uo pipefail

out=${1:-host-state}
mkdir -p "$out"

capture() {
  local name=$1
  shift
  {
    printf '$ %s\n\n' "$*"
    "$@" 2>&1
  } >"$out/$name.txt"
}

capture uname uname -a
capture uptime uptime
capture loadavg cat /proc/loadavg
capture memory free -m

# Cgroups first: this is where a leaked runtime shows up, and the tree says which
# process is holding a group that refused to be removed.
{
  printf '$ find /sys/fs/cgroup -maxdepth 4 -name "enclave*"\n\n'
  find /sys/fs/cgroup -maxdepth 4 -name 'enclave*' -print 2>&1
  printf '\n$ processes in each of them\n\n'
  while IFS= read -r group; do
    [[ -d $group ]] || continue
    printf '%s: ' "$group"
    cat "$group/cgroup.procs" 2>/dev/null | tr '\n' ' '
    printf '\n'
  done < <(find /sys/fs/cgroup -maxdepth 4 -name 'enclave*' -type d 2>/dev/null)
} >"$out/cgroups.txt" 2>&1

capture links ip -o link show
capture addresses ip -o address show
capture routes ip route show
capture firewall iptables-save
capture mounts findmnt --submounts /tmp
capture mountinfo cat /proc/self/mountinfo
capture loop-devices losetup -a
capture block-devices lsblk -o NAME,SIZE,TYPE,MOUNTPOINT

# The suite's own processes, which are the ones that should have exited.
{
  printf '$ ps -eo pid,ppid,stat,etime,args | grep -E "enclave|session-helper|workspace-session"\n\n'
  ps -eo pid,ppid,stat,etime,args 2>/dev/null \
    | grep -E 'enclave|session-helper|workspace-session' \
    | grep -v grep
} >"$out/processes.txt" 2>&1

# The daemon logs. Every test builds its own state directory under the temporary
# directory, and the daemon writes what it did there.
{
  printf '$ daemon logs and state left under /tmp\n\n'
  find /tmp -maxdepth 4 -name 'daemon.log' -print 2>/dev/null
  printf '\n$ enclave state directories\n\n'
  find /tmp -maxdepth 2 -name 'enclave-*' -type d -print 2>/dev/null
} >"$out/leftovers.txt" 2>&1
for log in $(find /tmp -maxdepth 4 -name 'daemon.log' 2>/dev/null | head -20); do
  safe=$(printf '%s' "$log" | tr '/.' '__')
  tail -n 400 "$log" >"$out/daemon-$safe.txt" 2>&1
done

capture dmesg-tail dmesg --ctime

printf 'collected host state into %s\n' "$out"
ls -la "$out"
