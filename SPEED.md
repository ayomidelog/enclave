# Enclave Performance Engineering Plan

> Status: planning document; benchmark every proposal before implementation.

## Purpose

This document describes how to make Enclave dramatically faster without removing,
renaming, weakening, or silently changing any existing command feature.

The goal is not to make one benchmark look good while making the product less safe.
The goal is to reduce startup time, command latency, transfer time, CPU overhead,
disk I/O, and contention while preserving the isolation and recovery guarantees that
make Enclave useful.

The recommendations are based on the current Rust codebase, including the CLI and
daemon boundary, registry persistence, sandbox lifecycle, workspace lifecycle,
namespace session helper, network setup, quota-backed storage, snapshots, and
workspace copy implementation.

Every optimization should be measured before and after.

Every optimization should have a correctness test.

Every optimization that touches isolation should have a security review.

Every optimization should preserve the current command surface.

## Non-goals

This plan does not remove `workspace cp`.

This plan does not remove snapshots.

This plan does not remove rootfs import, export, or fetch.

This plan does not remove the daemon.

This plan does not remove namespace isolation.

This plan does not make host-backed workspace directories mandatory.

This plan does not make quota-backed workspaces mandatory.

This plan does not remove policy checks.

This plan does not remove namespace reference validation.

This plan does not make destructive commands implicitly start a daemon.

This plan does not trade crash safety for a faster happy path.

This plan does not require a breaking protocol change as a first step.

## Executive summary

Enclave has several different performance profiles rather than one single speed.

A cold `enclave up` can be dominated by rootfs bootstrap and setup commands.

A warm `enclave up` can be dominated by registry reads, metadata writes, namespace
creation, mount setup, network setup, and process startup.

A repeated `workspace exec` can be dominated by namespace entry and helper startup.

A large `workspace cp` can be dominated by tar traversal, userspace copies, and
filesystem cache behavior.

A quota-backed workspace can be dominated by external utility startup and mount
operations.

A listing command can be dominated by repeatedly reading and parsing JSON files.

A destroy or repair command can be dominated by mountinfo scans and serialized cleanup.

The largest likely wins are:

1. Keep the daemon hot and make the request path cheaper.
2. Replace repeated whole-registry persistence with a transactional indexed store.
3. Cache stable sandbox and workspace metadata in daemon memory.
4. Reduce process spawning for `mount`, `umount`, `ip`, `mountpoint`, `truncate`,
   `mkfs.ext4`, `resize2fs`, `find`, and `tar` where safe.
5. Reuse workspace runtime infrastructure without reusing security-sensitive state.
6. Optimize workspace startup as a parallel dependency graph.
7. Make `workspace cp` use a high-throughput streaming path with fewer scans.
8. Use efficient rootfs cloning instead of recursive `cp` when the filesystem allows it.
9. Batch network and namespace setup operations.
10. Add a permanent benchmark and regression budget to prevent performance drift.

The order matters.

The project should first add observability and benchmarks.

Then it should remove avoidable repeated work.

Then it should optimize system calls and process boundaries.

Then it should investigate kernel- and filesystem-specific acceleration.

## Current architecture as a performance model

### CLI and daemon

The CLI is a `clap` application in `src/cli.rs`.

The CLI sends JSON requests through `src/client.rs`.

The daemon owns lifecycle operations through `src/daemon/mod.rs` and
`src/daemon/dispatch.rs`.

The daemon is the natural place for hot metadata caches, shared workers, and
long-lived runtime services.

The daemon is also a potential source of contention because all lifecycle commands
share its request handling and state locks.

The JSON protocol is appropriate for control-plane requests.

The JSON protocol should not carry large data-plane payloads.

The current `workspace cp` design correctly keeps file payloads outside the JSON
request and response path.

### Sandbox and workspace hierarchy

A sandbox has a root filesystem and can contain multiple workspaces.

The sandbox rootfs is mounted at runtime.

Each workspace has its own user, PID, mount, network, and UTS namespaces.

The workspace runtime stores PID information, readiness state, and namespace
references.

Workspace home data can be managed by Enclave or supplied as a host-backed path.

Quota-backed workspaces use sparse images and ext4.

Workspace home data can also use overlay directories.

This hierarchy gives strong isolation, but it creates setup and teardown work that
must be measured separately.

### State and persistence

Registry state is persisted under the configured state directory.

Sandbox and workspace metadata are persisted as JSON.

Atomic writes use temporary files, syncing, permissions, and rename.

That is a good safety baseline.

It also means every metadata mutation can involve serialization, allocation, a file
creation, a full write, a file sync, a permission operation, and a rename.

Those operations are acceptable for critical commits.

They are expensive when repeated unnecessarily in a hot loop.

### Namespace entry

`src/workspace/session` owns runtime launch, namespace reference handling, PID
validation, user namespace setup, process handling, and security application.

The namespace references are intentionally validated against the runtime PID and
recorded start time.

That validation must remain.

The optimization opportunity is to avoid reopening and revalidating identical
stable descriptors repeatedly while still rechecking process identity at the
correct security boundary.

### Workspace copy

`src/workspace/cp` separates path validation and streaming behavior.

Host-to-workspace and workspace-to-host transfers use tar-style streaming.

The current implementation includes archive validation, staging, no-overwrite
commit behavior, source validation, and namespace runtime validation.

Those guarantees must remain.

The performance work should focus on avoiding duplicate traversal, reducing copies,
using larger buffers, and selecting the cheapest safe path for regular files.

## Performance principles

### Measure user-visible latency

Measure time from CLI invocation to useful output.

Do not measure only the daemon function body.

CLI startup, config parsing, socket connection, request serialization, daemon queue
wait, work, response serialization, and terminal output all contribute to latency.

### Separate cold and warm paths

