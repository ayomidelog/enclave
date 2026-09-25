# Enclave

Run multiple isolated workspaces in one base system.

> Enclave is a hardened Linux namespace-based sandbox platform for isolated multi-workspace development environments.

One file, one command, entire environment running.

- **Sandbox** — isolated root filesystem bootstrapped and managed by Enclave.
- **Workspace** — isolated execution context inside a sandbox with dedicated user, PID, mount, network, and UTS namespaces plus an idmapped `/home` mount.

## Documentation

- [Architecture](docs/architecture.md)
- [Runtime Details](docs/runtime-details.md)
- [Storage Layout](docs/storage.md)
- [Command Reference](docs/commands.md)
- [Enclavefile Reference](docs/enclavefile.md)
- [Configuration](docs/configuration.md)
- [Security](docs/security.md)
- [Limitations](docs/limitations.md)
- [Roadmap](docs/roadmap.md)
- [Performance Harness](tools/perf/README.md)

## Runtime recovery

Enclave protects each configured state directory with an exclusive daemon lock. If a daemon or runtime is interrupted, run diagnostics before manually removing state:

```bash
enclave doctor
enclave doctor --repair
```

`doctor --repair` reconciles stale registry records, workspace namespace references, mounts, and safe orphaned files. Destructive commands require a running daemon by default; use `enclave daemon start` or opt in for one command with `--start-daemon`.

## Why Enclave exists
I was running multiple AI agents in parallel and needed each one isolated, separate filesystem, separate processes and no cross-contamination.

The obvious answer was Docker. But every container needed its own copy of Node, Python, whatever the agent used. Five agents meant five redundant installs, five images to maintain, five containers with identical tooling just to keep them apart.

What I actually wanted was simple: one environment with everything installed, split into isolated workspaces. Global tools shared, everything else separated.
That didn't exist. So I built it.

## How it differs from Docker

| | Enclave | Docker |
|---|---|---|
| **Rootfs sharing** | All workspaces share one sandbox rootfs | Each container has its own layered rootfs |
| **Environment source** | `debootstrap` or prebuilt rootfs | OCI images pulled from registries |
| **Runtime** | Single daemon → direct `unshare`/`nsenter` | dockerd → containerd → runc |
| **Configuration** | Single `Enclavefile` (TOML) | `Dockerfile` + `docker-compose.yml` |
| **Distribution** | None — local only | Push/pull via registries |
| **Scope** | Local dev and workspace isolation | Production container orchestration |

> Enclave is not a Docker replacement for shipping images, and it is not a VM or hypervisor. It is a Linux-only local development isolation tool built directly on namespaces, OverlayFS, and cgroup primitives.

> **Linux only.** Enclave depends on Linux kernel features (`unshare`, user namespaces, idmapped mounts, OverlayFS, `/proc` namespaces), not on any specific distribution. macOS and Windows are not supported.

## Latest Verified Lifecycle Timing

On the bounded eight-workspace cached-rootfs benchmark (bootstrap preparation
excluded), as the median of seven consecutive runs:

- cold workspace boot: `2.43s`
- cold shutdown: `0.61s`
- warm workspace boot: `2.03s`
- warm shutdown: `0.60s`

The validation host is shared with other work, so the spread is wide: cold boot
ranged 2.42–3.68s, cold shutdown 0.43–0.76s, warm boot 1.91–2.10s, and warm
shutdown 0.47–0.64s. The medians are the number to compare; the ranges are what
a shared host does to them. Reproduce them with:

```bash
ENCLAVE_UP_WORKERS=1 ENCLAVE_CLEANUP_WORKERS=4 ./tools/perf/live-lifecycle.sh
```

The benchmark reuses a local cached rootfs and does not download bootstrap packages.

The run pins one up worker so its numbers are comparable between releases. The
daemon's own default is one start per core, so a plain `enclave up` on this host is
faster than the number above: the pinned run is a floor, and it is the one the
range describes. A sweep of the worker count found no reason to change that
default. On this four-CPU host the eight-workspace fixture booted in 1.54s at four
workers, 1.40s at eight, and 1.65s at sixteen, each the median of three runs, which
is inside the spread of the fixture itself.

These numbers are all for the stop-and-start tier. Pausing a running sandbox is the
fast tier, and it keeps the processes and their memory; see the lifecycle tiers
section below.

## Lifecycle tiers

