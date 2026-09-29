# Changelog

## Unreleased

### Added

- A workspace can ask for a credential that is not one of the built-in
  providers. `env_tokens` now accepts any well-formed variable name, and the
  value comes from the store slot the name derives: `env_tokens =
  ["NETFLIX_PASSWORD"]` reads `<state_dir>/auth/users/<owner>/netflix-password.token`,
  which `enclave auth store --user <owner> --provider netflix-password` writes.
  A name that is one of the providers' variables still reads that provider's
  token, so a workspace that declares `env_tokens = ["GITHUB_TOKEN"]` resolves
  exactly as it did. A name that could never be injected — a lowercase one, one
  with a `-` in it, or one starting with a digit or `_` — is refused where it is
  declared rather than stored and silently never used.
- The store accepts any well-formed name, so a token can be held under a slot
  rather than only under a provider. `auth list` reports those slots by name and
  date in the shared namespace and alongside the providers in a user's, and
  `auth login` and `auth logout` name them the same way.

### Changed

- A workspace's credentials are re-resolved and rewritten before every command
  run through the daemon, so revoking a token takes effect on the next
  `workspace exec` instead of the next restart. The credentials the command was
  given are also what the scrubber and the audit use, so a command that resolved
  nothing records nothing.
- A stored name is either a provider or the slot an environment token derives,
  and `_` is a valid first character only: the derivation writes every other `_`
  of the variable name as `-`. A stored name with `_` anywhere else would be one
  no environment token can ask for, which is a token that is stored and never
  injected. Only `enclave`, `github`, and `npm` were ever storable before this
  release, so nothing that worked can stop working.

## 2.2.0 - 2026-09-29

### Fixed

- A wipe racing a workspace start no longer leaves a workspace directory on the
  host that no registry record describes. A launch writes the workspace's own
  record before it commits, and writing that record creates the directory it goes
  in, so a destroy that removed the record and the directory while the launch was
  in flight had its work undone behind it: the launch put the directory back,
  then refused to commit because the record it meant to update was gone, and its
  rollback released the cgroup, the network, and the mounts but not the files.
  The start's rollback now removes the directory when the record is gone. It
  leaves the directory alone whenever the record survives, so a start that fails
  for its own reason does not delete a healthy workspace's files, and a registry
  that cannot be read counts as registered. Measured on the race test, which runs
  the two operations against each other with one cleanup worker and twenty
  workspaces queued ahead of the target: 8 passes of 20 before, 20 of 20 after.

## 2.1.0 - 2026-09-29

This release adds an optional `owner` field to a workspace record and the token
layout it names. The field is additive and a registry written by 2.1.0 loads in
2.0.0, which ignores a field it does not know. What 2.0.0 cannot do is preserve
it: any lifecycle operation rewrites the registry, so a workspace that rolled
back to 2.0.0 loses its `owner` and resolves against the shared namespace again.
If you might roll back, keep the token in the shared namespace as well, or plan
to re-set the bindings.

### Added

- Auth tokens can be scoped to a user. A token lives either in the state
  directory's shared namespace at `<state_dir>/auth/<provider>.token`, which is
  what every workspace used before this existed, or in one user's namespace at
  `<state_dir>/auth/users/<user_id>/<provider>.token`. A workspace selects one
  with the new optional `owner` field, in the Enclavefile or in the workspace
  record. An owner with nothing stored gets nothing rather than falling back to
  the shared token, because the fallback would hand one user another user's
  credential; the start warns and continues, the way a missing provider token
  always has. A user id is limited to the URL-safe set — ASCII letters, digits,
  and `+`, `-`, `_`, `.` — because it becomes a single directory name.
- `enclave auth store --user <user_id> --provider <name> [--force]` provisions a
  token without an interactive prompt. The value is read from standard input and
  never from an argument, so it does not appear in the shell history or in the
  process list. An existing token is refused rather than replaced unless
  `--force` is given, and the outcome is reported in the exit status: `0` stored,
  `2` already stored, `3` an unusable provider or user id, `4` an I/O failure.
- `enclave auth list --user <user_id>` prints each stored provider and the date
  it was stored, never the value. `enclave auth logout --user <user_id>
  <provider>` removes one. Without `--user` both act on the shared namespace,
  which is what they did before.
