# Enclavefile Reference

An `Enclavefile` is a TOML file that declaratively defines an Enclave environment — one sandbox and one or more workspaces.

## Example

```toml
[sandbox]
name = "devbox"
suite = "bookworm"
memory_mb = 4096
cpu_percent = 50

setup = [
  "apt install -y nodejs python3 cargo",
  "npm install -g typescript",
  "pip install flask numpy",
]

[workspace.api]
name = "api"
run = "node server.js"
cpu_percent = 25
memory_mb = 2048
workspace_dir = "./project"
auth = ["github"]
ports = ["127.0.0.1:3001:3000/tcp"]

[workspace.builder]
name = "builder"
run = "cargo build --release"
clear_tmp_on_restart = true

[workspace.shell]
name = "shell"
```

## `[sandbox]`

Defines the sandbox environment.

| Key | Required | Default | Description |
|-----|----------|---------|-------------|
| `name` | Yes | — | Human-readable name for the sandbox. Used as the identifier in all commands. |
| `suite` | No | `bookworm` | Debian release to bootstrap. Allowed values: `bookworm`, `bullseye`, `trixie`, `sid`, `stable`, `testing`, `oldstable`. |
| `bootstrap_method` | No | `debootstrap` | How to create the sandbox rootfs. `debootstrap` builds a Debian/Ubuntu rootfs locally. `cached_rootfs` copies a prebuilt rootfs from `<state_dir>/sandboxes/rootfs-cache/base/` or a suite-specific cache. Use `enclave rootfs fetch` or `enclave rootfs import` to populate that cache ahead of time. |
| `memory_mb` | No | — | Aggregate sandbox memory cap across all running workspaces. Requires cgroup v2 for enforcement. |
| `cpu_percent` | No | — | Aggregate sandbox CPU share as a percentage of total machine CPU capacity. Requires cgroup v2 for enforcement. |
| `max_procs` | No | — | Aggregate sandbox process-count cap. Requires cgroup v2 for enforcement. |
| `disk_mb` | No | — | Total disk budget for the sandbox's workspaces. A sandbox rootfs is a shared lower layer rather than an image, so this caps the sum of its workspaces' `disk_mb` allocations and is enforced when a workspace is created or grown. Change it on an existing sandbox with `enclave resize <sandbox> --disk-mb N`. |
| `setup` | No | `[]` | List of shell commands run inside the sandbox root via `chroot` during creation and on later `enclave up` / `enclave restart` runs. Use idempotent commands. |

### Setup Commands

Setup commands run when the sandbox is first created and are re-executed on subsequent `enclave up` and `enclave restart` calls so Enclavefile changes can be applied to an existing sandbox.

Each command runs sequentially inside the sandbox rootfs via `chroot`. If any command fails, the setup stops and the error is reported.

Because setup commands are re-run, they should be idempotent (for example `apt install -y ...`).

### Setup Cache

By default every setup command runs on every `enclave up` and `enclave restart`. That is the always-run contract: setup is re-applied so Enclavefile changes reach an existing sandbox, and commands must stay idempotent.

Pass `--cache-setup` to skip a command whose result is already recorded for the sandbox. The cache key covers every input that can change what a command does:

- the sandbox name, suite, mirror, and bootstrap method;
- the base rootfs the command runs against, including the shared lower layer's identity, so re-importing the cache invalidates recorded results;
- the whole setup command list, in order, so adding, removing, or reordering commands invalidates them;
- the Enclave version that runs the commands.

Each command reports whether it ran or was answered from the cache, and why, on stderr. A recorded result is per command index, so a partially completed setup resumes from the first command without a result.

Use `--cache-setup` only when setup commands are known to be safe to skip after an unchanged Enclavefile digest and an unchanged rootfs.

Common uses:
- Installing packages: `apt install -y nodejs python3`
- Installing language toolchains: `npm install -g typescript`
- Configuring the environment: `echo 'export NODE_ENV=development' >> /etc/profile`

## `[workspace.<id>]`

Each `[workspace.*]` block defines a workspace. The key after `workspace.` is the internal ID used in the registry.

