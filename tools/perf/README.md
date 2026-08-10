# Enclave performance harness

The harness is intentionally outside the normal test path. It measures the
user-visible CLI and records the host metadata needed to compare runs.

```bash
./tools/perf/bench.sh --help
./tools/perf/bench.sh ping --iterations 30
./tools/perf/bench.sh health --iterations 30
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

Generate deterministic transfer fixtures with:

```bash
./tools/perf/fixtures.sh /tmp/enclave-fixtures
```

The generator creates 4 KiB and 1 MiB files, sparse 1 GiB and 5 GiB files,
100,000 small files, and a ten-level directory tree. Set
`ENCLAVE_PERF_MANY_FILES=1000` for a shorter smoke fixture. The fixture
directory is never inferred from the current working directory.