A cold path includes an empty page cache, a new daemon, a new sandbox, and no cached
rootfs.

A warm path includes a running daemon, existing metadata, cached rootfs, and an
existing workspace definition.

Both matter.

Cold start determines first-use experience.

Warm start determines development-loop productivity.

### Preserve safety boundaries

Do not remove path validation because it adds microseconds.

Do not skip PID start-time validation because it adds a file read.

Do not skip atomic persistence because it adds a sync.

Do not use a faster archive extractor that permits traversal or unsafe types.

Do not reuse a namespace reference after its owning runtime has changed.

### Optimize repeated work first

A one-time 10 millisecond operation is usually less important than a one-million
entry directory traversal repeated on every request.

Look for repeated parsing, repeated path canonicalization, repeated command lookup,
repeated mountinfo reads, repeated process spawning, and repeated directory scans.

### Make fast paths explicit

The code should have named fast paths and named fallback paths.

A fast path should declare its preconditions.

The fallback should preserve existing behavior.

Logs and metrics should identify which path was selected.

### Prefer bounded memory

Large transfers must not become large in-memory buffers.

Metadata caches must have limits.

Archive validation must stream.

Snapshot operations must avoid reading entire files into memory.

### Optimize for real workloads

Benchmark small files, large files, many files, deep trees, sparse files, and
high-latency storage.

Benchmark one workspace and many workspaces.

Benchmark one command and concurrent commands.

Benchmark ext4, XFS, and the filesystem types supported by deployment environments.

## Baseline and benchmark harness

### Benchmark command matrix

Create a repeatable benchmark harness outside the normal unit test path.

The harness should expose subcommands for:

- daemon ping latency;
- daemon health latency;
- sandbox create with cached rootfs;
- sandbox create with debootstrap;
- sandbox start;
- sandbox stop;
- sandbox destroy;
- workspace create;
- workspace start cold;
- workspace start warm;
- workspace stop;
- workspace exec with a short command;
- workspace exec with a long-running command;
- workspace list;
- workspace status;
- workspace stats;
- workspace ps;
- workspace cp for a small file;
- workspace cp for a large file;
- workspace cp for many small files;
- workspace cp for a large directory;
- snapshot create;
- snapshot export;
- snapshot import;
- snapshot restore;
- doctor check;
- doctor repair.

### Timing layers

Capture a monotonic timestamp in the CLI before config loading.

Capture a timestamp immediately before connecting to the daemon.

Capture a timestamp after the socket connection succeeds.

Capture a timestamp after the request is written.

Capture a timestamp when the daemon begins dispatch.

Capture a timestamp when the lifecycle operation begins.

Capture timestamps around every major phase.

Capture a timestamp before the response is serialized.

Capture a timestamp after the CLI receives the response.

Capture a timestamp after terminal rendering.

Do not expose all internal timing by default.

Expose it through `--verbose`, an environment variable, or a benchmark-only mode.

### Distribution statistics

Report median latency.

Report p50 latency.

Report p90 latency.

Report p95 latency.

Report p99 latency.

Report minimum and maximum only as supporting context.

Averages alone are not sufficient because mount and filesystem operations can have
heavy tails.

Run enough iterations to make the result stable.

Discard warm-up iterations explicitly.

Record kernel version, CPU model, RAM, storage device, filesystem, mount options,
Rust version, and whether the daemon is already running.

### Throughput statistics

For transfers, report bytes per second.

Report files per second.

Report metadata entries per second.

Report CPU seconds per gigabyte.

Report system calls per gigabyte when tracing is enabled.

Report peak resident memory.

Report page faults.

Report read and write amplification where measurable.

### Benchmark fixtures

Build deterministic fixtures rather than relying on a developer home directory.

Use a 4 KiB file.

Use a 1 MiB file.

Use a 1 GiB file.

Use a 5 GiB file.

Use 100 files of 1 MiB.

Use 100,000 files of 4 KiB.

Use a directory with ten levels of nesting.

Use a directory containing symlinks.

Use a directory containing rejected special files for validation tests.

Use sparse files.

Use incompressible random data.

Use highly compressible text data.

Use a workspace with a host-backed directory.

Use a workspace with a quota-backed image.

### System tracing

Use `strace -f -c` for syscall counts.

Use `strace -f -ttT` for targeted latency investigations.

Use `perf stat` for CPU cycles, instructions, context switches, page faults, and
branch misses.

Use `perf record` and flamegraphs for CPU hotspots.

Use `iostat` or equivalent for block I/O.

Use `pidstat` for process and context-switch behavior.

Use `/proc/<pid>/io` for process-level read and write totals.

Use `/proc/self/mountinfo` only when the operation actually requires it.

Use eBPF tools in privileged benchmark environments for production-like profiling.

Do not make tracing a required runtime dependency.

### Performance budget

Define a budget before optimization.

A warm daemon ping should be near socket and JSON overhead.

A workspace exec that runs a trivial command should not spend most of its time in
unrelated registry persistence.

A large file copy should approach the throughput of a local sequential copy on the
same storage device.

A listing command should scale with metadata entries, not with unrelated file data.

A destroy operation should scale with actual resources, not with repeated retries
of already-absent resources.

Store baseline results in versioned benchmark output.

Fail performance CI only after the benchmark is stable enough to avoid noise.

## Latency map by command

### `ping` and `health`

Expected costs include CLI startup, config parsing, Unix socket connection, JSON
serialization, daemon dispatch, and response parsing.

These commands should be the smallest possible control-plane baseline.

If ping is slow, every command feels slow.

The first optimization is connection setup and daemon availability.

The second optimization is request framing and response framing.

The third optimization is avoiding unnecessary logging and registry work.

### `workspace create`

Expected costs include registry lock acquisition, registry parse, sandbox lookup,
workspace ID generation, directory creation, metadata file creation, optional disk
image creation, and registry persistence.

