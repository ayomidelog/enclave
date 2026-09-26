# Runtime Details

This page collects the lower-level runtime behavior that is useful once you are past the quickstart: architecture philosophy, networking, performance expectations, and stability guarantees.

## Architecture Philosophy

- **Kernel-first design**: Enclave relies exclusively on Linux kernel primitives (`unshare`, user namespaces, idmapped mounts, OverlayFS, cgroup v2, network namespaces). There is no OCI/container runtime stack and no image format.
- **Reproducibility via rootfs cache**: Sandbox root filesystems are bootstrapped once (via `debootstrap` or a cached base image) and shared read-only across all workspaces. Workspace-specific changes live in OverlayFS upper layers, ensuring the base environment is always reproducible.
- **Single daemon, direct syscalls**: One daemon process manages all sandboxes and workspaces. Workspace sessions are created with direct `unshare` calls and entered through an internal namespace helper — no intermediate container runtime or orchestration layer.
- **Post-bootstrap hardening**: After workspace bootstrap, Enclave remounts `/proc/sys` read-only, attempts to remount `/sys` read-only when the kernel permits it, drops runtime capabilities, and installs a seccomp deny list.
- **Fail-safe cleanup**: All mount, cgroup, and network resources are cleaned up deterministically on workspace stop/destroy, and the cleanup is verified rather than assumed: a stop and a destroy each check the host afterwards and refuse to record success while anything is still held. The daemon reconciles stale state on startup and provides a `doctor` command for manual verification.

## Networking

Each workspace runs in its own network namespace with full port isolation and outbound Internet access via NAT.

### Architecture

```text
┌─────────────┐  ┌─────────────┐
│ workspace A │  │ workspace B │
│ eth0        │  │ eth0        │
│ 10.200.0.10 │  │ 10.200.0.11 │
└──────┬──────┘  └──────┬──────┘
       │ (veth)         │ (veth)
───────┴────────────────┴───────── enclave0 bridge (10.200.0.1/24)
                  │
           NAT masquerade → Internet
```

- **Port isolation**: Workspaces A and B can both bind `0.0.0.0:4000` without conflicts.
- **Outbound access**: `curl google.com` works inside every workspace.
- **Host isolation by default**: Host cannot reach workspace ports unless you explicitly publish them.
- **Loopback-only publishing**: Published ports bind to `127.0.0.1` only in v1, so host access stays local to the machine.
- **Cross-workspace isolation**: direct workspace-to-workspace forwarding is blocked by default on the Enclave bridge.
- **Host-service isolation**: direct access from a workspace to host-local services and the cloud metadata endpoint is blocked by default.
- **Clean teardown**: Stopping/destroying a workspace removes its veth pair and releases its IP.
- **Collision-resistant interface names**: Host-side veth names include the workspace identity, and temporary peer names stay within Linux's 15-character interface-name limit.
- **Compact IPAM**: The daemon reconciles the registry-facing used-IP set into a bounded bitmap for constant-time first-free allocation and restart-safe reconstruction.
- **Runtime metrics**: `daemon.health` includes request, transfer, file, cache,
  namespace-cache, helper-process, mount, unmount, cleanup-retry, and bounded request,
  per-phase, and registry-lock-wait latency histograms; detailed phase logs remain
  opt-in through `ENCLAVE_PERF=1`.
- **Host capabilities**: `enclave doctor` reports every capability the lifecycle
  depends on, with what each missing one costs. The rows are cgroup v2 and the
  controllers it exposes, OverlayFS, idmapped mounts, the ext4 tools a disk quota needs,
  iptables, and user namespaces. Every one is optional: a host without overlay still
  runs, it copies the cached rootfs instead of sharing it. Naming them together is what
  lets an operator see the whole answer at once rather than discovering one missing
  piece per failed start. Each is a property of the running kernel or of the installed
  programs, so the probes cache their answers and none is on a request path.
- **Cleanup retries**: the retryable kernel states a teardown can meet are counted
  as attempts, as the total delay they cost, and as the attempts that ran out without
  recovering. The last errno the kernel gave is reported alongside them, so a busy
  cgroup can be told apart from one that had already gone. Only the paths that read an
  errno report one: a retry decided from the text of an `ip` failure leaves it at zero
  rather than claiming a value the kernel never returned.
- **Registry lock budget**: the registry lock is held only for the mutations
  themselves, never across the host work a lifecycle operation does, so the wait on
  it is the latency one request adds to every other one. `daemon.health`
  reports that wait as a p99 with a documented budget of 50 ms and a boolean saying
  whether the observed p99 is inside it. A p99 above the budget means a holder is
  doing something inside the lock that belongs outside it. Measure it with
  `./tools/perf/bench.sh lock-wait`.

### Additional network guards

