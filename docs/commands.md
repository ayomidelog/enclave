# Command Reference

## Lifecycle responses

Every command that changes lifecycle state (`create`, `start`, `stop`, `pause`,
`resume`, `destroy`, `remove`, and the `workspace` equivalents) prints two things
beyond its own result: the operation id the daemon ran the request as, and the
state change it made.

```
$ enclave workspace stop mybox dev
stopped workspace
state: running -> stopped
operation 8bd51a0a-cb7d-4ee2-b006-af17108dc01a
cleanup verified: runtime exited, cgroup removed, mounts released, loop device detached, runtime files removed, network and ports released
```

The operation id ties the command to its journal record under
`<state-dir>/operations/`, to its log lines, and to its phase timings, so a
single run can be traced end to end. The transition is the daemon's own answer to
"what did this change": a stop that found the workspace already stopped prints no
transition line, because it changed nothing.


## Daemon

```bash
enclave daemon run    [--state-dir PATH] [--pid-file PATH] [--debootstrap-binary BIN] [--workspace-apparmor-profile PROFILE] [--workspace-selinux-label LABEL] [--idle-stop-after-secs N] [--destroy-after-secs N] [--max-running-workspaces N] [--default-workspace-cpu-percent N] [--default-workspace-max-processes N] [--sweep-interval-secs N] [--sweep-enabled BOOL] [--evict-idle BOOL] [--task-count-threshold N] [--detached-process-threshold N]
enclave daemon start  [--state-dir PATH] [--pid-file PATH] [--debootstrap-binary BIN] [--workspace-apparmor-profile PROFILE] [--workspace-selinux-label LABEL] [--wait-secs N] [--idle-stop-after-secs N] [--destroy-after-secs N] [--max-running-workspaces N] [--default-workspace-cpu-percent N] [--default-workspace-max-processes N] [--sweep-interval-secs N] [--sweep-enabled BOOL] [--evict-idle BOOL] [--task-count-threshold N] [--detached-process-threshold N]
enclave daemon status
enclave daemon stop
enclave ping
enclave health
enclave doctor [--repair]
```

| Command | Description |
|---------|-------------|
| `daemon run` | Run the daemon in the foreground. The lifecycle flags set the sweep interval, idle and retention windows, daemon-wide running-workspace cap, default workspace CPU/process limits, and doctor thresholds. |
| `daemon start` | Start the daemon in the background and wait for it to be ready. It accepts the same lifecycle policy flags as `daemon run`. |
| `daemon status` | Check if the daemon is running, and report the lifecycle operation running now and the last one that ran. |
| `daemon stop` | Stop the daemon. |
| `ping` | Send a ping to the daemon and print the response. |
| `health` | Print daemon health information (state dir, uptime, etc.). |
| `doctor` | Run diagnostic checks: registry consistency, orphaned and nested workspace cgroups, orphaned mounts and sandbox rootfs mounts, workspace networking (veth interfaces, NAT rules, and anti-spoofing rules), loop devices backing workspace images, orphan runtimes whose metadata is gone, the operation journal, host capabilities, published host ports no running workspace is using, idle workspaces, task-count pressure, and tracked detached-process pressure. Mounts and interfaces Enclave did not create are reported separately and left in place. |
| `doctor --repair` | Reconcile registry and filesystem state, resolve an interrupted lifecycle transition (complete an interrupted stop, roll back an interrupted start), remove stale workspace mounts and namespace state, retire firewall rules for interfaces that are gone, stop idle workspaces through the verified stop path, and validate daemon ownership. Only mounts and rules Enclave created are released; anything it cannot attribute to itself is reported and left alone. Requires a running daemon unless global `--start-daemon` is supplied. |

Destructive commands do not start a stopped daemon automatically. Start it with `enclave daemon start`, or opt in for one invocation with the global `--start-daemon` flag:

```bash
enclave daemon start
enclave --start-daemon workspace destroy mybox agent1
enclave --start-daemon doctor --repair
```

## Enclavefile Lifecycle

```bash
enclave init
enclave up        [--rebuild]
enclave down
enclave restart   [--rebuild]
enclave rootfs export [--state-dir PATH] (--suite SUITE | --base) --output ARCHIVE
enclave rootfs import [--state-dir PATH] (--suite SUITE | --base) [--replace] ARCHIVE
enclave rootfs fetch  [--state-dir PATH] (--suite SUITE | --base) [--replace] URL
```

