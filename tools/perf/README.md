# Enclave performance harness

The harness is intentionally outside the normal test path. It measures the
user-visible CLI and records the host metadata needed to compare runs.

```bash
./tools/perf/bench.sh --help
./tools/perf/bench.sh ping --iterations 30
./tools/perf/bench.sh health --iterations 30
./tools/perf/bench.sh list --iterations 30
./tools/perf/bench.sh stats --iterations 30
./tools/perf/bench.sh ps --iterations 30
./tools/perf/bench.sh doctor --iterations 10
./tools/perf/bench.sh workspace-list --iterations 30
./tools/perf/bench.sh registry --iterations 1000
./tools/perf/bench.sh many-files

Set `ENCLAVE_PERF_MANY_FILES=100000` before the benchmark for the full
metadata-heavy fixture; the default 1000-file run is intended as a quick smoke
test.

Capture syscall counts for a focused command with:

```bash
./tools/perf/trace.sh target/release/enclave --help
```

Pass `--verbose` to a daemon-backed command to emit the CLI and daemon phase
timings to stderr without changing normal output contracts.

Use `enclave up --cache-setup` or `enclave restart --cache-setup` only when setup
commands are known to be safe to skip after an unchanged Enclavefile digest.
./tools/perf/bench.sh cp --sandbox mybox --workspace agent1 \
  --src /path/to/5GiB.bin --dst ws:/home/5GiB.bin --iterations 3

After generating fixtures, `--fixture-dir /tmp/enclave-fixtures` defaults the
copy benchmark to the sparse 5 GiB host-to-workspace workload unless `--src`
and `--dst` are supplied explicitly.

Add `--progress` to `enclave workspace cp` for opt-in stderr progress updates;
add `--gzip` for directory archive compression when CPU-for-bandwidth trade-offs
are favorable;
the JSON control response and normal stdout remain unchanged.
```

Commands that require namespaces or root privileges report `SKIP` instead of
silently producing an invalid number. Use `strace=1`, `perf=1`, or `pidstat=1`
to enable optional tracing for a single command.

Run the bounded eight-workspace lifecycle benchmark with an already-prepared
cached rootfs:

```bash
ENCLAVE_UP_WORKERS=1 ENCLAVE_CLEANUP_WORKERS=4 \
  ./tools/perf/live-lifecycle.sh
```

The benchmark copies the local cached rootfs into temporary state and never
downloads bootstrap packages. Set `ENCLAVE_LIVE_ROOTFS` to use another cache.

Each cycle is an `up` followed by a `down`. The first cycle is the cold tier and
every cycle after it is the warm tier, so the default of two cycles is the
published cold and warm pair. Set `ENCLAVE_LIVE_ITERATIONS` higher to publish a
distribution:

```bash
ENCLAVE_LIVE_ITERATIONS=20 ./tools/perf/live-lifecycle.sh
```

That prints a median, p95, p99, and maximum for the warm tier alongside every raw
sample. The samples are printed so a saved run can be re-analyzed rather than only
read; a percentile of a run that was not kept cannot be checked later. The raw
samples are also what a regression gate should compare, because a single sample
cannot show a tail and the tail is where a lifecycle regression appears first.

Run the quota-backed lifecycle benchmark, which measures the tier whose storage
is an ext4 image on a loop device rather than a directory:

```bash
./tools/perf/live-quota.sh
```

Run the pause/resume lifecycle benchmark, which measures the tier that keeps the
runtime alive rather than releasing it:

```bash
./tools/perf/live-pause.sh
```

It reports the pause and resume times and proves the tier's two claims: the
counter a workspace is incrementing does not move while the sandbox is paused,
and it continues afterwards, which is what makes the run a resume rather than a
restart. It also checks that the runtime pid is the same on both sides and that the
sandbox cgroup is actually frozen, because a pause of a sandbox with nothing to
freeze is a no-op and the counter alone would not tell the two apart.

