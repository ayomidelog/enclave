# Runtime Details

This page collects the lower-level runtime behavior that is useful once you are past the quickstart: architecture philosophy, networking, performance expectations, and stability guarantees.

## Architecture Philosophy

- **Kernel-first design**: Enclave relies exclusively on Linux kernel primitives (`unshare`, user namespaces, idmapped mounts, OverlayFS, cgroup v2, network namespaces). There is no OCI/container runtime stack and no image format.
- **Reproducibility via rootfs cache**: Sandbox root filesystems are bootstrapped once (via `debootstrap` or a cached base image) and shared read-only across all workspaces. Workspace-specific changes live in OverlayFS upper layers, ensuring the base environment is always reproducible.
- **Single daemon, direct syscalls**: One daemon process manages all sandboxes and workspaces. Workspace sessions are created with direct `unshare` calls and entered through an internal namespace helper — no intermediate container runtime or orchestration layer.
- **Post-bootstrap hardening**: After workspace bootstrap, Enclave remounts `/proc/sys` read-only, attempts to remount `/sys` read-only when the kernel permits it, drops runtime capabilities, and installs a seccomp deny list.
- **Fail-safe cleanup**: All mount, cgroup, and network resources are cleaned up deterministically on workspace stop/destroy. The daemon reconciles stale state on startup and provides a `doctor` command for manual verification.

## Networking

Each workspace runs in its own network namespace with full port isolation and outbound Internet access via NAT.

### Architecture

```text
┌─────────────┐  ┌─────────────┐
│ workspace A │  │ workspace B │
│ eth0        │  │ eth0        │
│ 10.200.0.10 │  │ 10.200.0.11 │
└──────┬──────┘  └──────┬──────┘
       │ (veth)         │ (veth)
───────┴────────────────┴───────── enclave0 bridge (10.200.0.1/24)
                  │
           NAT masquerade → Internet
```

- **Port isolation**: Workspaces A and B can both bind `0.0.0.0:4000` without conflicts.
- **Outbound access**: `curl google.com` works inside every workspace.
- **Host isolation by default**: Host cannot reach workspace ports unless you explicitly publish them.
- **Loopback-only publishing**: Published ports bind to `127.0.0.1` only in v1, so host access stays local to the machine.
- **Cross-workspace isolation**: direct workspace-to-workspace forwarding is blocked by default on the Enclave bridge.
- **Host-service isolation**: direct access from a workspace to host-local services and the cloud metadata endpoint is blocked by default.
- **Clean teardown**: Stopping/destroying a workspace removes its veth pair and releases its IP.

### Additional network guards

- IPv6 is disabled on the Enclave bridge and workspace veth interfaces instead of relying on a parallel IPv6 firewall policy.
- Per-veth anti-spoofing rules drop packets whose source IP does not match the workspace's assigned IPv4 address.

### IP allocation

- `10.200.0.1` — bridge gateway (`enclave0`)
- `10.200.0.2`–`.9` — reserved
- `10.200.0.10`–`.254` — workspace pool (auto-allocated, persisted in registry)

### DNS

A managed `/etc/resolv.conf` is provisioned in each workspace rootfs. If the host resolver is a systemd-resolved stub (`127.0.0.53`), Enclave reads the upstream resolver list from `/run/systemd/resolve/resolv.conf` instead, ensuring name resolution works without the stub dependency.

## Performance

Rough ballpark on a 4-core x86_64 host (NVMe):

| Metric | Enclave | Docker (alpine) |
|---|---|---|
| Sandbox create (debootstrap) | ~30–60 s | N/A (image pull) |
| Sandbox create (cached_rootfs) | ~2–10 s | N/A (image pull) |
| Workspace start (namespace + overlay) | ~200–400 ms | ~300–500 ms (`docker run`) |
| Workspace enter (nsenter) | ~50 ms | ~50 ms (`docker exec`) |
| Rootfs size (minimal bookworm) | ~300 MB (shared) | ~5 MB per alpine layer |
| Memory overhead per workspace | ~2–4 MB | ~6–10 MB |

Numbers vary by host hardware, suite, and setup commands. The key tradeoff: one shared rootfs means the bootstrap cost is paid once regardless of workspace count.

## Stability Guarantees

- **Crash recovery**: On daemon startup, Enclave reconciles workspace state against the process table. Any workspace marked as `Running` whose session PID no longer exists (or whose start-time ticks do not match) is automatically transitioned to `Stopped`. This handles daemon crashes, host reboots, and OOM-killed sessions without manual cleanup.
- **cgroup fallback**: When cgroup v2 is not available, Enclave falls back to rlimit-only resource enforcement and logs a warning. Workspace isolation remains intact — only hard memory/PID limits are downgraded to soft rlimits.
- **Mount cleanup**: Sandbox destroy fails if the rootfs unmount fails, preventing orphaned mounts. Workspace stop tears down per-workspace networking (veth + IP release) and clears runtime state atomically.
- **Overlay guarantees**: The shared rootfs is bind-mounted read-only under OverlayFS. Workspace writes go to the upper layer only. Stopping or destroying a workspace removes only the workspace-specific overlay data — the shared rootfs is never modified.
- **Workspace source mounts**: `/home` is presented through an idmapped bind mount, whether the source is the Enclave-managed workspace directory or an explicit host `workspace_dir`.
- **System diagnostics**: Run `enclave doctor` to verify mount state, cgroup state, registry consistency, and runtime process health at any time.