The expensive branch is quota-backed storage because it may run `truncate` and
`mkfs.ext4`.

The common branch should not pay quota-specific checks or utility discovery costs.

### `workspace start`

Expected costs include metadata lookup, sandbox validation, rootfs mount checks,
namespace creation, user namespace setup, mount setup, network setup, auth mounts,
resource limits, security hardening, runtime metadata writes, and readiness wait.

This is a dependency graph, not one indivisible step.

Independent host-side preparation can run in parallel.

Namespace-sensitive operations must remain ordered and isolated.

### `workspace exec`

Expected costs include metadata lookup, runtime identity validation, opening namespace
references, `setns`, command process creation, security setup where applicable, and
stdio forwarding.

The command itself may be much shorter than the setup around it.

This is the strongest candidate for a persistent session helper or optimized helper
protocol.

### `workspace cp`

Expected costs include source validation, path planning, tar startup, archive
traversal, pipe transfer, namespace entry, staged extraction, destination validation,
and atomic commit.

The safe path is intentionally more work than `cp`.

The right goal is to remove duplicate work, not to remove safety checks.

### `workspace stop` and `destroy`

Expected costs include runtime process termination, namespace cleanup, port cleanup,
network teardown, unmounting, storage cleanup, metadata persistence, and registry
persistence.

Already-absent resources should be cheap.

Nested mounts should be discovered once and unmounted in reverse-depth order.

Independent workspace cleanup can run concurrently with bounded parallelism.

## Highest-priority optimization: daemon hot path

### Keep the daemon resident

The daemon already exists to centralize lifecycle state and authority.

Users should be encouraged to run a persistent daemon for development workflows.

`enclave daemon start` should make the hot path predictable.

Commands should avoid starting a new daemon when a healthy owned daemon already
exists.

Commands should avoid polling with unnecessarily long intervals.

The daemon readiness protocol should be a single cheap ping.

### Reduce CLI work

Load only the configuration needed for the selected command.

Do not scan for an Enclavefile for commands that do not use it.

Do not parse project configuration for global daemon commands.

Do not initialize formatting state before the response is received.

Reuse a prepared request buffer where practical.

Avoid pretty JSON for internal request payloads.

Use compact JSON internally while preserving human-readable output at the CLI.

### Improve request framing

The current newline-delimited JSON protocol is simple and useful.

Keep it for compatibility.

Use a length prefix only as an optional protocol version when profiling proves the
newline framing is material.

Avoid a protocol rewrite before measuring.

For large command output, stream through a dedicated data channel rather than
embedding output in a large JSON value.

### Avoid needless socket churn

A single CLI invocation normally needs one request.

For future batch commands, support multiple actions over one connection.

Do not open one socket per workspace when a command operates on many workspaces.

Use a connection pool only inside long-lived daemon-side components, not in a short
CLI process where setup would cost more than it saves.

### Daemon request scheduling

The daemon should distinguish short control requests from long data transfers.

A long `workspace cp` should not block ping, health, or unrelated metadata requests.

Use bounded worker threads or an async event loop with explicit blocking pools.

Never run blocking filesystem and subprocess work on a single request accept thread.

Keep authorization and state-lock acquisition explicit.

Measure queue wait separately from operation time.

## Registry and metadata optimization

### Current cost model

The registry uses JSON persistence and an atomic replace pattern.

A mutation can read a complete registry, deserialize it, mutate a structure,
serialize it, write a temporary file, sync the file, and rename it.

This is robust but does not scale linearly with the changed record.

A registry with many sandboxes and workspaces pays for unrelated entries.

### Short-term cache

Add a daemon-owned in-memory registry cache.

Load the registry once after daemon startup.

Track the registry file identity and modification time.

Refresh only when an external writer changes the file.

All daemon mutations update the cache and persistence transaction together.

Keep the existing JSON file as the durable compatibility format initially.

Do not allow commands outside the daemon to mutate the file silently without a
refresh strategy.

### Dirty-state batching

Mark individual sandbox and workspace records dirty.

Coalesce multiple mutations from one high-level command into one durable write.

For example, `up` should not persist after every tiny intermediate state if the
intermediate state is not needed for crash recovery.

Persist at defined recovery checkpoints.

Preserve enough state before every operation that can leave resources behind.

Document which state transitions are durable boundaries.

### Avoid pretty serialization internally

Pretty JSON increases bytes written and serialization work.

Use compact JSON for internal metadata if humans do not directly edit it.

If readability is a supported workflow, provide a formatting command or retain
pretty metadata only for user-facing files.

Measure before changing the on-disk format.

### Incremental metadata files

Consider one metadata file per sandbox and workspace as the primary source of truth.

Use `registry.json` as an index or compatibility snapshot.

A single workspace update should not rewrite unrelated workspaces.

The existing reconciliation and repair code is a foundation for this design.

Migration must be atomic and restartable.

Old versions must fail clearly or read the new format safely.

### Lock contention

Measure time waiting on `registry.lock` and daemon state locks.

Do not hold a global registry lock while running `mount`, `ip`, `tar`, `mkfs`, or
long-running user commands.

Read the record and reserve a state transition under the lock.

Release the lock during external work when another operation can safely proceed.

Reacquire the lock to commit the result.

Use generation numbers to detect stale commits.

### Avoid repeated filesystem reconciliation

A normal status or list command should not run a full repair pass.

Repair should remain explicit through `doctor --repair` and lifecycle recovery
paths.

Cheap runtime validation can use cached process identity information with a bounded
refresh interval.

Full mountinfo scans should happen only for cleanup, repair, or uncertain state.

## Sandbox rootfs optimization

### Rootfs bootstrap is a separate workload

`debootstrap` is inherently expensive and network-sensitive.

It should not be treated as the normal warm-start benchmark.