Enclave has three ways to put a workspace away, and they preserve different things.
Which one to use is a question about what the work inside the workspace holds that is
expensive to rebuild, so each is stated in those terms.

| Command | Runtime and its PID | Memory and processes | Mounts | Files | Typical cost |
|---|---|---|---|---|---|
| `pause` / `resume` | kept | kept, and frozen while paused | kept | kept | ~0.2 s |
| `stop` / `start` | released and rebuilt | lost | released and rebuilt | kept | ~0.9 s warm |
| `destroy` | released | lost | released | removed | a stop, plus deletion |

A `pause` freezes the sandbox cgroup and leaves everything else where it is. The
processes keep their PIDs, their memory, and their mounts, so a program that was
running continues from where it was rather than starting over. That is the tier to
reach for when the workspace holds something that took minutes to build and cannot be
rebuilt cheaply, such as a loaded model, a warm cache, or a long test run in progress.

A `stop` releases the runtime and everything only the runtime held, and keeps the
workspace files. A later `start` builds a new runtime from those files, so the
processes are new processes with new PIDs and nothing in memory survives. Use it when
the workspace is between jobs.

A `destroy` does what a `stop` does and then removes the workspace files and its
record. Use it when the work inside the workspace is finished with.

The published number for each tier is measured by the benchmark under `tools/perf/`
that exercises it: `live-lifecycle.sh` for stop and start, `live-pause.sh` for pause
and resume, `live-quota.sh` for the storage tier whose workspace owns an ext4 image
rather than a directory, and `live-loaded.sh` for a sandbox whose workspaces are all
busy at once. They are separate runs because a change to one tier is not a change to
another, and one number covering all of them would describe none of them.

## Snapshot Archives

Workspace snapshots are still stored locally as copy-based directories, but Enclave can now package and restore them as portable archives:

```bash
enclave snapshot export mybox ws1 snap1 --output ./ws1-snap1.tar.gz
enclave snapshot import mybox ws1 --name imported-snap ./ws1-snap1.tar.gz
```

That lets you move a workspace snapshot between hosts or keep an archive copy without changing the normal local snapshot workflow.

## Requirements

- Any modern Linux distribution with kernel 5.12+ recommended for idmapped mounts
- OverlayFS support (built-in on most kernels, or `modprobe overlay`)
- Namespace support (user, PID, mount, network, UTS)
- `util-linux` with `mount` support for `X-mount.idmap` and `setpriv`
- `iproute2`
- `iptables` (either `iptables-nft` or `iptables-legacy`; auto-detected at runtime)
- `debootstrap` only if using the `debootstrap` bootstrap method (default)
- Rust toolchain for building

Enclave relies exclusively on Linux kernel features. There is no distro detection or distro-specific branching at runtime. It runs on Debian, Ubuntu, Fedora, Arch, Alpine, and other distributions that provide the required kernel and util-linux features.

Workspace runtimes are hardened with user-namespace isolation, capability dropping, read-only `/proc/sys` and `/sys` remounts, seccomp deny rules, and optional AppArmor/SELinux hooks.

Managed workspace disk limits cover more than `/home`: when `disk_mb` is configured, the workspace uses a quota-backed root OverlayFS layer, so writes under `/opt`, `/var`, `/etc`, `/root`, `/home`, and workspace-private `/tmp` consume that workspace's allocation. The shared sandbox rootfs remains the lower layer and is not modified by those writes.

Temporary files normally survive a workspace stop when they are stored on managed disk. To get reset-on-restart behavior, opt in per workspace:

```toml
[workspace.builder]
name = "builder"
clear_tmp_on_restart = true
```

## Bootstrap Methods

Enclave supports multiple methods for creating sandbox root filesystems:

| Method | Description | When to use |
|---|---|---|
| `debootstrap` (default) | Builds a Debian/Ubuntu rootfs locally | Standard usage on any distro with `debootstrap` installed |
| `cached_rootfs` | Copies a prebuilt minimal rootfs from the Enclave state dir | Distro-agnostic; use any rootfs (Alpine, Fedora, etc.) |

### Using `debootstrap` (default)

```bash
sudo apt-get install -y debootstrap util-linux iproute2   # Debian/Ubuntu
sudo pacman -S debootstrap util-linux iproute2             # Arch
sudo dnf install -y debootstrap util-linux iproute2        # Fedora
```