| Command | Description |
|---------|-------------|
| `init` | Scaffold a blank `Enclavefile` in the current directory. |
| `up` | Read the Enclavefile, create sandbox, run setup, create and start all workspaces, and launch configured `run` commands detached inside each workspace cgroup. |
| `up --rebuild` | Force sandbox recreation and rerun setup commands. |
| `down` | Stop all workspaces in the sandbox as a coordinated batch, then stop the sandbox. |
| `restart` | Stop and restart the entire environment. |
| `restart --rebuild` | Rebuild the sandbox from scratch on restart. |

## Rootfs Cache

```bash
enclave rootfs export [--state-dir PATH] (--suite SUITE | --base) --output ARCHIVE
enclave rootfs import [--state-dir PATH] (--suite SUITE | --base) [--replace] ARCHIVE
enclave rootfs fetch  [--state-dir PATH] (--suite SUITE | --base) [--replace] URL
```

| Command | Description |
|---------|-------------|
| `rootfs export` | Package an existing cached rootfs into a tar or tar.gz archive suitable for sharing or publishing. |
| `rootfs import` | Import a rootfs archive into Enclave's cached-rootfs store for later `cached_rootfs` startup. |
| `rootfs fetch` | Download a published rootfs archive with `curl` and import it directly into the cache. |

Example published asset:

```bash
enclave rootfs fetch --suite bookworm https://github.com/ayomidelog/enclave/releases/download/rootfs-bookworm-2026-06-07/bookworm-rootfs-clean-2026-06-07.tar.gz
```

## Sandbox

```bash
enclave create  <name> [--suite bookworm] [--mirror URL] [--bootstrap-method debootstrap|cached_rootfs] [--memory-mb N] [--cpu-percent N] [--max-procs N] [--disk-mb N] [--max-running-workspaces N]
enclave start   <sandbox>
enclave stop    <sandbox>
enclave pause   <sandbox>
enclave resume  <sandbox>
enclave destroy <sandbox>
enclave list
enclave stats
enclave ps [--processes]
enclave ps --local    # alias: --project
enclave resize  <sandbox> [--memory-mb N | --no-memory-limit] [--disk-mb N | --no-disk-budget] [--max-procs N] [--max-running-workspaces N | --no-max-running-workspaces]
enclave status  <sandbox>
enclave remove  <sandbox-id>
enclave wipe
```

| Command | Description |
|---------|-------------|
| `create` | Bootstrap a new sandbox with `debootstrap` or `cached_rootfs`. `--max-running-workspaces N` sets a per-sandbox cap on runtimes that are `starting` or `running`. |
| `start` | Start a stopped sandbox (mount rootfs). |
| `stop` | Stop all workspaces in the sandbox, then stop the sandbox. |
| `pause` | Freeze the sandbox cgroup while preserving workspace processes, namespaces, mounts, and storage for fast resume. |
| `resume` | Thaw a paused sandbox and best-effort restore its published ports. Port conflicts are reported without stopping workspaces. |
| `resize` | Change a sandbox's resource limits. Each target is optional and an omitted one is left alone. Memory and process limits are cgroup values and are applied to a running sandbox without restarting it. `--disk-mb` sets the total disk budget the sandbox's workspaces are measured against, since a sandbox rootfs is a shared lower layer rather than an image of its own; a budget below what the workspaces already allocate is refused, naming the total. `--max-running-workspaces` changes the sandbox runtime cap. `--no-memory-limit`, `--no-disk-budget`, and `--no-max-running-workspaces` remove a limit rather than setting one, and are spelled as flags because a size of zero is a mistake rather than a request to remove anything. |
| `destroy` | Stop and permanently delete a sandbox and all its workspaces. Requires an already-running daemon unless `--start-daemon` is supplied. |
| `list` | List all sandboxes. |
| `stats` | Show live stats for all running workspaces across all sandboxes. |
| `ps` | Show live process status for all running workspaces across all sandboxes. `--processes` includes a row for every process in each workspace: PID, PPID, PGID, command, age, CPU sample, RSS, thread count, detached state, and tracked job id. |
| `ps --local` (`--project`) | Show only workspaces defined by the Enclavefile in the current directory. |
| `status` | Show detailed status for a sandbox. |
| `remove` | Remove a sandbox entry from the registry (does not delete files). |
| `wipe` | Destroy all sandboxes. Requires confirmation and an already-running daemon unless `--start-daemon` is supplied. |

