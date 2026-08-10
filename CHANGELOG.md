# Changelog

## Unreleased

No unreleased changes.

## 1.0.7 - 2026-08-10

### Added
- Explicit `--cache-setup` opt-in for `enclave up` and `enclave restart`. Successful setup commands are recorded independently using a digest of the sandbox identity, bootstrap method, suite, and ordered command list.
- Additional repeatable performance-harness workloads for sandbox and workspace listing, workspace statistics, process status, and diagnostics, plus opt-in daemon phase timing through global `--verbose`.

### Changed
- Sandbox rootfs bind mounting now uses direct `mount(2)` calls with the existing validation and propagation behavior, avoiding repeated mount-utility process launches on the common workspace lifecycle path.
- Workspace lifecycle operations use bounded workers and identity-checked registry commits so independent setup, startup, cleanup, statistics, and status work can progress without holding the global registry lock during slow filesystem, namespace, network, or process operations.
- `workspace wipe` plans cleanup from one registry snapshot, processes independent work concurrently, and commits each confirmed deletion individually.
- Shared bridge and NAT initialization is serialized separately from workspace-specific networking, preventing duplicate host setup without unnecessarily serializing workspace starts.
- Host-to-workspace directory copies validate archive entries while producing the Rust-controlled tar stream in a single traversal, eliminating the host `tar` subprocess and a second metadata walk.
- Generated `SPEED.md` and `PERFORMANCE.md` benchmark reports are now ignored; repeatable commands and operational guidance live in `tools/perf/README.md`.

### Fixed
- Setup-cache filesystem policy is isolated behind validated marker-path and atomic-write helpers, preventing invalid digests or command indexes from escaping the sandbox cache root.
- The lifecycle integration benchmark reports warm workspace-start and persistent-exec timings without changing lifecycle assertions or normal command output contracts.

## 1.0.6 - 2026-08-10

### Added
- Persistent workspace session helpers for daemon-managed `workspace exec` requests, reusing validated namespace descriptors and runtime identity checks across repeated commands.
- Optional `enclave workspace cp --gzip` compression for directory transfers in either direction.
- Bounded worker pools for Enclavefile workspace startup, workspace cleanup, workspace statistics, and process-status collection.

### Changed
- `workspace wipe` now uses one daemon-side batch plan with bounded cleanup workers and per-resource registry deletion after confirmed cleanup.
- Registry generations advance on durable mutations, and workspace start/stop operations release the registry lock during namespace, storage, network, and process work before committing identity-checked results.
- Snapshot creation releases the registry lock before mounting storage and copying workspace data, preventing multi-gigabyte snapshots from blocking unrelated metadata requests.
- Concurrent workspace starts now serialize only shared bridge/NAT initialization, preventing duplicate host-network setup while preserving parallel per-workspace networking.
- `enclave up --cache-setup` and `enclave restart --cache-setup` add an explicit digest-keyed setup cache; successful commands are marked individually and default setup behavior remains uncached.
- Host-to-workspace directory copies now combine source safety validation and archive generation in one Rust traversal, removing a host `tar` process and duplicate metadata walk.
- Global `--verbose` emits opt-in phase timing diagnostics to stderr for benchmark and troubleshooting runs.
- Workspace creation performs filesystem, namespace-reference, metadata, and storage preparation outside the global registry lock, then commits metadata with a final identity-checked transaction.
- Persistent command output is drained with nonblocking polling and bounded buffers so stdout/stderr cannot deadlock the helper or consume unbounded memory.
- Persistent helper sockets use short, private runtime paths and inherited pidfds/namespace descriptors to avoid repeated `/proc` lookups and to reject reused runtime identities.
- Directory archive transfers use gzip-aware tar flags only when requested; regular-file transfers retain the direct kernel streaming path.
- Performance documentation now records the new helper, worker-pool, gzip, and release-candidate validation paths.

### Fixed
- A malformed or unauthorized persistent-helper request no longer terminates the helper process; it is rejected and logged while the helper remains available for the owning client.
- Dead pidfds reporting error or hangup events are no longer treated as live workspace runtimes.

## 1.0.5 - 2026-08-10

### Added
- Identity-checked namespace descriptor reuse for daemon-managed workspace commands and transfers, with explicit invalidation after session shutdown.
- Bounded daemon health histograms and lifecycle counters for request latency, phase latency, lock wait, transfers, files, mounts, unmounts, and cleanup retries.
- Opt-in `enclave workspace cp --progress` status output on stderr.
- Deterministic performance fixtures for 4 KiB, 1 MiB, sparse 1 GiB/5 GiB, many-file, and deep-tree workloads.
- Bounded bitmap IP allocation for deterministic network address selection under registry reconciliation.

