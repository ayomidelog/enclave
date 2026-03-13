# Command Reference

## Daemon

```bash
enclave daemon run    [--state-dir PATH] [--pid-file PATH] [--debootstrap-binary BIN] [--workspace-apparmor-profile PROFILE] [--workspace-selinux-label LABEL]
enclave daemon start  [--state-dir PATH] [--pid-file PATH] [--debootstrap-binary BIN] [--workspace-apparmor-profile PROFILE] [--workspace-selinux-label LABEL] [--wait-secs N]
enclave daemon status
enclave daemon stop
enclave ping
enclave health
enclave doctor
```

| Command | Description |
|---------|-------------|
| `daemon run` | Run the daemon in the foreground. |
| `daemon start` | Start the daemon in the background and wait for it to be ready. |
| `daemon status` | Check if the daemon is running. |
| `daemon stop` | Stop the daemon. |
| `ping` | Send a ping to the daemon and print the response. |
| `health` | Print daemon health information (state dir, uptime, etc.). |
| `doctor` | Run diagnostic checks: registry consistency, orphaned mounts, stale cgroups, and cgroup v2 availability. |

## Enclavefile Lifecycle

```bash
enclave init
enclave up        [--rebuild]
enclave down
enclave restart   [--rebuild]
```

| Command | Description |
|---------|-------------|
| `init` | Scaffold a blank `Enclavefile` in the current directory. |
| `up` | Read the Enclavefile, create sandbox, run setup, create and start all workspaces. |
| `up --rebuild` | Force sandbox recreation and rerun setup commands. |
| `down` | Stop all workspaces and the sandbox. |
| `restart` | Stop and restart the entire environment. |
| `restart --rebuild` | Rebuild the sandbox from scratch on restart. |

## Sandbox

```bash
enclave create  <name> [--suite bookworm] [--mirror URL] [--bootstrap-method debootstrap|cached_rootfs]
enclave start   <sandbox>
enclave stop    <sandbox>
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
| `destroy` | Stop and permanently delete a sandbox and all its workspaces. |
| `list` | List all sandboxes. |
| `stats` | Show live stats for all running workspaces across all sandboxes. |
| `ps` | Show live process status for all running workspaces across all sandboxes. |
| `ps --local` (`--project`) | Show only workspaces defined by the Enclavefile in the current directory. |
| `status` | Show detailed status for a sandbox. |
| `remove` | Remove a sandbox entry from the registry (does not delete files). |
| `wipe` | Destroy all sandboxes. Requires confirmation. |

## Workspace

```bash
enclave workspace create  <sandbox> <name> [--cpu-seconds N] [--memory-mb N] [--max-procs N] [--max-open-files N]
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
| `start` | Start a workspace session (namespaces + mounts). |
| `stop` | Stop a running workspace session. |
| `destroy` | Stop and permanently delete a workspace. |
| `list` | List workspaces, optionally filtered by sandbox. |
| `status` | Show detailed status for a workspace (process count, resource usage). |
| `remove` | Remove a workspace entry from the registry. |
| `wipe` | Destroy all workspaces across all sandboxes. Requires confirmation. |
| `enter` | Enter a running workspace interactively (namespace handoff). |
| `exec` | Execute a one-shot command inside a workspace. |
| `run` | Run a command inside a workspace (alias for exec). |
| `port publish` | Persist and activate a loopback-only TCP port mapping for a workspace. |
| `port unpublish` | Remove a previously declared loopback-only TCP port mapping. |
| `port list` | Show declared and active published ports for a workspace. |
| `logs` | Show workspace session logs. `--follow` continuously streams appended log output. |
| `stats` | Show workspace resource metrics like CPU %, memory usage/limit, memory %, network I/O, block I/O, pids, and threads. |

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

Sandbox limits are aggregate caps across all running workspaces in that sandbox. Workspace limits apply to the individual workspace process tree.

## Snapshots

```bash
enclave snapshot create  <sandbox> <workspace> [--name snapshot-name]
enclave snapshot list    <sandbox> <workspace>
enclave snapshot restore <sandbox> <workspace> <snapshot-name>

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