## Lifecycle Tiers

The lifecycle commands are not interchangeable, and their costs differ by an order
of magnitude. Each row states what survives the command and what has to be rebuilt.

| Command | Processes | Mounts | Memory | Published ports | Rebuild cost |
|---------|-----------|--------|--------|-----------------|--------------|
| `pause` / `resume` | preserved | preserved | preserved | withdrawn on pause, restored on resume | near zero |
| `workspace stop` / `start` | rebuilt | rebuilt | lost | withdrawn on stop, restored on start | full workspace start |
| `workspace create` / `destroy` | n/a | n/a | n/a | n/a | filesystem plus start |
| `sandbox stop` / `start` | every workspace rebuilt | rebuilt | lost | withdrawn | every workspace start |
| `down` / `up` | rebuilt | rebuilt | lost | restored | sandbox plus every workspace |
| `up --rebuild` | rebuilt | rebuilt | lost | restored | rootfs bootstrap plus setup |

`pause` is the fast path: it freezes the sandbox cgroup in place, so nothing is
torn down or recreated. `stop` releases every host resource the workspace owned
and reports a verified cleanup certificate, so it is the right choice when a
workspace must not keep holding memory, mounts, or a loop device.

### Measured latencies

Medians of eight runs of each command against a cached 127 MB Debian bookworm
rootfs (5237 files), one sandbox and one workspace, on the validation host:

| Command | Median | Range |
|---------|--------|-------|
| `create` (sandbox, cached rootfs) | `0.11s` | 0.08–0.17s |
| `workspace create` | `0.23s` | 0.20–0.76s |
| `workspace start` | `0.19s` | 0.17–0.25s |
| `workspace stop` | `0.12s` | 0.10–0.14s |
| `workspace destroy` | `0.04s` | 0.03–0.06s |
| `pause` | `0.12s` | 0.10–0.17s |
| `resume` | `0.11s` | 0.10–0.13s |
| `stop` (sandbox) | `0.21s` | 0.19–0.30s |
| `start` (sandbox) | `0.22s` | 0.20–0.24s |

Two things are worth reading off this table. A sandbox create is fast because a
cached rootfs is mounted as a shared base layer rather than copied; the 127 MB
tree is never written per sandbox. And `pause` is roughly half of `stop` on the
way out and `resume` roughly half of `start` on the way back, because neither
tears down or rebuilds the runtime, the mounts, or the filesystem. The absolute
gap is a tenth of a second here; what `pause` preserves is the process state,
which is what makes the difference matter for a workspace that took real work to
get into its current state.

These are single-workspace numbers. For the eight-workspace aggregate see the
lifecycle timing section in [runtime-details.md](runtime-details.md).

## Operation IDs

Every daemon request runs as one operation with one id. Mutating commands print it
on success:

```console
$ enclave workspace start mybox agent1
started workspace
operation 8f0c3a2e-6d1b-4c9a-9f4e-2b7d5a1c8e30
```

A failing command names it in the error, so a failure can be traced without
reproducing it:

```console
$ enclave workspace start mybox missing
error: workspace 'missing' not found in sandbox 'mybox-1a2b3c4d5e6f' (operation 7c6b...)
```

The same id names the lifecycle journal record under `<state_dir>/operations/`,
the `lifecycle operation started` and `request failed` log lines, and the phase
timings emitted with `--verbose` or `ENCLAVE_PERF=1`. `enclave daemon status`
prints the operation running now and the last one that ran.

Read-only requests such as `workspace status` are not journaled and print no id.

## Workspace