Cache misses should be visible in timing output.

Cache hits should be optimized independently.

### Better cached rootfs cloning

The current cached-rootfs path recursively copies a directory and may invoke the
host `cp` utility.

That is simple and portable but can be expensive for a large rootfs.

Evaluate these safe alternatives in order:

1. Clone with filesystem reflinks where supported.
2. Clone with hard links only for immutable content and never for writable state.
3. Use a filesystem snapshot or subvolume when explicitly configured.
4. Use archive streaming only when it outperforms recursive copy.
5. Retain recursive copy as the portable fallback.

Do not hard-link mutable rootfs data.

Do not expose a writable shared rootfs accidentally.

### Rootfs cache metadata

Record suite, architecture, source, creation time, content digest, and tool version.

Avoid probing every possible cache path on every request.

Maintain a small index of available cache variants.

Invalidate the index when imports complete.

### Rootfs mount reuse

A running sandbox should keep its rootfs mount for the lifetime of the sandbox.

Workspace starts should reuse the existing sandbox mount.

Do not remount the same rootfs for each workspace.

Stop should unmount only after all workspaces have stopped.

### Setup command optimization

Setup commands are part of `up` and can dominate startup.

Record a configuration digest for setup inputs and commands.

If the Enclavefile and relevant inputs are unchanged, avoid rerunning idempotent
setup commands only when the user explicitly opts into cached setup behavior.

The default behavior must remain compatible with current semantics.

An explicit `--no-cache` or `--rebuild` path should force execution.

Never skip setup when the current command contract says it is re-applied.

### Parallel setup preparation

Prepare command arguments, environment, and validated paths before entering the
sandbox.

Commands that are semantically independent could be parallelized only if the
Enclavefile declares them independent or if the existing semantics guarantee it.

Do not reorder setup commands by default.

## Workspace startup optimization

### Model startup as phases

Use explicit phases such as:

1. Load and validate metadata.
2. Reserve the workspace transition.
3. Ensure sandbox rootfs is ready.
4. Prepare storage.
5. Prepare network allocation.
6. Create namespaces.
7. Mount workspace filesystems.
8. Apply identity mapping.
9. Mount auth and environment data.
10. Apply resource limits.
11. Apply security hardening.
12. Launch the runtime command.
13. Write namespace references.
14. Mark readiness.
15. Publish ports.

The exact order must follow security and dependency requirements.

The phase model makes timing and parallelism visible.

### Parallel host-side preparation

Before namespace creation, prepare independent host-side work concurrently.

Candidates include directory creation, auth token reads, environment token reads,
port specification validation, cgroup path preparation, and network allocation.

Do not parallelize operations that race on the same mountpoint.

Do not parallelize metadata writes without a transaction owner.

### Reduce process startup

A workspace start may invoke multiple external utilities.

Inventory every command spawned during startup.

Prefer direct Rust or `nix` APIs for small, stable operations.

Keep external tools where portability or kernel behavior makes them safer.

Do not replace a reliable utility only to save an unmeasured few milliseconds.

Cache resolved executable paths when the environment is stable.

Avoid invoking `sh -c 'command -v ...'` repeatedly.

### Persistent session helper

The sandbox already caches a session helper binary.

Extend the helper protocol rather than spawning a fresh helper for every operation.

A persistent helper can receive authenticated commands over a private Unix socket.

Each request should still validate workspace identity, PID, namespace references,
policy, and command boundaries.

The helper must not retain ambient privileges between requests.

Reset signal handlers, file descriptors, environment, working directory, and
resource limits for every request.

Use a per-request child process for untrusted command execution if needed.

The persistent process should own only expensive namespace plumbing.

### Readiness signaling

Replace polling-heavy readiness checks with a pipe or Unix socket handshake.

The runtime should write readiness after all required mounts and hardening are done.

The parent should wait on the descriptor rather than sleep and poll.

Retain a timeout and diagnostic fallback.

Record readiness phase timing.

### Runtime metadata writes

Write the minimum metadata needed for crash recovery before launch.

Write namespace references only after they are valid.

Use one atomic metadata commit for the final ready state.

Avoid writing the same workspace JSON multiple times during successful startup.

## Namespace and exec latency

### Namespace file descriptor cache

Namespace reference files are stable only while the runtime identity is stable.

A daemon-side cache may retain open file descriptors for mount and PID namespaces.

Before using a cached descriptor, verify the runtime PID and start time.

Invalidate descriptors on stop, restart, PID reuse, or failed `setns`.

Do not cache descriptors across daemon ownership changes unless explicitly safe.

### Minimize repeated validation without skipping it

Validation can be split into cheap and expensive portions.

Cheap validation checks in-memory state, PID, start time, and expected paths.

Expensive validation opens and compares namespace references.

Perform the cheap checks on every request.

Perform the expensive checks when the cached descriptor is first used, after a
runtime generation change, and at a bounded revalidation interval.

A security-sensitive policy can force validation every time.

### Direct process creation

Measure whether the current launch path invokes shell wrappers unnecessarily.

Use direct `execve`-style process creation for commands that do not require shell
syntax.

Keep shell semantics for commands that intentionally use a shell.

Do not reinterpret user command strings silently.

### Stdio path

Use direct inherited file descriptors where possible.

Avoid extra relay threads when a descriptor can be passed directly.

Use `splice` or `tee` for large pipe transfers on Linux where safe.

Retain fallback `io::copy` for unsupported descriptors.

Do not buffer interactive output.

### Environment construction

Cache immutable environment fragments for the sandbox.

Construct workspace-specific environment values separately.

Avoid cloning large maps for every exec request.

Keep auth and secret handling isolated from ordinary environment caching.

## Workspace copy optimization

### Preserve the current safety contract

The copy command must continue to support host-to-workspace transfers.