- An append-only audit log at `<state_dir>/auth/audit.log` (mode `0600`), one
  JSON line per store, revoke, and inject, fsynced on write. Each event names the
  action, the namespace, the provider, and the workspace; there is no field for
  the token value, so a value cannot reach the log. An `inject` is recorded when
  a token enters a workspace at start, and again when a command runs with it,
  whether or not the value appeared in that command's output. The events of one
  operation are one append and one fsync rather than one each, which keeps the
  durable cost off the start path: a three-provider start pays for one write, not
  three.
- `workspace exec` replaces every injected token value in the captured output
  with `[REDACTED]` before returning it, so a command that prints a credential
  does not put it in a terminal, a log, a CI job, or a bug report. `--no-scrub`
  prints the output exactly as written, warns that it may contain a credential,
  and restores the direct streaming path for a command that reads standard input
  or writes a lot of output. An interactive `workspace enter` session is not
  captured and so is not scrubbed.

### Changed

- The `auth` commands take `--state-dir`. They act on a state directory rather
  than on a running daemon, and the directory was fixed to the invoking user's,
  which left no way to point them anywhere else.
- `workspace exec` runs the command through the daemon so its output can be
  scrubbed. It therefore buffers output and does not forward standard input;
  `--no-scrub` keeps the previous streaming behaviour, and `workspace enter` is
  unchanged.

## 2.0.0 - 2026-09-26

### Breaking

This release changes durable state and the daemon protocol, so it is a major
version rather than a minor one. Each of these is something an operator has to know
about before upgrading, and none of them is reversible by reinstalling the previous
binary alone.

- **The registry is versioned, and an upgrade is one way.** A registry written by a
  newer Enclave is refused with the version it declares rather than guessed at, so
  rolling back to 1.0.8 after this release has written state requires restoring the
  state directory from a backup. An older schema is migrated in memory on the read
  path and persisted by the next mutation, so a read-only command never rewrites the
  file.
- **Every daemon response carries an `operation_id`, and a failure carries a
  `code`.** A client that deserializes responses strictly has to accept the new
  field, and one that matched on error message text should switch to the code, which
  is stable across message changes. A request accepts an optional `operation_id` so
  a caller can correlate a retry with the attempt it repeats.
- **Workspace cgroups are named from the sandbox and workspace ids**
  (`enclave-ws-<sandbox>-<workspace>`) rather than from the runtime pid. Tooling that
  scraped the pid-derived path sees different names. The legacy cgroup is still found
  and removed, so an upgrade does not orphan one.
- **Bridge and NAT rules are comment-tagged and installed by generation.** The first
  start after the upgrade retires the previous generation's shared rules rather than
  stacking a second copy, so a host whose firewall is managed by other tooling should
  have Enclave's rules reviewed afterwards.
- **Every lifecycle wait is bounded.** An operation that used to wait indefinitely
  now fails on a deadline, and the failure names the variable that raises it. A host
  slower than the defaults has to raise the value; the whole table is in
  [Runtime details](docs/runtime-details.md).
- **`wipe` and `workspace wipe` fail when their confirmation cannot be read.** A
  command with no terminal used to print `aborted` and exit zero without deleting
  anything, so a script took the status for a completed teardown. It now exits
  non-zero and says nothing was deleted. Supplying the two answers on standard input
  is the supported way to script it.

### Added

- Resizing in both directions. `enclave workspace resize <sandbox> <workspace>` now
  takes `--memory-mb` as well as `--disk-mb`, and both can be raised or lowered. An
  omitted limit is left alone. Memory is a cgroup value, so changing it does not
  interrupt a running workspace; a disk change stops and restarts the workspace the
  way any image resize has to. Growing and shrinking are the same two steps in
  opposite order — a grow enlarges the image first, a shrink shrinks the filesystem
  first — and either way the result is verified from the ext4 superblock, so an image
  and a filesystem that disagree are reported rather than accepted. A shrink is
  refused before anything is written when the filesystem holds more data than the
  target, and the message names the smallest allocation that would work.
  `--no-memory-limit` removes a memory limit rather than setting one, spelled as a
  flag because a size of zero is refused as a size that cannot work.
  A memory change writes both layers the limit is enforced by, the cgroup and the
  session process's `RLIMIT_AS`, so a raise is usable without a restart; moving only
  the cgroup left a workspace capped at the limit it had started with. A memory limit
  can be changed on any workspace, including one whose storage Enclave does not manage
  and which therefore has no disk to resize.