```bash
enclave create mybox --suite bookworm
```

### Using a cached rootfs

Import a rootfs archive to register it in the cache index. Hand-placing a directory in `rootfs-cache` can leave it unindexed when the cache index already exists. Use `--suite` for a suite-specific cache or `--base` for a generic cache:

```bash
enclave rootfs import --suite bookworm ./bookworm-rootfs.tar.gz
# or
enclave rootfs import --base ./minimal-rootfs.tar.gz
```

Then create a sandbox with:

```bash
enclave create mybox --bootstrap-method cached_rootfs
```

Or set it in your `Enclavefile`:

```toml
[sandbox]
name = "devbox"
bootstrap_method = "cached_rootfs"
```

### Sharing a prebuilt rootfs

If you want Docker-like first-run speed, build the rootfs once, package it, host the archive somewhere like GitHub Releases, then fetch it into Enclave's cache:

```bash
enclave rootfs export --suite bookworm --output ./bookworm-rootfs.tar.gz
```

Publish `bookworm-rootfs.tar.gz`, then on another machine fetch it directly:

```bash
enclave rootfs fetch --suite bookworm https://github.com/ayomidelog/enclave/releases/download/rootfs-bookworm-2026-06-07/bookworm-rootfs-clean-2026-06-07.tar.gz
```

You can also import a local archive without an extra `curl` step:

```bash
enclave rootfs import --suite bookworm ./bookworm-rootfs.tar.gz
```

Current published example asset:

- Release page: `https://github.com/ayomidelog/enclave/releases/tag/rootfs-bookworm-2026-06-07`
- Asset: `bookworm-rootfs-clean-2026-06-07.tar.gz`

After that, set:

```toml
[sandbox]
bootstrap_method = "cached_rootfs"
```

and first-time `enclave up` will mount the prebuilt rootfs as a shared,
read-only lower layer instead of constructing one from `debootstrap` or copying
it. Each sandbox gets its own writable overlay on top, so creation cost does
not depend on how many files the cached rootfs contains.

## Install from source

```bash
./scripts/install.sh
```

## Install a release binary

Release binaries target x86_64 Linux and are built on Ubuntu 22.04 so they run
on supported glibc-based distributions including Ubuntu 22.04 and Debian 12.
Download and install the archive with:

```bash
curl -fL https://github.com/ayomidelog/enclave/releases/latest/download/enclave-linux-x86_64.tar.gz -o enclave-linux-x86_64.tar.gz
curl -fL https://github.com/ayomidelog/enclave/releases/latest/download/enclave-linux-x86_64.tar.gz.sha256 -o enclave-linux-x86_64.tar.gz.sha256
sha256sum -c enclave-linux-x86_64.tar.gz.sha256
tar -xzf enclave-linux-x86_64.tar.gz
sudo install -m 0755 enclave-linux-x86_64 /usr/local/bin/enclave
enclave --version
```

The release archive contains a binary named `enclave-linux-x86_64`. Runtime
tools such as `iproute2`, `iptables`, util-linux, and (for the default bootstrap
method) `debootstrap` are still required on the host.

## Quickstart

**1. Scaffold an Enclavefile:**

```bash
enclave init
```

**2. Edit it:**

```toml
[sandbox]
name = "devbox"
suite = "bookworm"

setup = [
  "apt install -y nodejs python3 cargo",
]

[workspace.api]
name = "api"
run = "node server.js"
workspace_dir = "./project"
ports = ["127.0.0.1:3001:3000/tcp"]

[workspace.shell]
name = "shell"
```

**3. Bring it up:**

```bash
enclave up
```

**4. Enter a workspace:**

```bash
enclave workspace enter devbox shell
```

**5. Reach a workspace service from the host when needed:**

```bash
curl http://127.0.0.1:3001
enclave workspace port list devbox api
```

**6. Stream logs and inspect metrics:**

```bash
enclave workspace logs api --follow
enclave workspace stats api
enclave stats
```

**7. Copy files to or from a running workspace when needed:**

```bash
enclave workspace cp devbox shell ./notes.md ws:/home/notes.md
enclave workspace cp devbox shell ws:/home/output.txt ./output.txt
```

Use the `ws:/` prefix on exactly one path to identify the workspace side. The
command streams regular files and directories through the namespace boundary;
see the [Command Reference](docs/commands.md) for copy semantics and current
limitations.

**8. Tear it down:**

