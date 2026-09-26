#!/usr/bin/env bash
# Run the privileged integration suite, or say what the host is missing.
#
# The suite is `#[ignore]`d because it needs root and a host that can create
# namespaces, mounts, loop devices, cgroups, and firewall rules. It is not run by
# `cargo test` for that reason, and it is the only suite that exercises the
# lifecycle end to end: everything the unit tests cover is a decision, and
# everything this covers is the host state that decision produced.
#
# The host requirements are checked before the suite runs and reported one at a
# time with the package that provides them. A missing tool otherwise surfaces as
# a test failing somewhere in the middle of the run with an error about a command
# not being found, which reads as a bug in the code rather than as an incomplete
# host.
#
# Usage: privileged-suite.sh [cargo-test-args...]
#   ENCLAVE_SUITE   the ignored test binary to run (default: integration_suite).
#                   The stress suite needs the same host, so it goes through the
#                   same checks: ENCLAVE_SUITE=stress_suite.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
cd "$repo_dir"

if [[ $(id -u) -ne 0 ]]; then
  printf 'error: the privileged suite needs root; run it with sudo\n' >&2
  exit 2
fi

# sudo resets PATH, so a host with a rustup-managed toolchain for the invoking
# user and a distribution cargo for root would build with the distribution one.
# The repository pins its toolchain, and a cargo older than the pin cannot read
# the lock file, so the mismatch is reported here rather than as a parse error
# from cargo. CI installs the pinned toolchain for root, so this is a local-host
# concern.
cargo_bin=${CARGO:-$(command -v cargo || true)}
if [[ -z "$cargo_bin" ]]; then
  printf 'error: cargo is not on PATH\n' >&2
  exit 2
fi

pinned_channel=$(awk -F'"' '/^channel/ { print $2; exit }' rust-toolchain.toml)
cargo_version=$("$cargo_bin" --version 2>/dev/null | awk '{ print $2 }')
if [[ -n "$pinned_channel" && -n "$cargo_version" && "${cargo_version%.*}" != "${pinned_channel%.*}" ]]; then
  printf 'error: this repository pins Rust %s but the cargo on PATH is %s (%s)\n' \
    "$pinned_channel" "$cargo_version" "$cargo_bin" >&2
  printf '  sudo resets PATH, so run the suite with the pinned toolchain visible:\n' >&2
  printf '    sudo env "PATH=$PATH" "HOME=$HOME" bash %s\n' "$0" >&2
  exit 2
fi

# Each tool is paired with what it does for the suite, so a missing one is a
# statement about the run rather than a bare name.
missing=()
check() {
  local tool=$1 purpose=$2 package=$3
  if command -v "$tool" >/dev/null 2>&1; then
    return
  fi
  missing+=("$tool ($purpose) - install $package")
}

check ip 'workspace network namespaces and veth pairs' iproute2
check iptables 'anti-spoofing rules' iptables
check iptables-save 'reading the rules back to prove they are gone' iptables
check mount 'workspace storage and the sandbox rootfs' util-linux
check umount 'releasing those mounts' util-linux
check losetup 'the quota-backed disk image' util-linux
check truncate 'creating that image' coreutils
check nsenter 'entering a workspace namespace' util-linux
check unshare 'the session namespaces' util-linux
check mkfs.ext4 'formatting the quota image' e2fsprogs
check tar 'snapshot archives' tar

# The fixture rootfs is built from a static shell, because the host's /bin/sh is
# dynamically linked against libraries a minimal rootfs does not have.
if ! command -v busybox >/dev/null 2>&1; then
  missing+=("busybox (the fixture rootfs shell) - install busybox-static")
fi

if ((${#missing[@]})); then
  printf 'error: the privileged suite cannot run on this host:\n' >&2
  for entry in "${missing[@]}"; do
    printf '  - %s\n' "$entry" >&2
  done
  exit 2
fi

# The capabilities are checked by asking the kernel rather than by looking for a
# file, because a host can have the paths and still refuse the operation: cgroup
# v2 mounted read-only, a kernel built without overlayfs, or a container without
# the privilege to mount.
capability_probe=$(mktemp -d /tmp/enclave-capability.XXXXXX)
trap 'umount -l "$capability_probe/mnt" 2>/dev/null || true; rmdir "$capability_probe/mnt" 2>/dev/null || true; rm -rf "$capability_probe"' EXIT

problems=()

if ! mkdir -p "$capability_probe/mnt" 2>/dev/null; then
  problems+=('cannot create a directory under /tmp')
fi
if ! mount -t tmpfs tmpfs "$capability_probe/mnt" 2>/dev/null; then
  problems+=('cannot mount a tmpfs: the host is not a VM or lacks CAP_SYS_ADMIN')
else
  umount "$capability_probe/mnt"
fi
if ! mkdir -p /sys/fs/cgroup/enclave-capability-probe 2>/dev/null; then
  problems+=('cannot create a cgroup under /sys/fs/cgroup: cgroup v2 is not writable')
else
  rmdir /sys/fs/cgroup/enclave-capability-probe 2>/dev/null || true
fi
# A veth pair is what the suite actually builds for a workspace, and its name has
# to fit the kernel's 15-character interface limit, so the probe uses the same
# kind of interface at the same length rather than a longer name that fails for a
# reason that has nothing to do with the capability being tested.
if ! ip link add ecl-probe-a type veth peer name ecl-probe-b 2>/dev/null; then
  problems+=('cannot create a veth pair: NET_ADMIN is missing')
else
  ip link delete ecl-probe-a
fi

if ((${#problems[@]})); then
  printf 'error: the privileged suite cannot run on this host:\n' >&2
  for entry in "${problems[@]}"; do
    printf '  - %s\n' "$entry" >&2
  done
  exit 2
fi

# One test at a time. The suite drives a real daemon and real host resources, so
# two tests running together would compete for the same bridge, the same loop
# devices, and the same cgroup root. A caller that passes its own --test-threads
# keeps it: libtest rejects a second one rather than letting it override, so
# forwarding the arguments as documented would otherwise fail on the duplicate.
serial_args=(--ignored --test-threads=1)
for argument in "$@"; do
  case $argument in
    --test-threads*) serial_args=(--ignored) ;;
  esac
done

suite=${ENCLAVE_SUITE:-integration_suite}
printf 'running the privileged %s\n' "$suite"
# The capability probe above is removed by an EXIT trap, and `exec` would replace
# this shell before that trap could run, leaving the probe directory behind on every
# run. The collector that CI uploads after a failure reports those as leftovers, so
# the leak was visible as noise in the one artifact meant to be evidence. Running the
# test as a child and propagating its status keeps the trap.
status=0
"$cargo_bin" test --test "$suite" -- "${serial_args[@]}" "$@" || status=$?
exit "$status"