| Key | Required | Default | Description |
|-----|----------|---------|-------------|
| `name` | Yes | — | Human-readable label for the workspace. |
| `run` | No | — | Command executed when the workspace starts. If omitted, the workspace starts idle and can be entered interactively. |
| `workspace_dir` | No | — | Host directory to mount at `/home` inside the workspace instead of the Enclave-managed directory. Relative paths are resolved from the `Enclavefile` directory (for example `./project`). The directory must exist. Enclave mounts it through an idmapped bind mount rather than a raw host bind. |
| `path` | No | — | Backward-compatible alias for `workspace_dir`. Use only one of `path` or `workspace_dir`. |
| `cpu_seconds` | No | — | Per-workspace CPU time budget (`RLIMIT_CPU`). |
| `cpu_percent` | No | — | Per-workspace steady CPU share as a percentage of total machine CPU capacity. Requires cgroup v2 for enforcement. |
| `memory_mb` | No | — | Per-workspace memory cap. Enforced through cgroup v2 when available and also applied as `RLIMIT_AS`. |
| `max_procs` | No | — | Per-workspace process-count cap. Enforced through cgroup v2 when available and also applied as `RLIMIT_NPROC`. |
| `max_open_files` | No | — | Per-workspace file-descriptor cap (`RLIMIT_NOFILE`). |
| `disk_mb` | No | — | Per-workspace disk quota for Enclave-managed writable storage. On quota-backed workspaces, `/home`, workspace-private `/tmp`, and the root OverlayFS copy-on-write layer share the same quota-backed filesystem, including writes under `/opt`, `/var`, `/etc`, and `/root`. Not supported when `workspace_dir` / `path` mounts a host directory into `/home`. |
| `clear_tmp_on_restart` | No | `false` | Clear the managed workspace `/tmp` after a successful workspace or sandbox stop, before the next start. This is opt-in and has no effect on host-mounted `workspace_dir` / `path` storage. |
| `auth` | No | `[]` | List of auth providers to inject into this workspace. Supported values: `enclave`, `github`, `npm`. Only listed providers are exposed. |
| `owner` | No | — | The auth namespace this workspace's tokens are read from, e.g. `owner = "alice"`. Omitted means the shared namespace at `<state_dir>/auth`, which is what every workspace used before namespaces existed. |
| `env_tokens` | No | `[]` | List of environment variables to inject into this workspace. Any well-formed variable name is accepted: uppercase letters, digits, and `_`. `GITHUB_TOKEN` reads the `github` provider's token; any other name reads the store slot it derives (`NETFLIX_PASSWORD` reads `netflix-password`). |
| `ports` | No | `[]` | Loopback-only published port mappings. Format: `127.0.0.1:HOST_PORT:WORKSPACE_PORT/tcp`. `tcp` is the only supported protocol in v1. |

### Workspace Auth Providers

Use `auth` to explicitly opt a workspace into provider tokens:

```toml
[workspace.api]
name = "api"
auth = ["github", "npm"]
env_tokens = ["ENCLAVE_TOKEN"]
```

When `env_tokens` are declared and configured, Enclave injects matching read-only files under `/run/enclave/env/` and exports them as plain environment variables during `workspace enter`, `workspace exec`, and runtime command execution.

### Credentials that are not providers

`auth` names one of the three providers, because the wrapper inside a workspace is
generated from that table. A credential Enclave has no provider for is declared as
an environment token instead, and named by the variable the workspace asks for:

```toml
[workspace.vault]
name = "vault"
owner = "alice"
env_tokens = ["NETFLIX_PASSWORD", "GTBANK_CARD_NUMBER"]
```

The value comes from the store slot the variable name derives: the name
lowercased, with `_` written as `-`.

| Variable | Store slot | Stored with |
|----------|-----------|-------------|
| `NETFLIX_PASSWORD` | `netflix-password` | `enclave auth store --user alice --provider netflix-password` |
| `GTBANK_CARD_NUMBER` | `gtbank-card-number` | `enclave auth store --user alice --provider gtbank-card-number` |
| `_INTERNAL` | `_internal` (a leading `_` is kept) | `enclave auth store --user alice --provider _internal` |
| `GITHUB_TOKEN` | `github` (the provider) | `enclave auth store --user alice --provider github` |

The mapping is what lets a name the provider table has never heard of be stored
and injected at all, and it is one to one: two environment tokens never read the
same slot. The name is checked when the workspace is defined, so a name that could
never be written into the workspace — `netflix_password`, `NETFLIX-PASSWORD`,
`1TOKEN` — is refused rather than stored and silently never used. A leading `_` is
kept in the slot rather than written as `-`, because a token file may not begin
with `-`.

The variable is injected and scrubbed exactly like a provider token: it reaches
the workspace only as a `0400` file under `/run/enclave/env/`, it is exported for
the command being run, and its value is replaced with `[REDACTED]` in captured
`workspace exec` output.

### Per-user token namespaces

`owner` scopes a workspace's tokens to one namespace, so two workspaces on the
same sandbox can hold different credentials for the same provider:

```toml
[workspace.api]
name = "api"
owner = "alice"
auth = ["github"]

[workspace.worker]
name = "worker"
owner = "bob"
auth = ["github"]
```

