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

Place a minimal rootfs under `<state_dir>/sandboxes/rootfs-cache/<suite>/` for a suite-specific cache, or `<state_dir>/sandboxes/rootfs-cache/base/` for a generic cache, then create a sandbox with:

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

Publish `bookworm-rootfs.tar.gz`, then on another machine import it directly:

```bash
enclave rootfs fetch --suite bookworm https://github.com/<owner>/<repo>/releases/download/<tag>/bookworm-rootfs.tar.gz
```

You can also import a local archive without an extra `curl` step:

```bash
enclave rootfs import --suite bookworm ./bookworm-rootfs.tar.gz
```

After that, set:

```toml
[sandbox]
bootstrap_method = "cached_rootfs"
```

and first-time `enclave up` will copy the prebuilt rootfs instead of constructing one from `debootstrap`.

## Install

```bash
./scripts/install.sh
```

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

**7. Tear it down:**

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