It must continue to support workspace-to-host transfers.

It must continue to support directories.

It must continue to support workspace path prefixes.

It must continue to auto-detect direction.

It must continue to reject ambiguous paths.

It must continue to protect against archive traversal.

It must continue to reject unsafe archive entry types.

It must continue to avoid unsafe overwrites.

It must continue to cancel on client disconnect.

### Measure the current transfer pipeline

Record source validation time.

Record namespace entry time.

Record tar startup time.

Record tar traversal time.

Record bytes read from source.

Record bytes written to destination.

Record staged extraction time.

Record final commit time.

Record cleanup time.

Report throughput separately from total latency.

### Avoid duplicate source traversal

Host source validation currently needs to inspect the source for safety and size.

The transfer path may then traverse the same tree again to create an archive.

For regular files, combine validation and transfer eligibility into one stat operation.

For directories, investigate a single traversal that validates entries while feeding a
controlled archive writer.

Do not let archive generation bypass the validation policy.

If a combined traversal becomes too complex, retain the two-pass safe fallback.

### Buffer sizing

Use a large reusable buffer for host-side streaming.

Benchmark 64 KiB, 256 KiB, 1 MiB, and 4 MiB buffers.

Avoid allocating a new buffer per file.

Avoid copying from a read buffer into multiple intermediate vectors.

Use vectored I/O for metadata where it helps.

### Direct regular-file fast path

A single regular file can use a specialized path.

Validate source type and destination plan first.

Open the source with no-follow semantics where required.

Open the destination staging file with exclusive creation.

Copy with `sendfile` or `copy_file_range` when available.

Verify the source and destination contract before the atomic commit.

Fall back to the tar path for directories, symlinks, and special semantics.

The fast path must preserve no-overwrite behavior.

### Directory transfer fast path

For a directory with only regular files and directories, consider a Rust archive
writer that streams directly into the workspace-side extractor.

Keep symlink policy explicit.

Keep path normalization identical to the current validator.

Reject hard links and device nodes unless the existing contract is expanded through
a deliberate feature proposal.

Use a single archive format for compatibility between host and workspace paths.

### Compression policy

Do not gzip by default for local transfers.

Compression consumes CPU and may reduce throughput on fast storage.

Add an explicit `--gzip` option only after measuring networked or slow-storage use.

Select compression based on observed data entropy only if the decision is predictable
and visible to users.

Use a larger pipe buffer for compressed streams.

### Progress reporting

Progress must not slow down the default path.

Make it opt-in with `--progress`.

Update at a bounded interval rather than once per chunk.

Use bytes transferred, total bytes when known, throughput, and estimated remaining
time.

Do not require a full directory scan solely to calculate progress.

### Client disconnect handling

Use poll or readiness notifications rather than frequent fixed sleeps.

The current wait loop uses periodic polling for child completion and disconnects.

Replace fixed sleep with a signal-aware wait where practical.

Retain cancellation and child cleanup guarantees.

### Workspace-side tar startup

The wrapped command must continue using an explicit `--` separator before user or
tar arguments.

Keep the executable-before-flags regression test.

Resolve workspace tar or busybox availability once per workspace generation.

Do not probe multiple executables for every transfer when the preferred executable is
known to work.

Invalidate the preference after a runtime restart or image change.

### Pipe and socket tuning

Measure Linux pipe capacity and transfer throughput.

Use `F_SETPIPE_SZ` where permitted and useful.

Avoid changing global kernel settings.

Use separate stderr handling that cannot deadlock when stdout carries large data.

Ensure child processes inherit only the required descriptors.

## Storage and filesystem optimization

### Host-backed workspace paths

Host-backed paths avoid image mount overhead.

They may have slower metadata behavior depending on filesystem and mount options.

Use direct path operations when the workspace is already mounted.

Avoid canonicalizing the same path repeatedly.

Cache validated mount source identity while the workspace generation is unchanged.

Invalidate on configuration update or detected mount change.

### Quota-backed images

Quota-backed workspaces provide isolation and accounting but require image setup.

Do not run `mountpoint` as a shell process for every operation.

Use `/proc/self/mountinfo` or `statfs`-based checks where correct.

Cache the mounted state only with a generation and mount identity.

Mount the image once for a running workspace.

Avoid mounting and unmounting around every copy or metadata operation.

### Sparse file behavior

Sparse images avoid writing all allocated bytes initially.

Measure first-write latency and fragmentation.

Use allocation and filesystem sizing policies appropriate to the storage device.

Do not eagerly zero a multi-gigabyte image unless required by a security policy.

### Ext4 creation and resize

`mkfs.ext4` and `resize2fs` are expensive but infrequent.

Keep them off the normal warm start path.

When resizing, stop the runtime only when required by mount safety.

Future optimization may resize offline storage with a shorter downtime window, but
correctness comes first.

Record resize duration and bytes changed.

### Overlay directories

Overlay lower layers should be immutable and shared where possible.

Workspace upper and work directories must remain private.

Avoid scanning overlay trees to determine whether they are active if mount state can
answer the question directly.

Use mount generation metadata to distinguish a stale directory from a live mount.

### Snapshot storage

Current snapshots are copy-based and can duplicate large workspace data.

Measure snapshot create, restore, export, and import separately.

Consider reflink snapshots when supported.

Consider filesystem snapshots when explicitly configured.

Retain copy-based fallback for portability.

Never expose a snapshot as mutable shared state accidentally.

### Snapshot garbage collection

GC should use indexed snapshot metadata rather than recursively scanning every file
on every invocation.

Compute retention candidates from metadata first.

Validate paths before deletion.

Run deletion with bounded parallelism.

Avoid competing with active workspace I/O without an explicit scheduling policy.

## Mount and cleanup performance

### Current cleanup requirements

