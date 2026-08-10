# Enclave performance report

## Measurement policy

Numbers are recorded from the same optimized build, host, kernel, filesystem,
and fixture. Latencies use monotonic wall-clock samples; transfer results include
all validation, staging, namespace, and commit work. Privileged namespace tests
are marked `SKIP` when the benchmark host cannot execute them.

## Baseline

Captured before the first performance implementation on August 9, 2026.

| Workload | Result | Evidence |
| --- | ---: | --- |
| Release build | 98.30 s | `/usr/bin/time cargo build --release` |
| Daemon ping, 10 CLI invocations | p50 `0.05s`, p95 `0.12s` | Untouched `HEAD` worktree, `tools/perf/bench.sh ping --iterations 10` |
| Workspace start | unavailable | Runtime requires root on this host |
| Workspace exec | unavailable | Runtime requires root on this host |
| Workspace cp, 5 GiB | unavailable | Runtime requires root and a prepared workspace |

## After implemented phases

The implemented phases add an identity-checked registry read cache, compact
registry writes, bounded daemon workers, nonblocking daemon shutdown,
event-driven workspace readiness, direct mountinfo checks, cached workspace
utility discovery, pidfd-aware transfer cancellation, a Rust archive stream for
regular files and directories, explicit timestamp preservation, enlarged transfer
pipes, reflink-aware storage copies, bounded snapshot GC, collision-resistant
veth naming, safe quota resize checks, and phase timers. The privileged
measurements below use the same host and harness as the baseline.

| Workload | Result | Evidence |
| --- | ---: | --- |
| Registry cache correctness | pass | `registry_cache_refreshes_after_external_atomic_replace` |
| Rootfs cache index | pass | Indexed required-directory fingerprints with invalidation tests in `sandbox::cache` |
| Registry repeated read | `1.78x` faster (`0.020666869s` -> `0.011633155s`) | `tools/perf/bench.sh registry` |
| Single-file archive creation | `22.04x` faster (`1.892066383s` -> `0.085836286s`) | 10 x 16 MiB fixture, `tools/perf/check-thresholds.sh`, August 10, 2026 |
| Many-file archive creation | `0.74x` relative to external tar (`0.256312628s` -> `0.345617069s`) | 1000 x 4 KiB fixture, `tools/perf/bench.sh many-files`; host-to-workspace directory transfers now use the kernel-streamed host tar path, while workspace-to-host remains Rust-validated extraction |
| Daemon worker isolation | compile- and test-validated | `cargo test --all-targets --no-run` |
| Daemon scheduling | separate bounded control/transfer queues | 6 control workers, 2 transfer workers, classification regression tests |
| Runtime observability | bounded latency histograms and lifecycle counters | `daemon.health` exposes request/phase buckets, lock wait, mount/unmount, cleanup retry, transfer-file, cache, and process-spawn counters |
| Cleanup mount parsing | one mountinfo snapshot per cleanup transaction | `MountInfoSnapshot` reverse-depth planning tests |
| Namespace handoff | identity-checked descriptor reuse | runtime PID/start-time and five namespace identities key the daemon cache; helper descriptors are inherited without reopening `/proc/<pid>/ns/*` |
| Kernel transfer path | `splice` attempted before `sendfile` for regular host files | short-write, EINTR, EOF, and unsupported-kernel handling covered by the direct transfer path |
| Daemon ping, 10 CLI invocations | p50 `0.07s`, p95 `0.16s` | Current branch, `tools/perf/bench.sh ping --iterations 10` |
| Daemon health, 8 CLI invocations | p50 `0.06s`, p95 `0.14s` | Current branch, `tools/perf/bench.sh health --iterations 8`, August 10, 2026 |
| Concurrent daemon control requests | 64 requests in `1.119924s` | 16 clients, 6 control workers plus 2 transfer workers, `tools/perf/bench.sh stress --iterations 64` |
| Workspace readiness | event-driven wait | `inotify` + bounded timeout; privileged start fixture pending |
| Privileged workspace cp regression fixture | 14.00 s | Files, directories, metadata preservation, symlink rejection, and both directions passed |
| Regular-file transfer path | kernel direct stream | `sendfile` into namespace-local receiver; privileged fixture passed |
| Workspace cp, 5 GiB | `59.004412s` (`86.773 MiB/s`) | Privileged namespace fixture with sparse 5 GiB source, `ENCLAVE_PERF_5G=1`, August 10, 2026 |
| Deterministic transfer fixtures | pass | `tools/perf/fixtures.sh`: 4 KiB, 1 MiB, sparse 1 GiB/5 GiB, 100,000 files by default, ten-level tree |
| Copy progress reporting | opt-in, stderr-only | `enclave workspace cp ... --progress`; no progress text enters stdout or JSON protocol |

## Interpretation

The build time is compilation time, not application runtime, and is not treated as
an optimization success metric. Ping remains within subprocess and host noise in
this ten-sample run; the measured speedup claim is limited to registry reads.
The 5 GiB transfer and workspace lifecycle rows remain open until a prepared
namespace fixture is available. Correctness and safety gates remain higher
priority than a favorable number.
