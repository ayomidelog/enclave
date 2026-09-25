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
enclave daemon run    [--state-dir PATH] [--pid-file PATH] [--debootstrap-binary BIN] [--workspace-apparmor-profile PROFILE] [--workspace-selinux-label LABEL]
enclave daemon start  [--state-dir PATH] [--pid-file PATH] [--debootstrap-binary BIN] [--workspace-apparmor-profile PROFILE] [--workspace-selinux-label LABEL] [--wait-secs N]
enclave daemon status
enclave daemon stop
enclave ping
enclave health
enclave doctor [--repair]
```

| Command | Description |
|---------|-------------|
| `daemon run` | Run the daemon in the foreground. |
| `daemon start` | Start the daemon in the background and wait for it to be ready. |
| `daemon status` | Check if the daemon is running, and report the lifecycle operation running now and the last one that ran. |
| `daemon stop` | Stop the daemon. |
| `ping` | Send a ping to the daemon and print the response. |
| `health` | Print daemon health information (state dir, uptime, etc.). |
| `doctor` | Run diagnostic checks: registry consistency, orphaned mounts, stale cgroups, cgroup v2 availability, and published host ports no running workspace is using. |
| `doctor --repair` | Reconcile registry and filesystem state, remove stale workspace mounts and namespace state, and validate daemon ownership. Requires a running daemon unless global `--start-daemon` is supplied. |

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
enclave create  <name> [--suite bookworm] [--mirror URL] [--bootstrap-method debootstrap|cached_rootfs]
enclave start   <sandbox>
enclave stop    <sandbox>
enclave pause   <sandbox>
enclave resume  <sandbox>
enclave destroy <sandbox>
enclave list
enclave stats
enclave ps
enclave ps --local    # alias: --project
enclave status  <sandbox>
enclave remove  <sandbox-id>
enclave wipe
```

| Command | Description |
|---------|-------------|
| `create` | Bootstrap a new sandbox with `debootstrap` or `cached_rootfs`. |
| `start` | Start a stopped sandbox (mount rootfs). |
| `stop` | Stop all workspaces in the sandbox, then stop the sandbox. |
| `pause` | Freeze the sandbox cgroup while preserving workspace processes, namespaces, mounts, and storage for fast resume. |
| `resume` | Thaw a paused sandbox and best-effort restore its published ports. Port conflicts are reported without stopping workspaces. |
| `destroy` | Stop and permanently delete a sandbox and all its workspaces. Requires an already-running daemon unless `--start-daemon` is supplied. |
| `list` | List all sandboxes. |
| `stats` | Show live stats for all running workspaces across all sandboxes. |
| `ps` | Show live process status for all running workspaces across all sandboxes. |
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
enclave workspace create  <sandbox> <name> [--cpu-seconds N] [--memory-mb N] [--max-procs N] [--max-open-files N] [--disk-mb N]
enclave workspace resize  <sandbox> <workspace> --disk-mb N
enclave workspace cp      <sandbox> <workspace> <src> <dst>
enclave workspace start   <sandbox> <workspace>
enclave workspace stop    <sandbox> <workspace>
enclave workspace destroy <sandbox> <workspace>
enclave workspace list    [--sandbox-id <sandbox>]
enclave workspace status  <sandbox> <workspace>
enclave workspace remove  <sandbox> <workspace>
enclave workspace wipe
enclave workspace enter   <sandbox> <workspace> [--cwd /home] [--shell /bin/bash]
enclave workspace exec    <sandbox> <workspace> [--cwd /home] -- <command...>
enclave workspace run     <sandbox> <workspace> [--cwd /home] -- <command...>
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
| `create` | Create a new workspace inside a sandbox with optional resource limits. |
| `resize` | Increase the disk allocation of an Enclave-managed workspace. The target is an absolute size in MiB; host-backed workspace directories and decreases are not supported. |
| `cp` | Stream a file or directory between the host and a running workspace. Prefix the workspace side with `ws:/`; the unprefixed side is a host path. Transfers stage data before committing it, reject special files, and do not overwrite an existing destination entry. |
| `start` | Start a workspace session (namespaces + mounts). |
| `stop` | Stop a running workspace session. |
| `destroy` | Stop and permanently delete a workspace. Requires an already-running daemon unless `--start-daemon` is supplied. |
| `list` | List workspaces, optionally filtered by sandbox. |
| `status` | Show detailed status for a workspace (process count, resource usage). |
| `remove` | Remove a workspace entry from the registry. |
| `wipe` | Destroy all workspaces across all sandboxes. Requires confirmation and an already-running daemon unless `--start-daemon` is supplied. |
| `enter` | Enter a running workspace interactively (namespace handoff). |
| `exec` | Execute a one-shot command inside a workspace. |
| `run` | Run a command inside a workspace (alias for exec). |
| `port publish` | Persist and activate a loopback-only TCP port mapping for a workspace. |
| `port unpublish` | Remove a previously declared loopback-only TCP port mapping. |
| `port list` | Show declared and active published ports for a workspace. |
| `logs` | Show workspace session logs. `--follow` continuously streams appended log output. |
| `stats` | Show workspace resource metrics like CPU %, memory usage/limit, memory %, network I/O, block I/O, pids, and threads. |

### Resize workspace storage

`workspace resize` takes an absolute target size in MiB rather than a size delta:

```bash
enclave workspace resize mybox agent1 --disk-mb 2048
```

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
initially supports regular files and recursive directories. Permissions and
timestamps are preserved; archive ownership is not restored. Existing symlinked
destination components are rejected. The reported byte count is the logical
source/destination payload size, not a byte-for-byte pipe counter.

For directories, the destination follows standard `cp`-style behavior: an
existing destination directory receives an entry named after the source, while
a missing trailing-slash destination receives the source directory's contents.

The initial implementation intentionally does not include compression, progress
output, glob expansion, resumable transfers, or parallel directory transfer.

## Auth

```bash
enclave auth login  <provider>
enclave auth list
enclave auth logout <provider>
```

| Command | Description |
|---------|-------------|
| `auth login` | Read token from hidden stdin prompt and store at `<state_dir>/auth/<provider>.token` with mode `0600`. |
| `auth list` | List all supported providers and mark which ones currently have stored tokens. |
| `auth logout` | Delete stored provider token. |

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

## Registry

```bash
enclave registry repair [--strict]
```

| Command | Description |
|---------|-------------|
| `repair` | Scan and repair the registry. `--strict` removes entries with missing on-disk state. |

> **Destructive commands** (`wipe`, `workspace wipe`) require two-step confirmation before executing.