- IPv6 is disabled on the Enclave bridge and workspace veth interfaces instead of relying on a parallel IPv6 firewall policy.
- Per-veth anti-spoofing rules drop packets whose source IP does not match the workspace's assigned IPv4 address.

### IP allocation

- `10.200.0.1` — bridge gateway (`enclave0`)
- `10.200.0.2`–`.9` — reserved
- `10.200.0.10`–`.254` — workspace pool (auto-allocated, persisted in registry)

An address is allocated against the union of two things: the registry's own
assigned addresses, and the octets carried by every Enclave interface already on
the host. The second is what keeps a second daemon with its own state directory
from handing out an address the first daemon's workspace holds — the address lives
inside a network namespace and the other daemon's registry is not this one's to
read, but a host veth is named for the octet it carries (`veth-<octet>-<hash>`), so
the host itself records which ones are taken. See [limitations.md](limitations.md)
for what two daemons on one host still share.

### DNS

A managed `/etc/resolv.conf` is provisioned in each workspace rootfs. If the host resolver is a systemd-resolved stub (`127.0.0.53`), Enclave reads the upstream resolver list from `/run/systemd/resolve/resolv.conf` instead, ensuring name resolution works without the stub dependency.

## Performance

Rough ballpark on a 4-core x86_64 host (NVMe):

| Metric | Enclave | Docker (alpine) |
|---|---|---|
| Sandbox create (debootstrap) | ~30–60 s | N/A (image pull) |
| Sandbox create (cached_rootfs) | ~2–10 s | N/A (image pull) |
| Workspace start (namespace + overlay) | ~200–400 ms | ~300–500 ms (`docker run`) |
| Workspace enter (nsenter) | ~50 ms | ~50 ms (`docker exec`) |
| Rootfs size (minimal bookworm) | ~300 MB (shared) | ~5 MB per alpine layer |
| Memory overhead per workspace | ~2–4 MB | ~6–10 MB |

Numbers vary by host hardware, suite, and setup commands. The key tradeoff: one shared rootfs means the bootstrap cost is paid once regardless of workspace count.

### Where a workspace start spends its time

The ballpark above is a total. What is worth knowing before optimizing anything is
the share each phase has of it, because two phases that each cost a fifth are worth
different work than one that costs two fifths. `tools/perf/phases.sh` reads that
breakdown out of a daemon log written with `ENCLAVE_PERF=1`.

Medians of eight steady-state starts of one workspace, release binary, validation
host (AMD EPYC, kernel 6.8.0), `ENCLAVE_PERF=1`, one up worker:

| phase | median | share of the request |
|---|---|---|
| `workspace.start.network` | 51 ms | 39% |
| `workspace.start.session` | 32 ms | 25% |
| `workspace.start.intent` | 15 ms | 11% |
| `workspace.start.commit` | 15 ms | 12% |
| `workspace.start.cgroup` | 9 ms | 7% |
| `workspace.start.commit_registry` | 7 ms | 5% |
| `workspace.start.auth` | 0.2 ms | 0.1% |
| `workspace.start.storage` | 0.01 ms | 0.0% |
| `daemon.request` (the total) | 130 ms | 100% |

Two things are worth reading from that table rather than from the totals. Storage
is effectively free on the default tier, so a storage optimization on a
directory-backed workspace buys nothing; the quota tier is where storage work
appears, and `docs/storage.md` has that breakdown. The network phase is the
largest, and almost all of it is kernel work rather than process spawns: the two
`ip` invocations cost about 4.5 ms each, against 24 ms for the veth pair and 26 ms
for the anti-spoofing rules the kernel and `iptables` do. That is why
`src/network/veth.rs` documents the decision not to replace `ip` with rtnetlink:
it would remove about 10 ms of a 130 ms start and none of the kernel work.

### Why the session is several processes

The session phase is the second largest, and the plan proposes evaluating a
persistent runtime manager with prewarmed namespace templates against it. Measured,
the phase does not support that work.

Of the 32 ms median, 4 ms is the `setsid` fork and exec, 26 ms is the readiness
wait, and under 0.1 ms is staging the helper binary. The readiness wait is not
spinning: it is an inotify watch on the runtime directory, so it costs one poll
descriptor and no wakeups, and it ends the moment the session writes its ready
file. The time inside it is the session doing its own work in its own namespaces:
`session.init.namespace_refs`, the mounts, the pivot, and the hardening. A pooled
or prewarmed runtime would have to be reset to a known state before it could be
handed to a workspace, and that reset is the same mounts and the same pivot, so the
only part a pool could remove is the 4 ms fork and exec. Against that stands a
pool that holds a live namespace per idle workspace, which is memory and cgroup
state the host pays for whether or not anyone starts a workspace, and a reset path
that has to be proven as strong as a fresh launch before it can be trusted to hand
out isolation. That is a large amount of code and a new class of risk for about 3%
of a start.