```bash
enclave workspace create  <sandbox> <name> [--cpu-seconds N] [--cpu-percent N] [--memory-mb N] [--max-procs N] [--max-open-files N] [--disk-mb N]
enclave workspace resize  <sandbox> <workspace> [--disk-mb N] [--memory-mb N | --no-memory-limit]
enclave workspace cp      <sandbox> <workspace> <src> <dst> [--progress] [--gzip]
enclave workspace start   <sandbox> <workspace>
enclave workspace stop    <sandbox> <workspace>
enclave workspace destroy <sandbox> <workspace>
enclave workspace list    [--sandbox-id <sandbox>]
enclave workspace status  <sandbox> <workspace>
enclave workspace remove  <sandbox> <workspace>
enclave workspace wipe
enclave workspace enter   <sandbox> <workspace> [--cwd /home] [--shell /bin/bash]
enclave workspace exec    <sandbox> <workspace> [--cwd /home] [--no-scrub] [--background] [--ttl-secs N] [--keep-running] -- <command...>
enclave workspace cancel  <sandbox> <workspace>
enclave workspace jobs    <sandbox> <workspace> [--job ID]
enclave workspace keep    <sandbox> <workspace> (--keep-running | --no-keep-running | --idle-stop-after-secs N | --destroy-after-secs N)
enclave workspace run     <sandbox> <workspace> [--cwd /home] [--no-scrub] [--background] [--ttl-secs N] [--keep-running] -- <command...>
enclave workspace port publish   <sandbox> <workspace> <127.0.0.1:HOST_PORT:WORKSPACE_PORT[/tcp]>
enclave workspace port unpublish <sandbox> <workspace> <127.0.0.1:HOST_PORT[/tcp]>
enclave workspace port list      <sandbox> <workspace>
enclave workspace logs    <sandbox> <workspace> [--tail N] [--follow]
enclave workspace logs    <workspace> [--tail N] [--follow]
enclave workspace stats   <sandbox> <workspace>
enclave workspace stats   <workspace>
```

| Command | Description |
|---------|-------------|
| `create` | Create a new workspace inside a sandbox with optional CPU, memory, process, open-file, and disk limits. |
| `resize` | Change a workspace's disk allocation, its memory limit, or both. Each target is an absolute size in MiB and an omitted one is left alone. The disk can be grown or shrunk; shrinking is refused when the filesystem holds more data than the target, and the message names the smallest allocation that would work. A disk change stops and restarts a running workspace; a memory change is applied to the running runtime through its cgroup without interrupting it. Host-backed `workspace_dir`/`path` workspaces have no managed disk to resize. |
| `cp` | Stream a file or directory between the host and a running workspace. Prefix the workspace side with `ws:/`; the unprefixed side is a host path. Transfers stage data before committing it, reject special files, and do not overwrite an existing destination entry. |
| `start` | Start a workspace session (namespaces + mounts). |
| `stop` | Stop a running workspace session. |
| `destroy` | Stop and permanently delete a workspace. Requires an already-running daemon unless `--start-daemon` is supplied. |
| `list` | List workspaces, optionally filtered by sandbox. |
| `status` | Show detailed status for a workspace: process count, resource usage, and the storage tier it is on with the lifecycle cost that tier implies (a directory-backed workspace, or a quota-backed one whose `/home`, private `/tmp`, and root overlay live on a loop-mounted ext4 image). |
| `remove` | Remove a workspace entry from the registry. |
| `wipe` | Destroy all workspaces across all sandboxes. Requires confirmation and an already-running daemon unless `--start-daemon` is supplied. |
| `enter` | Enter a running workspace interactively (namespace handoff). The session's output is not captured, so it is not scrubbed of injected token values. |
| `exec` | Execute a one-shot command inside a workspace. The daemon captures the output and replaces every injected token value with `[REDACTED]`, so a command that prints a credential does not put it in your terminal, your log, or your shell history. `--no-scrub` prints the output exactly as written and warns that it may contain a credential; it also restores the direct streaming path, which is what you want for a command that reads stdin or writes a lot of output. `--background` returns immediately and keeps a durable job; `--ttl-secs N` limits its lifetime, and `--keep-running` pins the workspace while it runs. |
| `cancel` | Stop the command this workspace is running now by signalling the verified persistent-helper process group. It also reaches descendants that inherited the group. Exits successfully when no command was running, and reports whether anything was actually stopped. |
| `jobs` | List durable foreground and background job records for a workspace. Each record carries the job id, command, mode, start and finish time, PID/start-time identity, process group, status, exit code, TTL, and keep-running policy. |
| `keep` | Change only the lifecycle fields named on the command line. `--keep-running` pins the workspace against idle stop and destroy-after retention deletion, `--no-keep-running` removes the pin, and the idle/destroy window flags update their window without changing the pin. At least one is required. |
| `run` | Run a command inside a workspace (alias for exec). |
| `port publish` | Persist and activate a loopback-only TCP port mapping for a workspace. |
| `port unpublish` | Remove a previously declared loopback-only TCP port mapping. |
| `port list` | Show declared and active published ports for a workspace. |
| `logs` | Show workspace session logs. `--follow` continuously streams appended log output. |
| `stats` | Show workspace resource metrics including CPU, memory usage and limit, network I/O, block I/O, `process_count` (processes in the workspace PID namespace), and `task_count` (cgroup tasks, including threads). Without cgroup v2, `task_count` reports the PID-namespace process count as its fallback value. |