### Changed
- Cleanup plans load and parse `/proc/self/mountinfo` once per transaction before reverse-depth unmounting.
- Host-to-workspace directory copies stream the host archive producer directly into the namespace-local extractor after source validation, reducing metadata-heavy transfer overhead.
- Regular host-file transfers attempt `splice` before `sendfile` to reduce kernel/userspace copying on supported filesystems and pipes.
- Performance documentation now includes privileged 5 GiB transfer and full lifecycle measurements.

## 1.0.4 - 2026-08-10

### Added
- Rootfs cache indexing with fingerprint validation, atomic index updates, and cache hit/miss metrics.
- Separate bounded daemon control and workspace-transfer queues so long copies cannot monopolize control requests.
- Kernel-assisted regular-file workspace copies using `sendfile`, with secure namespace-local receiving and atomic destination commits.
- `copy_file_range` acceleration for snapshot file copies where the filesystem supports it.
- Daemon health metrics for requests, transfers, bytes, cache hits/misses, and helper process launches.
- Performance regression gating and repeatable stress/transfer benchmark commands under `tools/perf`.

### Changed
- Workspace directory archives now use a single controlled Rust tar traversal and preserve file mode and timestamps without a second recursive accounting pass.
- Snapshot cloning prefers reflinks and then `copy_file_range` before using the portable file-copy path.
- Performance documentation now records implementation evidence, benchmark results, and remaining privileged workload gaps in `PERFORMANCE.md`.
- CI and release verification run the archive performance threshold in addition to formatting, lint, and test checks.

### Fixed
- Workspace file receiving now rejects unsafe targets, parent traversal, symlink components, and pre-existing destination entries inside the workspace namespace.
- Tar-style wrapped arguments remain separated from the internal command executable, including flags such as `-C`.


## 1.0.3 - 2026-08-06

### Added
- Portable workspace snapshot archive support via `enclave snapshot export` and `enclave snapshot import`.
- `enclave workspace resize` for increasing Enclave-managed ext4 workspace disk allocations without recreating the workspace.
- `enclave workspace cp <sandbox> <workspace> <src> <dst>` for streaming files and directories between the host and a running workspace with `ws:/` workspace paths.
- `enclave doctor --repair` for reconciling stale registry entries, orphaned state directories, dead namespace references, stale workspace mounts, and daemon ownership metadata.

### Changed
- Destructive commands (`destroy`, `remove`, and `wipe`) now require an already-running daemon unless the global `--start-daemon` flag is supplied explicitly.
- Daemons now take an exclusive lock for their state directory and record the owning PID, socket, binary version, binary path, and start time in `daemon.lock`.

### Fixed
- Sandbox and workspace destruction are idempotent when mounts, rootfs data, or runtime artifacts are already absent.
- Cleanup now handles nested workspace mounts deepest-first, lazily detaches mounts owned by dead runtimes, and preserves workspace directories while a mount remains active.
- Registry repair now removes safe invalid-metadata or orphan directories, persists corrected metadata, and avoids deleting paths that still contain mounts.
- Unmount failures now identify the mount target, kernel errno, and namespace processes holding the mount while independent cleanup continues.
- Workspace copy now invokes the wrapped `tar` executable correctly, validates workspace-to-host archive entries in a private staging directory, rejects special source files, and commits only to an absent destination entry.

## 1.0.2 - 2026-07-12

### Changed
- Repository verification now allows normal explanatory comments and only rejects unresolved `TODO`/`FIXME`/`XXX` markers in tracked Rust, shell, and workflow files.

### Fixed
- Rust CI is green again after the 1.0.1 release by aligning tests and lockfile metadata with the released version.
- Batch-stop session tests now assert the stable contract instead of a CI-sensitive transient PID outcome.
- Stability tests now track the current released version string.

## 1.0.1 - 2026-07-12

### Added
- `enclave rootfs export`, `enclave rootfs import`, and `enclave rootfs fetch` for distributing prebuilt cached rootfs archives.
- Documentation for hosting a prebuilt rootfs archive on GitHub Releases and reusing it with `bootstrap_method = "cached_rootfs"`.

### Changed
- Workspace startup now fails closed if networking is not actually ready, including missing default-route validation.
- `enclave up`/`enclave down` auto-start paths now respect project configuration more consistently and are less dependent on manual runtime-dir ownership fixes.
- Runtime workspace files are written against the live workspace root instead of the shared sandbox rootfs.
- Sandbox shutdown now stops workspace runtimes as a coordinated batch and cleans up per-workspace resources in parallel instead of waiting through serial per-workspace stop windows.
- Existing-workspace startup is substantially faster on large sandboxes by reusing the session helper at the sandbox level, caching host networking readiness, caching user-namespace mode detection, collapsing repeated host/netns veth setup commands, and skipping unchanged DNS file rewrites.

### Fixed
- Registry mutation no longer silently replaces invalid registry data with an empty registry.
- Session helper resolution now works correctly for library/test-driven startup flows.
- Auth provider visibility now reflects only usable configured tokens.
- Large sandbox stop/start paths no longer scale as poorly with workspace count during normal lifecycle operations.
