# Security

Enclave implements multiple layers of security hardening across privilege management, authorization, input validation, persistence, process safety, and filesystem operations.

## Threat Model

**Guarantees (what Enclave enforces):**

- Workspace runtimes execute inside dedicated user, PID, mount, network, and UTS namespaces.
- Workspace processes cannot see or signal processes in other workspaces (PID namespace).
- Workspace runtimes do not execute as host kernel root; user-namespace mappings place workspace root on subordinate host IDs.
- Workspace filesystem writes are isolated to the workspace overlay or configured workspace source mount; the shared rootfs is not modified (OverlayFS upper/lower split).
- Workspace runtime mounts remount `/proc/sys` read-only after bootstrap and attempt to remount `/sys` read-only when the kernel allows it in the workspace's namespace model.
- Workspace runtime masks selected proc/sys paths that expose kernel-global information (`/proc/kallsyms`, `/proc/modules`, `/sys/module`, and similar paths).
- Workspace runtime capabilities are reduced and escape-oriented syscalls are blocked by a seccomp deny list.
- Workspace-to-workspace forwarding is blocked on the Enclave bridge by default.
- Direct workspace access to host-local services and the cloud metadata endpoint (`169.254.169.254`) is blocked by default.
- Resource consumption per workspace is bounded by rlimit and (where available) cgroup v2 limits.
- Daemon API access is restricted by a UID-based policy engine with deny-by-default support.
- Requests are rate-limited per-UID and globally to prevent abuse and resource exhaustion.
- Optional AppArmor/SELinux labels can be applied to workspace runtime helpers when configured on the host.

**Non-goals (what Enclave does not defend against):**