It then drives the two lifecycle operations that follow a pause, which take a
different path from the ones that follow a normal stop: a signal sent to a runtime
inside a frozen cgroup is not delivered until the cgroup thaws, so the daemon has to
thaw before it stops. The run pauses, stops, and requires the sandbox cgroup to be
gone and the workspace files to still be there; it starts the same sandbox again to
show the stop was a stop rather than a destroy; and it pauses again and destroys,
requiring the cgroup and the sandbox directory to be gone.

The three suites are separate files because each measures a different lifecycle
tier on a different code path. Publishing one number for all of them would invite
reading a change in one as a change in another.

Run the loaded lifecycle check, which drives twelve workspaces whose run commands
are CPU loops, all live at once, inside a sandbox capped at a quarter of the host:

```bash
./tools/perf/live-loaded.sh
```

The eight-workspace benchmark measures a sandbox whose workspaces are idle, so its
number describes the lifecycle request and nothing else. This one measures the same
lifecycle while every workspace is competing for the sandbox CPU, which is where a
tail in a start appears and where a control request has to keep answering. It
publishes the boot, the shutdown, and the stop and start of one workspace taken
while the other eleven keep running.

It is also a correctness check, and the assertions are made while the load is on:
each workspace is entered and its /tmp is required to be a linked, writable
directory of its own, which is the state whose absence made every write inside a
workspace fail; the two quota-backed workspaces in the fixture are required to
mount /tmp from their own workspace tmp directory inside their image rather than
from a fresh tmpfs, so the two storage tiers are told apart; and the teardown is
required to leave no runtime, no cgroup, no interface, and no bound port behind,
which is a stronger claim with twelve runtimes to release than with one.

Two numbers make the load a measured quantity rather than an assumption. The
sandbox cgroup reports how much CPU it actually spent and how often it was
throttled, and the run refuses to pass if the sandbox was never throttled, which
would mean the workspaces were not asking for more than they were allowed. The
whole host reports its busy fraction over the loaded window, so the demand this
run put on the machine is published next to the result.

The demand is larger than the cap on purpose, which is what makes the run safe to
take on a host that is doing other work: the workspaces asking for 4.8 CPUs are
refused rather than served, and the sandbox stays at the one CPU it was given.
Point ENCLAVE_LOADED_ENCLAVEFILE at another fixture on a host with more cores.

Run the published-port lifecycle check, which drives a workspace with three declared
ports through start, pause, resume, stop, and destroy and asserts on whether each host
port can be bound at every step:

```bash
./tools/perf/live-ports.sh
```

It is a correctness check rather than a measurement, and it is a live run for the same
reason the port publisher belongs to the daemon: the workspace layer never binds a
listener, so the daemon has to be in the loop for the ports to exist at all.

It also drives the rollback a failed publication owes the caller. A start whose host
port is already held by something else must undo the start rather than leave a
running workspace with no listeners, so the run holds one of the declared ports,
requires the start to fail, and then requires the workspace to be stopped and the
kernel to hold none of the three ports. Two of the checks are on kernel state rather
than on the registry record the request itself wrote: a workspace cgroup cannot exist
without a live process in it, and a veth cannot exist without the start having made
it. Both are also required to have been seen to exist while the failing start ran, so
a start that failed before reaching either of them cannot pass the checks that they
were released. The port is then released and the same start is required to succeed,
which is what keeps the assertions from passing on a workspace that can no longer
start at all.

To see where a lifecycle request spends its time rather than only how long it took,
run the daemon with `ENCLAVE_PERF=1` and read its log:

```bash
ENCLAVE_PERF=1 enclave daemon start ...
tools/perf/phases.sh "$daemon_log" workspace.start
```

It reports the request p50, p95, and p99, and each phase median with its share of the
request, ordered by the share. The share is the part a total cannot give: two phases
that each cost a third of a request are worth different work than one that costs two
thirds. The optional argument limits the report to the phases of one operation.

Generate deterministic transfer fixtures with:

```bash
./tools/perf/fixtures.sh /tmp/enclave-fixtures
```

The generator creates 4 KiB and 1 MiB files, sparse 1 GiB and 5 GiB files,
100,000 small files, and a ten-level directory tree. Set
`ENCLAVE_PERF_MANY_FILES=1000` for a shorter smoke fixture. The fixture
directory is never inferred from the current working directory.