`workspace exec` creates a job for every command. A foreground job holds a command
lease for the whole request, so the idle sweeper cannot stop its workspace. A
background job is returned immediately and remains tracked after the CLI exits:
use `--background`, and optionally `--ttl-secs N` to stop it after a deadline or
`--keep-running` to keep the workspace alive while it runs.
An ordinary background job remains tracked but does not pin the workspace; only
`--keep-running` does. It is stopped by the idle policy unless the workspace is
otherwise active or the job has an explicit TTL.

`workspace jobs` reports both durable records and their terminal outcomes. The
job identity is the host helper process's pid and start time; a detached helper is
also the leader of its own process group, which is what the TTL reaper signals.

A foreground command terminates descendants that remain in the helper's process
group when the direct child exits. Use `--background` to keep work after the
request returns. `workspace cancel` is the recovery path for a foreground command
that is still running; it verifies the helper identity before sending `SIGKILL`.

`enclave ps --processes` adds one row per process in each workspace, including
its PID, PPID, PGID, age, CPU sample, RSS, thread count, detached state, job id, and
command. A detached row means the process belongs to a tracked background job;
ordinary service children are not mislabeled as detached.

### Resize a workspace

`workspace resize` takes an absolute target size in MiB rather than a size delta, and
either limit on its own:

```bash
# Memory is a cgroup value, so the running workspace is not interrupted.
enclave workspace resize mybox agent1 --memory-mb 2048

# A disk change stops and restarts the workspace, because an image cannot be resized
# while it is mounted.
enclave workspace resize mybox agent1 --disk-mb 2048

# Both together is one stop, not two.
enclave workspace resize mybox agent1 --disk-mb 2048 --memory-mb 2048

# Remove a limit rather than setting one.
enclave workspace resize mybox agent1 --no-memory-limit
```

Both limits can be raised or lowered. A disk allocation is refused when the
filesystem holds more data than the target, and the message names the smallest
allocation that would work; host-backed `workspace_dir`/`path` workspaces have no
managed disk to resize, but their memory limit can still be changed. Every reason a
request could be refused is checked before a running workspace is stopped for it, so a
refused resize leaves it running.

### Resize a sandbox

A sandbox has no image of its own: its rootfs is a shared lower layer on the host
filesystem, so what it allocates is the sum of its workspaces' quota images. `resize`
therefore takes the sandbox's own limits, and `--disk-mb` sets the budget those
workspace allocations are measured against:

```bash
enclave resize mybox --memory-mb 8192
enclave resize mybox --disk-mb 32768
enclave resize mybox --max-procs 512
enclave resize mybox --max-running-workspaces 4
enclave resize mybox --no-disk-budget
enclave resize mybox --no-max-running-workspaces
```

Memory and process limits are cgroup values and are applied to a running sandbox
without restarting it. A disk budget is enforced where an allocation is granted, so a
workspace cannot be created or grown past it, and a budget below what the sandbox's
workspaces already allocate is refused rather than stored.

### Copy files with a workspace

`workspace cp` requires exactly one `ws:/` path. Host paths are resolved from
the current directory; workspace paths are absolute paths inside the workspace.
The workspace must already be running. Enclave stages each transfer and only
commits it after both sides succeed, so an existing destination entry is rejected
rather than partially merged or overwritten.

```bash
enclave workspace cp mybox agent1 ./myfile.py ws:/home/myfile.py
enclave workspace cp mybox agent1 ws:/home/output.txt ./output.txt
enclave workspace cp mybox agent1 ./project/ ws:/home/project/
```

Transfers stream `tar` directly across the workspace namespace boundary. It
supports regular files and recursive directories. Permissions and timestamps are
preserved; archive ownership is not restored. Existing symlinked destination
components are rejected. The reported byte count is the logical
source/destination payload size, not a byte-for-byte pipe counter.

`--gzip` compresses a directory transfer, and `--progress` reports progress on
stderr. The JSON control response and normal stdout are unchanged by either.

For directories, the destination follows standard `cp`-style behavior: an
existing destination directory receives an entry named after the source, while
a missing trailing-slash destination receives the source directory's contents.

Glob expansion, resumable transfers, and parallel directory transfer are not
supported.