```bash
enclave down
```

## Parallel AI agents example

A concrete Enclave setup is a small agent swarm that shares one prepared toolchain but keeps each role isolated:

```toml
[sandbox]
name = "agents"
setup = [
  "apt install -y git nodejs python3",
]

[workspace.planner]
name = "planner"
run = "./agent.sh planner"
workspace_dir = "./agents/planner"

[workspace.coder]
name = "coder"
run = "./agent.sh coder"
workspace_dir = "./agents/coder"

[workspace.reviewer]
name = "reviewer"
run = "./agent.sh reviewer"
workspace_dir = "./agents/reviewer"
```

That gives you one sandbox rootfs with shared system packages, while each agent gets its own `/home`, process tree, network namespace, logs, and lifecycle controls. When `workspace_dir` points at a host project directory, Enclave mounts it through an idmapped bind mount rather than a raw host bind.

If a workspace needs to serve a dev app back to the host browser, add `ports = ["127.0.0.1:3001:3000/tcp"]` in the `Enclavefile` or use `enclave workspace port publish ...` after startup.

## Auth Providers

Enclave supports minimal token-based auth providers for workspace access to service credentials.

### Store a provider token

```bash
enclave auth login github
```

You will be prompted for the token value via hidden stdin input.

Configured providers can be listed with:

```bash
enclave auth list
```

Remove a provider token with:

```bash
enclave auth logout github
```

### Declare workspace auth providers

In your `Enclavefile`:

```toml
[workspace.api]
name = "api"
auth = ["github", "npm"]
env_tokens = ["ENCLAVE_TOKEN"]
```

When a workspace starts, Enclave checks declared providers, loads available tokens, and injects them as:

- `enclave` → `ENCLAVE_TOKEN`
- `github` → `GITHUB_TOKEN`, `GH_TOKEN`
- `npm` → `NPM_TOKEN`

For GitHub-enabled workspaces, Enclave exports both `GITHUB_TOKEN` and `GH_TOKEN`, and configures non-interactive HTTPS Git auth (`git clone`, `git push`) through Git's `credential.helper` environment configuration plus `GIT_TERMINAL_PROMPT=0`.

Read-only token files are also written inside the workspace rootfs at:

- `/run/enclave/auth/<provider>.token` (mode `0400`)
- `/run/enclave/env/<TOKEN_NAME>` (mode `0400`) for `env_tokens = [...]`

Missing provider tokens log warnings and do not block workspace startup.

### Security model

- Tokens are stored only in the Enclave state directory under `<state_dir>/auth/<provider>.token`.
- Token files are validated for strict ownership and mode (`0600`, root-owned) before use.
- Enclave does **not** read host credential sources like `~/.ssh`, `~/.gitconfig`, or other host secret files.
- Tokens are only injected for providers explicitly declared in workspace configuration.

## Security at a glance

Enclave uses kernel-enforced UID authentication on its Unix socket, a UID-based policy engine, per-workspace user/PID/mount/network/UTS namespaces, idmapped `/home` mounts, capability dropping, read-only `/proc/sys` and `/sys` remounts, masked kernel-info proc/sys paths, host-local and metadata network blocks, and a seccomp deny list for runtime hardening.

It is designed for local development isolation, not for hostile multi-tenant workloads or VM-grade isolation. For the full threat model, setup-command caveats, and current constraints, see [docs/security.md](docs/security.md) and [docs/limitations.md](docs/limitations.md).

## Examples

The [Quickstart](#quickstart) section above covers the core workflow. For command details see the [Command Reference](docs/commands.md). For Enclavefile options see the [Enclavefile Reference](docs/enclavefile.md). For runtime behavior and networking details see [Runtime Details](docs/runtime-details.md).

## Tested Distributions

Enclave is validated across the following Linux distributions:

| Distribution | Kernel | Status |
|---|---|---|
| Debian 12 (Bookworm) | 6.1+ | ✅ Supported |
| Ubuntu 22.04+ | 5.15+ | ✅ Supported |
| Fedora 38+ | 6.2+ | ✅ Supported |
| Arch Linux | rolling | ✅ Supported |
| Alpine Linux 3.18+ | 6.1+ | ✅ Supported |

Any Linux distribution with a modern kernel, OverlayFS support, user namespaces, and idmapped mount support should work. If you encounter issues on an unlisted distro, please open an issue.
