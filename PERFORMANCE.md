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
utility discovery, pidfd-aware transfer cancellation, kernel-assisted regular
file streaming, streamed host-to-workspace directory transfers, explicit
timestamp preservation, enlarged transfer pipes, reflink-aware storage copies,
bounded snapshot GC, collision-resistant veth naming, safe quota resize checks,
and phase timers. The privileged
measurements below use the same host and harness as the baseline.

| Workload | Result | Evidence |
| --- | ---: | --- |
| Registry cache correctness | pass | `registry_cache_refreshes_after_external_atomic_replace` |
| Rootfs cache index | pass | Indexed required-directory fingerprints plus suite/source/architecture/time/tool/digest metadata with invalidation tests in `sandbox::cache` |
| Registry repeated read | `1.78x` faster (`0.020666869s` -> `0.011633155s`) | `tools/perf/bench.sh registry` |
| Single-file archive creation | `36.14x` faster (`1.352945186s` -> `0.037432050s`) | 10 x 16 MiB fixture, `tools/perf/check-thresholds.sh`, August 10, 2026 release-candidate run |
| Many-file archive creation | `0.71x` relative to external tar (`17.856997685s` -> `25.299986690s`) | 100,000 x 4 KiB fixture, `ENCLAVE_PERF_MANY_FILES=100000 tools/perf/bench.sh many-files`; host-to-workspace uses streamed host tar while workspace-to-host remains Rust-validated extraction |
| Daemon worker isolation | compile- and test-validated | `cargo test --all-targets --no-run` |
| Daemon scheduling | separate bounded control/transfer queues | 6 control workers, 2 transfer workers, classification regression tests |
| Worker configuration | bounded benchmark controls | `ENCLAVE_CONTROL_WORKERS` and `ENCLAVE_TRANSFER_WORKERS`, each limited to 1–64 |
| IPAM allocation | bounded bitmap scan | Pool entries are represented in four fixed words and reconstructed from reconciled registry state |
| Runtime observability | bounded request and per-phase latency histograms plus lifecycle counters | `daemon.health` exposes named phase buckets, lock wait, mount/unmount, cleanup retry, transfer-file, cache, and process-spawn counters |
| Cleanup mount parsing | one mountinfo snapshot per cleanup transaction | `MountInfoSnapshot` reverse-depth planning tests |
| Namespace handoff | identity-checked descriptor reuse | runtime PID/start-time and five namespace identities key the daemon cache; helper descriptors are inherited without reopening `/proc/<pid>/ns/*` |
| Kernel transfer path | `splice` attempted before `sendfile` for regular host files | short-write, EINTR, EOF, and unsupported-kernel handling covered by the direct transfer path |
| Daemon ping, 10 CLI invocations | p50 `0.07s`, p95 `0.16s` | Current branch, `tools/perf/bench.sh ping --iterations 10` |
| Daemon health, 8 CLI invocations | p50 `0.06s`, p95 `0.14s` | Current branch, `tools/perf/bench.sh health --iterations 8`, August 10, 2026 |
| Concurrent daemon control requests | 64 requests in `0.901267s` | 16 clients, 6 control workers plus 2 transfer workers, `tools/perf/bench.sh stress --iterations 64`, August 10, 2026 |
| Workspace readiness | event-driven wait | `inotify` + bounded timeout; covered by the privileged lifecycle fixture |
| Privileged workspace cp regression fixture | 14.00 s | Files, directories, metadata preservation, symlink rejection, and both directions passed |
| Privileged integration suite | 12/12 passed in `59.80s` | Auth, lifecycle, snapshot, quota, resize, tmp isolation, workspace copy, and host-mount fixtures; August 10, 2026 |
| Regular-file transfer path | kernel direct stream | `sendfile` into namespace-local receiver; privileged fixture passed |
| Workspace cp, 5 GiB | `59.004412s` (`86.773 MiB/s`) | Privileged namespace fixture with sparse 5 GiB source, `ENCLAVE_PERF_5G=1`, August 10, 2026 |
| Deterministic transfer fixtures | pass | `tools/perf/fixtures.sh`: 4 KiB, 1 MiB, sparse 1 GiB/5 GiB, 100,000 files by default, ten-level tree |
| Copy progress reporting | opt-in, stderr-only | `enclave workspace cp ... --progress`; no progress text enters stdout or JSON protocol |
| Directory transfer compression | opt-in gzip stream | `enclave workspace cp ... --gzip`; gzip is applied only to directory tar streams and tar-flag coverage is unit-tested |
| Persistent workspace exec | one helper per live runtime | Repeated daemon-managed commands reuse one identity-checked helper, inherited namespace descriptors, and a pidfd; output draining is poll-based and capped at 16 MiB per stream |
| Workspace startup fan-out | bounded client concurrency | `ENCLAVE_UP_WORKERS` defaults to 4 and parallelizes independent workspace requests while preserving definition-order errors and run-command order |
| Workspace cleanup fan-out | bounded daemon concurrency | `ENCLAVE_CLEANUP_WORKERS` defaults to 4 and replaces one unbounded cleanup thread per workspace |
| Workspace observation | bounded collection workers | `ENCLAVE_STATS_WORKERS` and `ENCLAVE_PS_WORKERS` default to 4 and collect live runtime data from one registry snapshot |
| Concurrent workspace preparation | reduced registry lock scope | Filesystem and storage preparation occurs before the final registry commit, allowing independent creates to progress without holding the global registry lock |
| Batch workspace wipe | one daemon plan and bounded cleanup | `workspace.wipe` snapshots all workspace records once, cleans independent resources with `ENCLAVE_CLEANUP_WORKERS`, and deletes each registry record immediately after confirmed cleanup |
| Registry generation tracking | monotonic stale-commit guard | Durable registry mutations advance a generation counter; lifecycle operations revalidate resource identity before committing external-work results |
| CLI phase timing | opt-in diagnostics | Global `--verbose` emits CLI total/config, socket connect, request write, response read, daemon request/dispatch, and named lifecycle phase timings to stderr |
| Syscall profiling harness | pass | `tools/perf/trace.sh` wraps `strace -f -c` without making tracing a runtime dependency |

## Interpretation

The build time is compilation time, not application runtime, and is not treated as
an optimization success metric. Ping remains within subprocess and host noise in
this ten-sample run; the measured speedup claim is limited to registry reads.
The 5 GiB transfer and workspace lifecycle rows now have measurements from the
privileged namespace fixture. The release-candidate privileged suite for 1.0.6
passed all 12 tests in `51.13s` after adding the persistent helper and bounded
fan-out paths. The many-file workspace-to-host direction remains
slower than external tar because it retains Rust-side archive validation and
staging; that safety boundary is intentional and is the next focused tuning
target. Correctness and safety gates remain higher priority than a favorable
number.