What the phase does have is a fixed 5 s readiness deadline
(`ENCLAVE_SESSION_READY_MS`) that a healthy start never approaches, and a load
failure recognized from the session log rather than waited out. Both are the
properties that matter here: a start that cannot succeed fails with the reason
rather than after the deadline.

### Lifecycle and shutdown controls

| Variable | Default | Effect |
|---|---|---|
| `ENCLAVE_PERF` | unset | Emit phase timings to stderr and the log, and enable the health counters. |
| `ENCLAVE_LOG` | info | Tracing filter for the daemon and CLI. |
| `ENCLAVE_UP_WORKERS` | available parallelism | Concurrency for a batch workspace start, capped at the number of workspaces. |
| `ENCLAVE_CLEANUP_WORKERS` | available parallelism, capped at 4 | Concurrency for a batch workspace teardown. |
| `ENCLAVE_CONTROL_WORKERS` | 6 | Daemon workers serving lifecycle and read requests. |
| `ENCLAVE_TRANSFER_WORKERS` | 2 | Daemon workers serving transfers. |
| `ENCLAVE_SHUTDOWN_GRACE_SECS` | 30 | How long shutdown waits for running lifecycle operations before reporting them incomplete. Accepted in the range 1 to 600. |
| `ENCLAVE_HOST_COMMAND_TIMEOUT_SECS` | 30 | Deadline for a host command that does not set its own. Accepted in the range 1 to 3600. |
| `ENCLAVE_SESSION_READY_MS` | 5000 | How long a launched runtime gets to publish its pid and ready files. Accepted in the range 1 to 120000. |
| `ENCLAVE_RUNTIME_EXEC_SETTLE_MS` | 500 | How long a stop waits for the command line of a pid it was told about to name a runtime. Accepted in the range 1 to 30000. |
| `ENCLAVE_RUNTIME_TERM_GRACE_MS` | 500 | How long a runtime gets to exit after `SIGTERM` before it is killed. Accepted in the range 1 to 30000. |
| `ENCLAVE_RUNTIME_KILL_GRACE_MS` | 500 | How long a killed runtime gets to disappear before the stop reports failure. Accepted in the range 1 to 30000. |
| `ENCLAVE_LOOP_DETACH_MS` | 2000 | How long a workspace image's loop device gets to detach after its mount is released. Accepted in the range 1 to 60000. |
| `ENCLAVE_HELPER_START_MS` | 5000 | How long the persistent command helper gets to accept a connection. Accepted in the range 1 to 60000. |

Every lifecycle wait is bounded and each bound has a hard ceiling, so a value
past the ceiling is clamped rather than honoured. A value that does not parse,
or that is zero, is ignored with a warning and the default is used. Values are
read when they are used, so an override applies to the next operation without a
daemon restart, and `daemon.health` reports the whole table with each value's
default, ceiling, and whether an operator set it. `enclave daemon status` prints
the overrides, which is what makes a start that failed on a short deadline
distinguishable from a start that failed on its own.

The default that matters most on a loaded host is `ENCLAVE_SESSION_READY_MS`. A
batch start runs several workspaces at once and each one's setup is CPU bound, so
on a small or busy host the last workspace in the batch can wait several times
longer than the first. On a 4-CPU host with a 5-second default, 16 workspaces
start in under 3 seconds, while 32 workspaces exceed the default and report
`workspace session did not become ready`. The failure is a deadline, not a broken
runtime, and the message now names the deadline with its variable and ceiling so
that is visible from the error. Raise `ENCLAVE_SESSION_READY_MS` when starting
many workspaces at once, or lower `ENCLAVE_UP_WORKERS` so fewer start in parallel.

The deadline that exists for a moment rather than for a slow host is
`ENCLAVE_RUNTIME_EXEC_SETTLE_MS`. A stop refuses to signal a pid whose command
line does not name an Enclave runtime, and a runtime carries its launcher's
command line from the fork that created it until the exec that makes it the
runtime has finished — during that exec the command line reads as empty. Reading
that moment as a stale record cleared the record without signalling anything and
left the runtime holding its cgroup, its interface, and its mounts with nothing
left to name it. The stop now waits this long for the command line to settle
before it gives up on the record. A runtime that is already recognizable costs
nothing, so only a stop of a record that names something which is not a runtime
at all pays the wait.

Deadlines for jobs that are minutes long and have their own supervision —
`debootstrap`, rootfs copies, snapshot archives — are not in this table. They are
bounded where the job is defined.

Shutdown stops accepting requests, waits up to the grace period for the lifecycle
operations already running, and then logs each one it left behind with its
operation id, action, target, and elapsed time. Their journals survive, so the
next daemon start finishes or rolls back the interrupted work.

