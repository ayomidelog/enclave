# Enclave performance harness

The harness is intentionally outside the normal test path. It measures the
user-visible CLI and records the host metadata needed to compare runs.

```bash
./tools/perf/bench.sh --help
./tools/perf/bench.sh ping --iterations 30
./tools/perf/bench.sh registry --iterations 1000
./tools/perf/bench.sh cp --sandbox mybox --workspace agent1 \
  --src /path/to/5GiB.bin --dst ws:/home/5GiB.bin --iterations 3
```

Commands that require namespaces or root privileges report `SKIP` instead of
silently producing an invalid number. Use `strace=1`, `perf=1`, or `pidstat=1`
to enable optional tracing for a single command.