Cleanup handles missing rootfs and mount resources as already-cleaned states.

Registry records are removed after individual resources are confirmed absent.

Repair reconciles registry records with on-disk paths.

Workspace cleanup removes stale images, namespace references, overlays, and metadata.

Nested mounts are considered in reverse-depth order.

Dead owners can require lazy unmount.

Namespace references are validated against runtime PID and start time.

These behaviors must remain.

### Mountinfo parsing

Reading and parsing `/proc/self/mountinfo` is useful but not free.

Read it once per cleanup transaction.

Parse into a normalized mount table.

Reuse the table for all resources in that transaction.

Do not re-read mountinfo for every individual workspace directory.

Compare mountpoints using normalized path components rather than repeated string
allocations where practical.

### Reverse-depth unmount planning

Build a list of mounts under the target path.

Sort by descending path depth.

Attempt normal unmount first.

Use lazy unmount when the owning namespace or process is confirmed dead.

Treat absent and already-unmounted resources as success.

Stop retrying a mount after a terminal error that cannot be changed by another
resource cleanup step.

### Parallel cleanup

Cleanup independent workspaces concurrently with a small worker limit.

Do not concurrently unmount parent and child paths.

Do not concurrently mutate the same registry record.

Use per-workspace cleanup plans and a final sandbox barrier.

The sandbox rootfs must remain until all dependent workspace mounts are gone.

### Syscall versus utility choice

Inventory external `umount`, `mountpoint`, and `ip` invocations.

Use `nix` or libc calls where error classification is reliable.

Retain utilities if they provide required behavior such as lazy unmount options or
portable diagnostics.

Measure process startup overhead before replacing them.

### Cleanup retry policy

Use bounded retries for transient busy states.

Use exponential backoff with a short maximum for expected process exit races.

Do not sleep for a fixed long interval after every failure.

Log the resource state and retry reason.

Make successful cleanup idempotent and cheap.

## Networking optimization

### Current network costs

Workspace networking can create bridge, veth, namespace, IPAM, NAT, anti-spoofing,
DNS, and published-port state.

Each external `ip` or firewall command has process and kernel round-trip cost.

Network setup may be a large fraction of warm workspace startup.

### Ensure host networking once

The bridge and host NAT should be initialized once per daemon or active sandbox set.

Do not run full ensure logic for every workspace if a generation marker proves the
host network is ready.

Check the marker against actual interface state after daemon restart.

### Batch network operations

Allocate workspace IPs from an in-memory pool under the daemon lock.

Batch creation of independent veth pairs only if the kernel APIs and error handling
remain clear.

Group firewall rule updates into one transaction where possible.

Avoid rewriting identical NAT rules.

### Port publisher

The port publisher should avoid one thread per idle port if an event loop can serve
many listeners.

Use a bounded acceptor and connection worker model.

Keep separate per-workspace ownership and cleanup.

Reuse buffers for proxied traffic.

Do not allow a slow port to starve unrelated workspaces.

### IPAM

Keep used IPs in memory while the daemon owns the registry.

Reconcile after restart.

Avoid scanning every workspace for every allocation.

Use a compact bitmap or bounded set for the workspace subnet.

## Registry, policy, and observability interaction

### Policy checks

Policy authorization is part of the security contract.

Cache the parsed policy in the daemon.

Invalidate on policy file changes or policy mutations.

Do not parse policy JSON for every request.

Keep authorization decisions observable in debug timing.

### Logging

Synchronous formatting and writing can add latency to hot commands.

Use structured events with cheap disabled paths.

Avoid formatting large command output twice.

Use asynchronous logging only if ordering and crash diagnostics remain acceptable.

Keep lifecycle errors durable even if verbose logs are dropped.

### Metrics

Add counters for command requests.

Add histograms for total latency.

Add histograms for phase latency.

Add counters for process spawns.

Add counters for mount and unmount operations.

Add bytes and files for copy operations.

Add registry lock wait time.

Add cache hit and miss counters.

Add cleanup retry counters.

Expose metrics through `health` or a debug-only endpoint without changing normal
command output.

## Safe caching strategy

### Cache categories

Cache immutable configuration.

Cache parsed policy.

Cache rootfs cache index.

Cache daemon-owned registry metadata.

Cache resolved utility paths.

Cache network readiness.

Cache runtime namespace file descriptors with identity checks.

Cache workspace command capability detection.

### Cache invalidation

Every cache needs an explicit invalidation event.

Invalidate workspace caches on metadata update.

Invalidate runtime caches on stop and restart.

Invalidate rootfs caches after import, replace, or delete.

Invalidate policy caches after policy mutation.

Invalidate network caches after daemon restart or detected interface loss.

Do not use time-based invalidation as the only correctness mechanism.

### Generation numbers

Assign a daemon generation.

Assign a sandbox runtime generation.

Assign a workspace runtime generation.

Include generations in cached records.

Reject stale cache entries rather than guessing.

## Process spawning reduction

### Spawn inventory

Build a table of every `Command::new` call.

Record command name, phase, expected frequency, and alternative API.

Prioritize calls on repeated hot paths.

Do not prioritize one-time debootstrap before repeated exec or list operations.

### Direct APIs

Use Rust filesystem APIs for directory creation, metadata, rename, and removal.

Use `nix` for namespace, mount, signal, and socket operations where already supported.

Use libc only behind small tested wrappers.

Keep error messages as descriptive as the current utilities.

### Utility retention

External tools can encode complex filesystem and network behavior.

Keep them for portability when replacement risks regressions.

If a utility remains, avoid launching a shell just to discover it.

Resolve and cache its absolute path.

### Process pooling

Do not pool untrusted command processes.

A helper or worker can be pooled only for trusted plumbing.

Reset all per-request state.

Bound the pool.

Shutdown cleanly on daemon stop.