The repository includes a bounded live lifecycle benchmark:

```bash
ENCLAVE_UP_WORKERS=1 ENCLAVE_CLEANUP_WORKERS=4 \
  ./tools/perf/live-lifecycle.sh
```

It reuses a local cached rootfs, applies an eight-workspace memory/CPU/process
budget, and excludes rootfs preparation from lifecycle timings.

On the current bounded eight-workspace cached-rootfs validation, the medians of
seven consecutive runs at a one-minute load average of 3.0 on four CPUs are:

- cold workspace boot: `2.14s` (2.14–4.13s)
- cold shutdown: `1.52s` (0.70–1.52s)
- warm workspace boot: `1.95s` (1.63–2.16s)
- warm shutdown: `0.75s` (0.70–0.93s)

The validation host is shared with other work, which is what widens the ranges;
the warm-boot outlier above was measured while the host was busier than the rest of
the run. The median is the number to compare and the range is what a shared host
does to it. These measurements are host-dependent, so use the repository benchmark
to compare changes on the same machine rather than across machines, and compare the
host metadata in `docs/lifecycle-report.md` before comparing two releases.

## Stability Guarantees

- **Crash recovery**: On daemon startup, Enclave reconciles workspace state against the process table. Any workspace marked as `Running` whose session PID no longer exists (or whose start-time ticks do not match) is automatically transitioned to `Stopped`. A workspace left in a transitional state by an interrupted operation is resolved deterministically instead of being guessed at: an interrupted start is rolled back and an interrupted stop is completed, and neither is resumed. The exception is a start whose own record was already written when the daemon died. A launch writes the workspace's status, runtime PID, start time, and namespace references to that record before it commits the registry, so a crash in between leaves the on-disk copy saying `running`; the next daemon adopts it and the start completes, because the record names the runtime and proves it rather than guessing. This handles daemon crashes, host reboots, and OOM-killed sessions without manual cleanup.
- **Interrupted launches**: A workspace records its runtime PID only when a launch commits, so a session that is still starting is not yet named by any record. A stop that arrives in that window, and a launch that fails in it, both find that session by the pid file it wrote or by its own command line and end it, so a failed start cannot leave a live runtime holding namespaces, mounts, and a private `/tmp` that nothing on the host describes.
- **Cleanup certificates**: A stop captures the resources the workspace owns, releases them, and then verifies the host — runtime PID and start time, cgroup, mounts, loop device, veth interface, firewall rules, namespace reference files, and files — before it records `stopped`. A destroy runs the same check before removing the workspace directory. A resource that cannot be released keeps the record transitional and names what was retained, which is what makes an interrupted teardown a recovery point rather than a silent leak.
- **Lifecycle journal**: Every lifecycle operation runs under one operation id and writes a durable record with its target and current phase under the state directory's `operations/` directory. The id is returned to the caller and printed by mutating commands, so a command, its journal record, its log lines, and its phase timings can be tied together. A daemon start closes the records a previous daemon left open.
- **cgroup fallback**: When cgroup v2 is not available, Enclave falls back to rlimit-only resource enforcement and logs a warning. Workspace isolation remains intact — only hard memory/PID limits are downgraded to soft rlimits.
- **Process termination**: Workspace runtimes use dedicated cgroups when cgroup v2 is available. Shutdown allows a short graceful interval, then uses `cgroup.kill` with identity-checked signal fallback so detached workspace commands cannot survive normal cleanup.
- **Detached run commands**: Enclavefile `run` commands launch asynchronously inside the workspace runtime cgroup. `enclave up` reports launch failures but does not wait for long-running services to exit.
- **Mount cleanup**: Cleanup loads one mountinfo snapshot per cleanup transaction, unmounts nested workspace mounts deepest-first, and retries with a lazy unmount when the runtime owner is gone; failed resources report the mount target, errno, and namespace holders while independent cleanup continues.
- **Overlay guarantees**: The shared rootfs is bind-mounted read-only under OverlayFS. Workspace writes go to the upper layer only. Stopping or destroying a workspace removes only the workspace-specific overlay data — the shared rootfs is never modified.
- **Workspace source mounts**: `/home` is presented through an idmapped bind mount, whether the source is the Enclave-managed workspace directory or an explicit host `workspace_dir`.
- **System diagnostics**: Run `enclave doctor` to verify mount state, cgroup state, registry consistency, runtime process health, and published host ports. Published ports are held by daemon threads rather than by anything on disk, so the check runs in the daemon and reports a listener whose workspace is no longer running, no longer declares the port, or is not registered at all. Run `enclave doctor --repair` to reconcile stale registry entries, files, mounts, namespace references, and workspace artifacts.