- A malicious root user on the host. Enclave trusts the host root.
- Unknown kernel vulnerabilities or a full VM-grade hardware isolation boundary.
- Untrusted sandbox setup commands. Setup still runs as root inside a `chroot`; a malicious setup script has root-level access within the sandbox rootfs during setup (see [Setup Command Security](#setup-command-security)).
- Automatically shipping AppArmor or SELinux host policies. Enclave can request a configured host profile/label, but the policy content itself is host-managed.
- Cross-host portability. Enclave relies on Linux-specific primitives and does not target other operating systems.

## Privilege Model

Most Enclave operations require root. The main exceptions are `help`, `--help`, `--version`, and `init`. The daemon authenticates connecting clients by reading the peer UID from the Unix socket using `SO_PEERCRED`. This means:

- The daemon knows which user is making each request.
- UID-based policy rules can restrict what each user is allowed to do.
- No passwords or tokens are involved — authentication is kernel-enforced.

## Policy Engine

The policy engine evaluates authorization rules before every daemon action.

- Rules can be **per-UID** or **wildcard** (apply to all users).
- UID-specific rules override wildcard rules.
- **Deny always wins** within a rule group — if both an allow and deny rule match, the action is denied.
- The default policy (allow or deny) applies when no rules match.

```bash
# Set default policy
enclave policy default deny

# Allow a specific user to create sandboxes
enclave policy allow --uid 1000 "sandbox.create"

# Deny all users from destroying sandboxes
enclave policy deny "sandbox.destroy"

# View current rules
enclave policy show
```

## Input Validation

- **Debootstrap suite**: validated against a whitelist of known Debian releases.
- **Mirror URL**: must use `http://` or `https://` scheme. `file://` mirrors are rejected.
- **Sandbox and workspace names**: must be 1–63 characters, starting with an alphanumeric character, containing only alphanumerics, hyphens, and underscores. Dots and path separators are rejected.
- **Snapshot names**: validated for length, allowed characters, and path traversal (`..`) segments.
- **Workspace CWD**: restricted to the `/home` subtree with component-level validation. Paths outside `/home` are silently redirected to `/home`.

## Persistence Integrity

Registry and policy state files use atomic writes:

1. Write to a temporary file in the same directory.
2. `fsync` the temporary file to ensure data is on disk.
3. Rename the temporary file to the target path (atomic on POSIX).

Advisory file locking (`flock`) prevents concurrent writes from corrupting state.

## Process Safety

Workspace runtime tracks both the PID and the process start-time ticks (`/proc/<pid>/stat` field 22). This mitigates PID reuse TOCTOU races:

- Before sending signals (SIGTERM, SIGKILL), the daemon verifies the PID still belongs to the expected process by comparing start-time ticks.
- If the PID has been reused by a different process, the signal is not sent.

## Filesystem Hardening

- **Runtime directory**: checked for correct ownership and permissions on startup.
- **Socket validation**: the daemon socket is verified for ownership and mode before the CLI connects.
- **Symlink rejection**: snapshot copy operations reject symlinks that point outside the workspace boundary.
- **Mount path canonicalization**: bind mount and overlay mount targets are canonicalized to prevent path confusion.
- **Sandbox boundary validation**: all paths derived from user input are validated to stay within the sandbox directory tree.
- **Path traversal guards**: `..` components are detected and rejected in snapshot names, workspace CWD, and other user-supplied paths.

## Resource Enforcement

Workspace and sandbox resource limits use a dual approach:

1. **rlimit (workspace-local)**: `cpu_seconds`, `memory_mb`, `max_procs`, and `max_open_files` are applied to the workspace session process via `prlimit` / `RLIMIT_*`.
2. **cgroup v2 (aggregate and steady-share)**: when the unified hierarchy is mounted at `/sys/fs/cgroup`, Enclave creates a sandbox parent cgroup plus per-workspace child cgroups and writes `memory.max`, `cpu.max`, and `pids.max`.

This split is intentional:

- `cpu_seconds` remains a CPU time budget (`RLIMIT_CPU`), not a steady CPU-share knob.
- `cpu_percent` maps to `cpu.max` and represents a percentage of total machine CPU capacity.
- sandbox `memory_mb`, `cpu_percent`, and `max_procs` are aggregate caps across all running workspaces in that sandbox.

When cgroup v2 is not available, Enclave still applies the workspace-local `RLIMIT_*` limits, but cgroup-only controls such as `cpu_percent` and aggregate sandbox limits cannot be enforced.

## Daemon Recovery

On startup, the daemon reconciles workspace state against the process table. Any workspace marked as `Running` whose session PID no longer exists (or whose start-time ticks do not match) is automatically transitioned to `Stopped`. This handles daemon crashes, host reboots, and OOM-killed workspace sessions without requiring manual cleanup.

The registry repair command (`enclave registry repair --strict`) can be used for deeper cleanup of stale on-disk state.

## Request Limits

- **Per-UID rate limiting**: the daemon enforces per-UID request rate limiting to prevent a single user from monopolizing the daemon.
- **Global rate limiting**: a separate global rate limit caps total requests across all UIDs within a time window, preventing distributed abuse from multiple users.
- **Maximum request size**: incoming JSON requests are limited in size to prevent memory exhaustion.

## Workspace Filesystem Exposure

Each workspace mounts `/home` from a workspace source directory:

- By default that source is the Enclave-managed `workspaces/<id>/fs/` mount target under the sandbox state tree.
- When a per-workspace disk quota is configured, that `fs/` mount target is backed by a loop-mounted `fs.img` ext4 image managed by Enclave.
- If `workspace_dir` / legacy `path` is configured, that host project directory is used instead.
- In both cases the mount is created as an **idmapped bind mount**, not a raw host bind mount. This preserves the hardened user-namespace model while still allowing workspace access to the selected source tree.

This means:

- Workspace processes have read/write access to the selected `/home` source tree only.
- The mount is scoped to that directory; it does not expose the full host filesystem.
- Writes inside the workspace still write back to the selected source directory.
- Ownership inside the mount is mediated by the idmap attached to that mount, rather than by direct host-root access.

If you mount a host project directory, treat that directory as shared development state. The workspace should not be considered an immutable snapshot of the host tree.

## Setup Command Security

Sandbox setup commands (the `setup` array in the Enclavefile) run as root inside a `chroot` of the sandbox rootfs. This means:

- Setup scripts have full root access within the chroot environment.
- A malicious or compromised setup command could modify any file in the sandbox rootfs.
- Setup does **not** use the hardened workspace runtime path. The seccomp, capability-drop, and namespace runtime protections described above apply to workspaces, not to the setup phase.

Only run setup commands from trusted sources. Treat the setup phase as equivalent to running a shell script as root on the sandbox filesystem.

## Product Scope

Enclave is designed for local Linux development isolation. It is not intended to provide VM-style isolation or to safely execute hostile multi-tenant workloads. For the full list of current product constraints, see [limitations.md](limitations.md).

## Recommendations

- Run the daemon as root with a restrictive policy (`enclave policy default deny`).
- Grant specific actions to specific UIDs as needed.
- Use `enclave registry repair --strict` periodically to clean up stale state.
- Keep the host kernel and `debootstrap` packages up to date.
- Ensure `/etc/subuid` and `/etc/subgid` are configured for the account that launches workspace sessions. When Enclave launches a workspace session as `root` (for example via `sudo enclave ...`), it now prefers configured subordinate ID ranges and falls back to a direct single-ID root mapping only when no usable subordinate ranges are available.
- Use a host `mount` implementation that supports `X-mount.idmap` for workspace `/home` mounts.
- Verify that host networking allows Enclave to install its bridge firewall rules (host-local blocking, metadata blocking, and anti-spoofing checks).
- Only use trusted setup commands in Enclavefiles.
- On systems with cgroup v2, verify that the unified hierarchy is mounted for hard resource enforcement.
- If you enable AppArmor or SELinux options, verify that the corresponding host profile or label actually exists before starting the daemon.