## Memory and allocation optimization

### Request allocation

Reuse request and response buffers in daemon workers where safe.

Avoid cloning large `serde_json::Value` trees.

Deserialize directly into typed parameter structs for hot actions.

Retain generic JSON dispatch for compatibility and low-frequency actions.

### Metadata allocation

Use borrowed deserialization where practical for read-only requests.

Avoid serializing metadata merely to compare it when a dirty flag can answer the
same question.

Use compact vectors and maps with known capacities for workspace lists.

### Transfer allocation

Use one reusable transfer buffer.

Avoid `Vec` growth based on untrusted archive headers.

Cap metadata entry names and error buffers sensibly.

Do not accumulate all tar entries in memory.

### Output allocation

Stream logs and command output.

Avoid collecting unbounded stderr from long-running commands.

Keep bounded diagnostic output with a clear truncation marker.

## Kernel and filesystem acceleration

### Copy-on-write cloning

Detect reflink support at startup or on demand.

Use `ioctl(FICLONE)` for rootfs and snapshot copies where supported.

Fall back cleanly to the current safe copy path.

Measure both speed and resulting storage usage.

### `copy_file_range`

Use `copy_file_range` for regular file transfers when source and destination are
local filesystems that support it.

Fall back to buffered copying on `EXDEV`, `EINVAL`, or unsupported filesystems.

Do not use it for paths whose security semantics require a staging archive.

### `sendfile`

Use `sendfile` for regular-file-to-pipe transfer when it avoids copies.

Benchmark against `splice` and buffered I/O.

Keep cancellation and short-write handling correct.

### `splice`

Use `splice` for file-to-pipe and pipe-to-file paths where supported.

Keep a portable buffered fallback.

Test behavior with full pipes, interrupted system calls, and disconnected clients.

### `io_uring`

Treat `io_uring` as an optional later experiment.

Do not introduce it before measuring that ordinary syscalls are the bottleneck.

Keep compatibility with kernels and security policies supported by Enclave.

## Concurrency model

### Bounded parallelism

Use bounded workers for independent workspace operations.

Do not create one unbounded thread per workspace.

Make the worker count configurable for benchmarks and constrained hosts.

### Lock hierarchy

Document lock ordering between daemon state, registry, sandbox, workspace, and
network locks.

Never hold a global lock while waiting for an external process.

Never hold a lock while waiting for user input.

Never hold a lock during a multi-gigabyte transfer.

### Batch commands

`up`, `down`, `stats`, `ps`, and `wipe` naturally operate on multiple resources.

Use one registry snapshot and one plan for the batch.

Parallelize only independent phases.

Aggregate errors without discarding successful cleanup results.

### Fairness

A large copy should not prevent lifecycle health checks from completing.

A slow debootstrap should not block a short ping.

A burst of status commands should not starve cleanup.

Use request classes and bounded queues.

## Command-specific proposals

### `enclave up`

Cache parsed Enclavefile content for one invocation.

Resolve all workspace definitions before making mutations.

Create missing workspace directories in parallel where safe.

Start workspaces in parallel after the sandbox is ready.

Preserve deterministic error reporting by sorting results by definition order.

Wait for all started workspaces before publishing the final summary.

Use one daemon transaction for the plan and per-resource commits for recovery.

### `enclave down`

Build a stop plan once.

Stop workspace runtimes in parallel with a bounded worker count.

Clear published ports as each workspace stops.

Tear down network resources after dependent workspaces finish.

Stop the sandbox only after all workspaces are confirmed absent.

### `enclave workspace list`

Read cached metadata.

Do not stat every data file for ordinary listing.

Use a `--verify` or existing status path for expensive live validation.

Keep normal output fast and predictable.

### `enclave workspace status`

Use cached metadata for static fields.

Validate runtime PID and start time for dynamic status.

Use a single namespace and process inspection pass.

Avoid invoking external tools for values already available from `/proc`.

### `enclave stats`

Read `/proc` files in a bounded batch.

Avoid repeated parsing of the same process status files.

Collect data concurrently only within a safe limit.

Return partial results with explicit errors when one workspace disappears during a
batch.

### `enclave ps`

Use one process enumeration per PID namespace where possible.

Do not launch one helper per process.

Cache static command names only for one observation cycle.

### `enclave doctor`

Keep a quick check mode for routine health.

Run full mount and filesystem reconciliation only for `--repair` or explicit deep
checks.

Parallelize independent read-only checks.

Serialize repairs that mutate the same resource tree.

### `enclave snapshot`

Use reflinks or filesystem snapshots when available.

Stream export directly from the snapshot source.

Avoid a temporary uncompressed archive when exporting gzip output.

Validate imports while streaming rather than extracting then rescanning when safe.

### `enclave rootfs`

Maintain a cache index.

Stream fetch into a temporary archive with checksum validation.

Extract once into a temporary cache directory.

Atomically publish the completed cache.

Avoid rescanning the cache after every read-only operation.

## Compatibility strategy

### Preserve command names

Every current command remains available.

Every current command argument remains accepted.

Every current workspace path convention remains accepted.

Every current host path convention remains accepted.

### Preserve output contracts

Human-readable output should remain stable unless a deliberate documentation change
is made.

Add optional timing and progress output behind flags.

Do not insert progress text into machine-readable output.

Keep exit status semantics unchanged.

### Preserve failure semantics

A faster command must not turn a failed operation into a successful partial operation.

Atomic commits must remain atomic.

Cleanup should remain idempotent.

Registry reconciliation must remain safe.

### Feature flags

Introduce risky optimizations behind internal feature flags first.

Expose user-facing flags only after stability testing.

Allow disabling reflink, direct copy, persistent helpers, or parallel startup for
troubleshooting.

Make the default selection conservative until benchmark and compatibility data are
available.

## Testing strategy