- `enclave resize <sandbox>` for a sandbox's resource limits: `--memory-mb`,
  `--disk-mb`, and `--max-procs`, each optional and each left alone when omitted.
  Memory and process limits are applied to a running sandbox through its cgroup
  without a restart. A sandbox rootfs is a shared lower layer rather than an image, so
  its `--disk-mb` is a budget measured against the sum of its workspaces'
  allocations: a workspace cannot be created or grown past it, and a budget below what
  the workspaces already allocate is refused, naming the total. A sandbox can also
  declare the budget in its Enclavefile, and `enclave create` takes `--disk-mb`.
  `--no-memory-limit` and `--no-disk-budget` remove a limit instead of setting one.
- A lifecycle journal. Every lifecycle operation runs under one operation id, which
  is written to a durable record under `<state_dir>/operations/` with its target and
  current phase, returned in the daemon response, and printed by mutating commands. A
  daemon start closes the records a previous daemon left open and reconciles their
  targets, so an interrupted operation has one documented outcome rather than an
  inference.
- Machine-readable error codes on every daemon failure, so a client can tell a
  conflict from a missing resource, a rate limit, a policy denial, an unsupported
  host, a timeout, or an incomplete cleanup without matching on message text.
- A workspace cleanup certificate. `workspace stop` and `destroy` verify the host
  after cleanup — runtime pid and start time, cgroup, mounts, loop device, veth,
  firewall rules, namespace references, and files — and fail while anything is still
  held, so a recorded `stopped` is evidence rather than a claim. Destroy additionally
  proves the workspace's host resources are gone before the directory is removed.
- A resource inventory captured before a stop and compared afterwards, so the
  certificate answers for the resources the workspace actually owned.
- `doctor` covers nested workspace cgroups, workspace networking (veth interfaces,
  NAT rules, and anti-spoofing rules), loop devices, orphan runtimes whose metadata is
  gone, and the operation journal. It also reports every host capability the lifecycle
  depends on — overlayfs, idmapped mounts, cgroup v2 and its controllers, loop
  devices, the firewall, and netlink — in one check, naming what is lost when one is
  missing.
- `doctor --repair` resolves an interrupted lifecycle transition rather than only
  reporting it: it completes an interrupted stop, rolls back an interrupted start, and
  leaves a workspace whose metadata is gone but whose runtime is live in place, with
  the runtime named.
- Per-sandbox lifecycle leases, so conflicting lifecycle actions are queued or
  refused deterministically instead of racing, while unrelated sandboxes proceed in
  parallel.
- Configurable, bounded lifecycle deadlines for start, stop, helper startup, and
  command execution. The active value and its override are named when a wait times out
  and are reported in status.
- Stable workspace cgroup names derived from the sandbox and workspace ids, with the
  legacy pid-derived cgroup still cleaned up. Cleanup never targets a cgroup by pid
  name alone.
- Registry schema versioning with ordered migrations applied on the read path. An
  older schema is migrated before use, a future schema is refused rather than guessed
  at, and a registry that cannot be parsed is never silently read as empty.
- `sandbox status` names the base-image backend a sandbox is on, and `workspace
  status` names the storage tier and its lifecycle cost, so the tier in use is visible
  rather than implied.
- Pause and resume as the fast tier: a pause freezes the sandbox cgroup and withdraws
  the published ports, and a resume thaws it and restores them, keeping processes,
  memory, and mounts in place.
- Per-operation phase timings with `--verbose` or `ENCLAVE_PERF=1`, correlated with
  the operation id that produced them, and `tools/perf/phases.sh` to report the request
  p50/p95/p99 with each phase's share of it.
- A published lifecycle latency contract. `docs/lifecycle-report.md` carries the
  p50/p95/p99 of the release's benchmark together with the host it was measured on,
  and `tools/perf/lifecycle-report.sh` regenerates it.
- Per-tier live benchmarks, because a change to one tier is not a change to another:
  `live-pause.sh` for pause and resume, `live-quota.sh` for a workspace whose storage
  is an ext4 image, `live-ports.sh` for the published-port lifecycle including the
  rollback a failed publication owes, and `live-loaded.sh` for a sandbox whose twelve
  workspaces are all busy at once.
- `tools/ci/privileged-suite.sh`, which checks the host's tools and kernel
  capabilities before running the privileged lifecycle suite, and is now run in CI.

### Changed

