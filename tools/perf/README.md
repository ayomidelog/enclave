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

That prints a median, p95, and maximum for the warm tier alongside every raw
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
restart. It also checks that the runtime pid is the same on both sides.

The three suites are separate files because each measures a different lifecycle
tier on a different code path. Publishing one number for all of them would invite
reading a change in one as a change in another.

Generate deterministic transfer fixtures with:

```bash
./tools/perf/fixtures.sh /tmp/enclave-fixtures
```

The generator creates 4 KiB and 1 MiB files, sparse 1 GiB and 5 GiB files,
100,000 small files, and a ten-level directory tree. Set
`ENCLAVE_PERF_MANY_FILES=1000` for a shorter smoke fixture. The fixture
directory is never inferred from the current working directory.