### Unit tests

Test generation counters.

Test cache invalidation.

Test request timing aggregation.

Test transfer buffer behavior.

Test direct-copy fallback errors.

Test mount plan sorting.

Test retry classification.

Test registry dirty-record batching.

### Integration tests

Test cold and warm startup.

Test daemon restart with caches populated.

Test external registry mutation and refresh.

Test workspace restart and namespace descriptor invalidation.

Test host-backed and quota-backed workspaces.

Test large regular files.

Test large directory trees.

Test client disconnect during transfer.

Test cleanup during process death.

Test reflink fallback.

Test concurrent workspace startup.

### Security tests

Test archive traversal.

Test symlink races.

Test hard links and device entries.

Test destination no-overwrite behavior.

Test PID reuse.

Test stale namespace references.

Test privilege reset in persistent helpers.

Test policy cache invalidation.

Test path validation under concurrent rename.

### Stress tests

Run hundreds of create/start/stop/destroy cycles.

Run concurrent copies and status requests.

Run daemon restart during active transfers.

Run host reboot-like stale state recovery.

Run many workspaces in one sandbox.

Run many sandboxes with a large registry.

Monitor file descriptor counts, thread counts, and memory growth.

## Rollout phases

### Phase zero: instrumentation

Add phase timers.

Add process spawn counters.

Add registry lock timing.

Add transfer throughput metrics.

Add benchmark fixtures.

Record baseline results.

No behavior change should be part of this phase.

### Phase one: cheap repeated-work removal

Cache parsed policy.

Cache rootfs cache discovery.

Cache daemon registry metadata.

Avoid repeated mountinfo scans within one cleanup transaction.

Avoid repeated utility discovery.

Coalesce successful metadata writes.

### Phase two: lifecycle parallelism

Parallelize independent host-side workspace preparation.

Parallelize workspace stop cleanup with bounded workers.

Batch list, stats, and ps observations.

Separate long transfers from short daemon requests.

### Phase three: copy throughput

Tune buffers.

Remove duplicate traversal where safe.

Add regular-file direct-copy path.

Evaluate `copy_file_range`, `sendfile`, and `splice`.

Add optional gzip and progress only after the baseline path is fast.

### Phase four: process and namespace overhead

Replace measured hot-path shell probes.

Add namespace descriptor caching with identity checks.

Improve readiness signaling.

Prototype the persistent session helper.

Run security and crash tests before enabling it by default.

### Phase five: storage acceleration

Evaluate reflink rootfs cloning.

Evaluate reflink snapshots.

Evaluate filesystem-specific snapshots behind explicit configuration.

Retain portable fallbacks.

### Phase six: advanced kernel paths

Evaluate io_uring only if profiling justifies it.

Evaluate zero-copy network forwarding.

Evaluate daemon batching for high-frequency developer workflows.

Do not make advanced kernel features mandatory.

## Rollback and failure containment

Every optimization should be independently disableable.

Keep the old implementation available behind a tested fallback for at least one
release after introducing a risky fast path.

Log fast-path selection at debug level.

Record fallback reasons at debug level and actionable warning level when frequent.

If a fast path fails before mutation, use the safe fallback.

If a fast path fails after staging, clean the staging area and report the error.

Never fall back after a partial destination commit without verifying the destination
state.

Never hide a security validation failure behind a fallback.

## Prioritized implementation backlog

1. Add benchmark harness and phase timing.
2. Measure registry lock and persistence costs.
3. Add daemon registry cache with invalidation.
4. Cache policy and utility paths.
5. Parse mountinfo once per cleanup transaction.
6. Replace fixed readiness polling with a handshake.
7. Bound and parallelize independent workspace stop cleanup.
8. Parallelize safe workspace preparation during `up`.
9. Tune copy buffers using 5 GiB and many-file fixtures.
10. Add a single-file direct-copy fast path.
11. Add `copy_file_range` and buffered fallback.
12. Reduce duplicate directory traversal in copy.
13. Remove repeated `mountpoint` shell probes.
14. Add namespace descriptor cache with runtime identity checks.
15. Separate long transfers from short daemon requests.
16. Add rootfs cache indexing.
17. Add reflink detection and fallback.
18. Add snapshot reflink support.
19. Prototype persistent session helper.
20. Add performance regression thresholds to CI.

## Definition of done

The performance project is successful when:

- all existing commands still work;
- all existing command arguments still work;
- all isolation checks remain active;
- all cleanup and repair guarantees remain active;
- cold and warm baselines are recorded;
- warm workspace startup has a measured improvement;
- trivial exec latency has a measured improvement;
- large-file copy approaches local storage throughput;
- many-file copy avoids unnecessary duplicate traversal;
- listing commands scale with metadata rather than unrelated data;
- cleanup latency improves under stale-resource conditions;
- daemon short requests remain responsive during large transfers;
- memory and file descriptors remain bounded;
- fallback paths are tested;
- benchmarks run repeatably in CI or a documented performance environment;
- regressions produce actionable diagnostics.

## Final recommendation

Do not start by rewriting Enclave.

Start by measuring the command phases users actually wait for.

The architecture already has the right high-level shape for speed: a persistent
daemon, a namespace session helper, explicit workspace storage, separate control and
data paths, and clear lifecycle modules.

The immediate opportunity is to stop paying for the same work repeatedly.

Cache metadata in the daemon.

Batch writes.

Reuse stable runtime infrastructure with identity validation.

Make startup and cleanup dependency-aware and bounded-parallel.

Make large transfers stream with fewer traversals and fewer memory copies.

Use filesystem acceleration when available, but keep portable fallbacks.

Measure every change against cold start, warm start, command latency, throughput,
CPU, I/O, memory, and failure behavior.

If those principles are followed, Enclave can become substantially faster without
sacrificing its current command features or security model.