- Workspace startup no longer spawns a shell. The in-namespace session setup runs
  in-process instead of as a script, removing the shell plus the `mount`, `hostname`,
  and `readlink` processes it launched, and the anti-spoofing rules are installed and
  deleted by one `iptables-restore` rather than one process per rule.
- A workspace's veth peer is created inside the workspace network namespace rather
  than created on the host and moved into it, which removes the move from every start.
- A cached rootfs is mounted as a shared immutable lower layer rather than copied per
  sandbox, so creating a sandbox from the cache does not traverse the tree. The cache
  index records a content digest at publication time, and a cache miss names the reason
  it was not reused.
- Workspace networking is configured with batched `ip` invocations, and the fixed
  route-readiness polling is gone: the command's own result is the readiness check.
- The registry lock is no longer held across host work. Start, stop, destroy, resize,
  and snapshot each release it while mounting, unmounting, setting up networking,
  waiting on processes, or copying data, and then commit with an identity check that
  refuses a result the workspace has moved past.
- A start's two intent records and a stop's two intent records are written together
  rather than one after the other, and a launched workspace's own record is written
  outside the lock. The namespace reference files and the journal's progress notes are
  written without `fsync`, which they do not need: they are re-derived from the live
  runtime, and a power loss takes the runtime with it.
- Every wait is event driven rather than polled: the daemon accept loop wakes on a
  shutdown pipe, session and helper readiness wait on a watch, runtime exit waits on
  pidfds, an idle published-port connection costs no CPU, and `workspace logs --follow`
  reads the delta from an offset and identifies the log by inode.
- A workspace runtime exits gracefully on a shutdown signal, and a stop terminates the
  process tree through cgroup v2 with a bounded graceful window before falling back to
  a signal.
- Destroy and wipe report per-resource outcomes. Normal mode fails while anything is
  still held so the records survive as recovery evidence, and force mode removes the
  records and reports exactly what it could not release.
- Bridge and NAT rules are comment-tagged and installed by generation, so an upgrade
  retires the previous generation's rules rather than stacking a second copy.
- `workspace cp` streams with a bounded buffer, cancels the transfer when the client
  disconnects, and leaves no partial target behind.

### Fixed

- A workspace start replaces its own leftover interface instead of building a second
  one beside it. A teardown that was forced, or a record that repair removed, can leave
  an interface on the bridge that no registry entry names any more. The allocator
  counted every bridge member's octet as taken, so the next start of that workspace was
  handed a different address, built `veth-<new octet>-<hash>`, and left the old
  interface holding an octet that nothing could release. The interface name carries a
  hash of the workspace id, which is what attributes it to the workspace it belongs to,
  so the allocator now excludes this daemon's own interfaces and reuses the address —
  which is what makes the start replace the leftover. Another daemon's workspaces are
  still counted, because their hashes are not in this registry.
- A destructive command whose confirmation prompt could not be read no longer exits
  zero. `wipe` and `workspace wipe` read the two answers from standard input, and a
  command run with no input read end of file and reported success without deleting
  anything, so a script took the exit status for a completed teardown. It now fails
  and says nothing was deleted, while an operator who answers the prompts with
  something other than the required phrases still aborts successfully.
- A stop no longer clears the record of a runtime that is still inside the `exec`
  that makes it the runtime. A process carries its launcher's command line until
  that exec finishes, and reads as having none at all while it is in progress, so a
  stop that arrived in that window called the record stale, reported the workspace
  stopped, and left the runtime running with its cgroup, its interface, and its
  mounts owned by nothing. A stop now waits a bounded
  `ENCLAVE_RUNTIME_EXEC_SETTLE_MS` for the command line to settle before it gives
  up on the record, which is what makes a stop of a workspace that was just started
  reliable on a loaded host.
- A stop no longer aborts when a process it was told about exits while it is being
  inspected. The liveness check and the read of the process's `/proc` entry are two
  separate moments, and a runtime that exited between them made the second read fail
  with `ENOENT`, which propagated out of the stop and released nothing: the runtime's
  cgroup, its mounts, and its interface all stayed behind while the command reported
  a failure. A read that finds the process gone is now the answer rather than an
  error, and one pid that cannot be read no longer leaves the rest of a batch
  unsignalled.
- The command lines a workspace runtime may have cover every step of it. The
  launcher, the in-namespace init, the bootstrap helper, and the session loop all
  keep the one pid the daemon recorded, and only the last two were recognized, so a
  stop that arrived while the runtime was still in its init step refused to signal
  it and cleared the record instead.