Each workspace is given the token stored at
`<state_dir>/auth/users/<owner>/github.token`. Store them with
`enclave auth store --user alice --provider github`, which reads the value from
standard input.

An owner with nothing stored gets nothing. Enclave does not fall back to the
shared token, because the fallback would hand one user another user's credential;
the start warns and continues. A workspace with no `owner` reads
`<state_dir>/auth/github.token` and behaves exactly as it did before namespaces
existed.

`owner` is part of the workspace's definition, so `enclave up` reconciles it the
way it reconciles `auth`: a declared owner is applied, and removing the line
unbinds the workspace, which moves it back to the shared namespace on its next
start. Changing a binding on a running workspace takes effect when it restarts.

When `enclave` is declared and a token exists, Enclave injects:

- `ENCLAVE_TOKEN`
- `/run/enclave/auth/enclave.token` (0400)

When `github` is declared and a token exists, Enclave injects:

- `GITHUB_TOKEN` and `GH_TOKEN` for API clients and `gh`
- Git HTTPS auth support via Git `credential.helper` environment config (`GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_*`, `GIT_CONFIG_VALUE_*`) plus `GIT_TERMINAL_PROMPT=0`
- Read-only files under `/run/enclave/auth/`:
  - `github.token` (0400)

When `npm` is declared and configured, Enclave injects:

- `NPM_TOKEN`
- `/run/enclave/auth/npm.token` (0400)

If a declared provider has no token configured, Enclave logs a warning and continues startup.

### Published Ports

Use `ports` to expose selected workspace services back to the host:

```toml
[workspace.web]
name = "web"
run = "npm run dev"
workspace_dir = "./web"
ports = ["127.0.0.1:3001:3000/tcp"]
```

This binds `127.0.0.1:3001` on the host and forwards it to port `3000` inside the workspace. Enclave only allows `127.0.0.1` loopback binds in v1, so published services stay host-local by default.

### Host-backed workspace directories

When `workspace_dir` / `path` is set, Enclave mounts that directory into `/home`
through an idmapped bind mount tied to the workspace user namespace. This keeps
the runtime off host-root privileges while still allowing the workspace to work
against a real host project tree.

Practical notes:

- The host directory must already exist.
- The host needs kernel + `mount` support for idmapped mounts.
- The common single-owner project-directory case is the target behavior. Mixed
  ownership trees may expose some files as overflow IDs inside the workspace.

### Workspace Names

Workspace names must:
- Be 1–63 characters long
- Start with an ASCII letter or digit
- Contain only ASCII letters, digits, hyphens, and underscores

## Lifecycle Commands

| Command | Description |
|---------|-------------|
| `enclave init` | Scaffold a blank Enclavefile in the current directory. |
| `enclave up` | Create sandbox, run setup, create and start all workspaces. |
| `enclave up --rebuild` | Force sandbox recreation and rerun setup. |
| `enclave down` | Stop all workspaces and the sandbox. |
| `enclave restart` | Stop and restart the entire environment. |
| `enclave restart --rebuild` | Rebuild from scratch on restart. |

## Behavior

- `enclave up` when the sandbox already exists skips sandbox creation, re-applies setup commands, and starts workspaces.
- `enclave up` and `enclave restart` reconcile declared sandbox/workspace resource limits onto existing environments; changing an existing `disk_mb` allocation requires the explicit `workspace resize` command.
- `enclave workspace resize <sandbox> <workspace> [--disk-mb N] [--memory-mb N]` changes an existing Enclave-managed workspace. The disk is the `fs.img` allocation and the ext4 filesystem inside it, and it can be grown or shrunk; a disk change restarts a running workspace through the normal lifecycle and restores its published ports. Memory is a cgroup value, so a memory change is applied to the running runtime without interrupting it. Omitted limits are left alone. A shrink is refused when the filesystem holds more data than the target, and the message names the smallest allocation that would work; host-backed `workspace_dir`/`path` mounts have no managed disk to resize.
- Setup commands run at creation time and are re-applied on later `up` / `restart` runs.
- `--rebuild` forces sandbox destruction and recreation, re-running all setup commands.
- If `up` fails after creating or starting a sandbox that was previously absent or stopped, Enclave attempts to stop it before returning the error. A previously paused sandbox is re-paused; a sandbox that was already running is left running. A failed rollback is included in the reported error.
- If no Enclavefile is found in the current directory, commands produce a clear error pointing to `enclave init`.
- Workspaces with a `run` command start executing it immediately. Workspaces without `run` start idle.

## File Location

The CLI looks for a file named `Enclavefile` in the current working directory. The file must be valid TOML.

`enclave init` creates a scaffold Enclavefile with commented examples to help you get started.