## Auth

```bash
enclave auth login  [--state-dir PATH] [--user <user_id>] <name>
enclave auth store  [--state-dir PATH] --user <user_id> --provider <name> [--force]
enclave auth list   [--state-dir PATH] [--user <user_id>]
enclave auth logout [--state-dir PATH] [--user <user_id>] <name>
```

| Command | Description |
|---------|-------------|
| `auth login` | Read a token from a hidden stdin prompt and store it. |
| `auth store` | Read a token from stdin without prompting. For scripts. |
| `auth list` | List stored names and when each was stored. Never the values. |
| `auth logout` | Delete a stored token. |

These commands act on a state directory rather than on a running daemon, so
`--state-dir` names the one to use and defaults to the invoking user's. They
require root, like every command that touches host state.

### Token namespaces

A token belongs to a namespace. A workspace selects one with `owner`, and a
workspace with no `owner` uses the shared namespace it always did.

```text
<state_dir>/auth/<name>.token                   the shared namespace
<state_dir>/auth/users/<user_id>/<name>.token   one user's namespace
```

`--user` selects a namespace on `login`, `store`, `list`, and `logout`. Without
it they act on the shared one. A user id may contain ASCII letters, digits, and
`+`, `-`, `_`, `.`, because it becomes a single directory name; anything else is
rejected.

### Token names

The name a token is stored under is either one of the providers (`enclave`,
`github`, `npm`) or the slot an environment token reads from. A workspace reaches
the second kind by declaring the variable it wants, and the slot is that variable
lowercased with `_` written as `-`:

```bash
printf '%s' "$NETFLIX_PASSWORD" | \
  enclave auth store --user alice --provider netflix-password
```

```toml
[workspace.vault]
name = "vault"
owner = "alice"
env_tokens = ["NETFLIX_PASSWORD"]
```

A name is one file name component, so it may only contain lowercase ASCII
letters, digits, and `-`, plus `_` as the first character. `_` is allowed there
because the slot for an environment token is its name lowercased with `_` written
as `-`, and a leading `_` is kept rather than written as `-` so that a slot never
begins with `-`; a name with `_` anywhere else is one no environment token could
ever ask for. `enclave auth list` prints stored slots under `Stored environment
token slots:` in the shared namespace, and alongside the providers in a user's.

### Storing a token from a script

The token is read from standard input, never from an argument, so it does not
appear in the shell history or in the process list:

```bash
printf '%s' "$TOKEN" | enclave auth store --user alice --provider github
```

An existing token is refused rather than replaced, so a re-run cannot silently
change a credential out from under a running workspace. Pass `--force` to replace
it. The outcome is reported in the exit status:

| Status | Meaning |
|--------|---------|
| `0` | Stored. |
| `2` | A token is already stored; pass `--force` to replace it. |
| `3` | The token name or the user id cannot be used, or the token read from stdin is empty. |
| `4` | The token could not be read or written. |

### Moving a token into a namespace

An existing `<state_dir>/auth/<provider>.token` is untouched and keeps working
for workspaces with no `owner`. To move it into a user's namespace, store it there
and then remove the shared copy:

```bash
printf '%s' "$TOKEN" | enclave auth store --user alice --provider github
enclave auth logout <provider>          # removes the shared copy
```

### The audit log

Every store, revoke, and inject appends one JSON line to
`<state_dir>/auth/audit.log` (mode `0600`). The event names the action, the
namespace, the provider, and the workspace; it never contains the token value, so
the log is safe to read and safe to ship somewhere else.

```json
{"ts":"2026-09-29T13:10:00Z","action":"inject","user":"alice","provider":"github","sandbox":"devbox-1a2b","workspace":"api-9c8d"}
```

### Supported providers

- `enclave` → `ENCLAVE_TOKEN`
- `github` → `GITHUB_TOKEN`, `GH_TOKEN`, and Git HTTPS auth via environment-provided `credential.helper` config
- `npm` → `NPM_TOKEN`

For GitHub-authenticated workspaces (`auth = ["github"]`), both `gh` and HTTPS Git operations such as `git clone https://github.com/...` and `git push` work without manual login.

### Resource Limits

When creating a sandbox, you can set aggregate sandbox resource limits:

| Flag | Description |
|------|-------------|
| `--cpu-percent N` | Maximum steady CPU share as a percentage of total machine CPU capacity. |
| `--memory-mb N` | Maximum aggregate memory for all workspace processes in the sandbox (`memory.max`, cgroup v2). |
| `--max-procs N` | Maximum aggregate process count for the sandbox (`pids.max`, cgroup v2). |
| `--disk-mb N` | Total disk budget for the sandbox's workspaces. A sandbox rootfs is a shared lower layer rather than an image, so this caps the sum of its workspaces' `disk_mb` allocations and is enforced when a workspace is created or grown. Change it later with `enclave resize`. |

When creating a workspace, you can set per-workspace resource limits:

| Flag | Description |
|------|-------------|
| `--cpu-seconds N` | Maximum CPU time in seconds (`RLIMIT_CPU`). |
| `--cpu-percent N` | Maximum steady CPU share as a percentage of total machine CPU capacity (`cpu.max`, cgroup v2). |
| `--memory-mb N` | Maximum virtual memory in megabytes (`RLIMIT_AS`). |
| `--max-procs N` | Maximum number of processes (`RLIMIT_NPROC` via `prlimit`). |
| `--max-open-files N` | Maximum number of open file descriptors (`RLIMIT_NOFILE`). |
| `--disk-mb N` | Maximum disk space for Enclave-managed workspace storage, including `/home`, workspace-private `/tmp`, and root OverlayFS writes such as `/opt`, `/var`, `/etc`, and `/root`. Not supported with host-mounted `workspace_dir` / `path`. |

The Enclavefile-only `clear_tmp_on_restart = true` setting clears a managed workspace's `/tmp` after a successful workspace or sandbox stop. It is disabled by default and does not affect host-mounted workspace directories.

Sandbox limits are aggregate caps across all running workspaces in that sandbox. Workspace limits apply to the individual workspace process tree.

## Snapshots

```bash
enclave snapshot create  <sandbox> <workspace> [--name snapshot-name]
enclave snapshot list    <sandbox> <workspace>
enclave snapshot restore <sandbox> <workspace> <snapshot-name>
enclave snapshot export  <sandbox> <workspace> <snapshot-name> --output ARCHIVE
enclave snapshot import  <sandbox> <workspace> [--name snapshot-name] [--replace] ARCHIVE

# Legacy workspace aliases and maintenance
enclave workspace snapshot      <sandbox> <workspace> [--name snapshot-name]
enclave workspace snapshot-list <sandbox> <workspace>
enclave workspace restore       <sandbox> <workspace> <snapshot-name>
enclave workspace snapshot-gc   <sandbox> <workspace> [--keep N]
```

| Command | Description |
|---------|-------------|
| `snapshot create` | Create a point-in-time copy of a workspace's filesystem. |
| `snapshot list` | List all snapshots for a workspace. |
| `snapshot restore` | Restore a workspace to a previous snapshot. |
| `snapshot export` | Package an existing workspace snapshot as a portable tar or tar.gz archive. |
| `snapshot import` | Import a portable snapshot archive into a workspace snapshot slot. |
| `workspace snapshot-gc` | Delete old snapshots, keeping the most recent N (default: 5). |

## Policy

```bash
enclave policy show
enclave policy default <allow|deny>
enclave policy allow   [--uid UID] <action-pattern>
enclave policy deny    [--uid UID] <action-pattern>
enclave policy clear   [--uid UID]
```

| Command | Description |
|---------|-------------|
| `show` | Display the current policy rules. |
| `default` | Set the default policy to allow or deny. |
| `allow` | Add an allow rule for an action pattern (optionally per-UID). |
| `deny` | Add a deny rule for an action pattern (optionally per-UID). |
| `clear` | Remove all rules (optionally per-UID). |

`show` reads the policy; the other four change it. Those four are restricted to root whatever the rules say, because authorizing them against the policy they edit would let a rule that allowed `policy.allow` grant its holder every other action.

## Registry

```bash
enclave registry repair [--strict]
```

| Command | Description |
|---------|-------------|
| `repair` | Scan and repair the registry. `--strict` removes entries with missing on-disk state. |

 > **Destructive commands** (`wipe`, `workspace wipe`) require two-step confirmation before executing.

The confirmation is read from standard input, so a destructive command with no
terminal fails rather than doing nothing: it exits non-zero and reports that nothing
was deleted. Answering a prompt with anything other than the required phrase aborts
the command and exits zero. To run one from a script, supply the answers on standard
input:

```bash
printf 'y\ndelete all sandboxes\n' | enclave wipe --force
```