- A start that never commits no longer leaves a live session running. A launch that
  fails before its session reports ready, and a stop of a workspace left `starting`,
  both end the session the launch started, finding it by the pid file it wrote or by
  its own command line. Previously the session became a live runtime holding its
  namespaces, mounts, and private `/tmp` with nothing on the host naming it, because
  the record was rewritten as stopped.
- A wipe or destroy that raced a start no longer deletes the record of a live runtime;
  each workspace is resolved again and re-checked under the lock.
- A daemon restart no longer hands a paused sandbox's published ports back.
- Repair no longer deletes a directory a live create is still building, a sandbox or
  workspace another create owns, or a workspace whose metadata is gone but whose
  runtime is alive.
- Enclave never deletes an interface it did not create, and a leftover workspace
  interface is replaced by the next start rather than adopted.
- Cleanup refuses to unmount or delete through a mount Enclave did not create, and a
  failed unmount names the holding pid and its namespace.
- The workspace-private `/tmp` is a linked, writable directory. The unlinked-inode
  state that made every write under `/tmp` fail is fixed, doctor checks the mount, and
  a quota-backed workspace's `/tmp` lives inside its own image rather than in a fresh
  tmpfs.
- A failed disk resize leaves the image and its filesystem consistent instead of at
  the requested size with the old filesystem, and an image-creation failure removes the
  partial image.
- The operation journal is bounded rather than growing without limit.
- Cgroup removal retries while the kernel reports the cgroup is still draining, and
  reports the attempt count, the delay, and the final errno.
- A workspace whose record says `running` but whose runtime is dead has its host
  resources released before the record is cleared, so the interface, its rules, and its
  mounts are not left unowned.
- An interrupted transition's journal record is closed at the next daemon start.
- A daemon worker survives a request panic, and a poisoned port-publisher lock no
  longer stops the publisher from serving.
- The daemon state lock cannot be deleted out from under its owner, and a stale owner
  is detected by type rather than by matching an error message.
- Published-port connections are bounded per port and in total, so a connection flood
  is refused rather than exhausting threads, and stopping a workspace closes its
  listeners within the deadline.
- Snapshot retention keeps the newest snapshot when timestamps tie.
- A created workspace directory is claimed before its name is visible, so a concurrent
  repair does not treat a live create as a leftover.
- Registry, sandbox, and workspace metadata copies follow an explicit precedence rule
  instead of one silently overwriting another.
- A workspace start that would be handed an empty sandbox rootfs is refused, and a
  missing rootfs mount is repaired.
- Overflowing workspace memory and disk limits are rejected, and omitted Enclavefile
  limit fields no longer clear an existing workspace's allocations.
- Enclave refuses to build its bridge on top of a host that already uses the Enclave
  subnet, naming the conflicting interfaces before any host state changes.

## 1.0.8 - 2026-09-15

### Added
- `enclave pause` and `enclave resume` for freezing and thawing a sandbox cgroup without destroying workspace processes, namespaces, or mounts.
- A bounded `workspace.start_many` lifecycle operation for bulk Enclavefile startup, plus the reproducible cached-rootfs live lifecycle benchmark at `tools/perf/live-lifecycle.sh`.

### Changed
- Enclavefile `run` commands launch asynchronously inside the target workspace cgroup, so long-running services no longer serialize `enclave up`.
- Workspace stop now uses cgroup v2 process-tree termination, bounded cleanup fan-out, one mountinfo snapshot, and overlapped network teardown.
- Workspace pivot-root paths are unique per workspace, allowing reliable multi-workspace startup without shared `/.old_root` collisions.

### Fixed
- Detached workspace commands are attached to their cgroup before namespace-command forking, preventing resource-limit and shutdown escapes.
- Persistent workspace helper startup tolerates transient socket races and active helper binaries are never replaced.
- Pause failures preserve published ports, and omitted Enclavefile limit fields no longer clear existing workspace allocations during bulk reconciliation.

- Quota-backed workspaces now use a per-workspace root OverlayFS upper/work layer on the same ext4 image as `/home` and workspace-private `/tmp`, so writes under `/opt`, `/var`, `/etc`, and `/root` consume the configured `disk_mb` allocation without modifying the shared sandbox lower rootfs.
- Managed workspace `/tmp` can be cleared on restart with the opt-in Enclavefile setting `clear_tmp_on_restart = true`.

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
