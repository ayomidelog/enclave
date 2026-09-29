# Roadmap

This roadmap focuses on the next practical steps for making Enclave more useful for day-to-day development without changing its core model of one shared sandbox rootfs with many isolated workspaces.

## Near term

1. **Per-workspace and per-sandbox access control**
   - Extend the current UID-based policy engine with finer-grained ACLs.
   - Allow one UID to manage a specific sandbox or workspace without granting broad access.

2. **Better multi-workspace service discovery**
   - Add first-class naming or registration for workspace-to-workspace communication.
   - Reduce the need to manually track bridge IP addresses when several services run together.

## After that

3. **More efficient snapshot storage**
   - Improve on the current full-copy snapshot model to reduce disk usage and restore time.
   - Keep the existing snapshot commands and retention workflow simple.

## Not planned for v1.0

- Rootless daemon / CLI operation
- VM-style or hardware-backed isolation guarantees
- Cross-platform support beyond Linux kernel primitives

## Recently completed

- **Credentials that are not providers**
  - `env_tokens` accepts any well-formed variable name, so a workspace can hold a
    credential Enclave has no provider for. The value comes from the store slot
    the name derives (`NETFLIX_PASSWORD` reads `netflix-password`), which is what
    `auth store --provider netflix-password` writes.
  - A workspace's credentials are re-resolved and rewritten before every command
    run through the daemon, so a revocation takes effect on the next command
    rather than the next restart.
- **Per-user provider credentials**
  - Tokens live in a namespace: the state directory's shared one, or
    `auth/users/<user_id>/`. A workspace selects one with `owner`, and a workspace
    with no owner keeps the shared token it always had.
  - `auth store` provisions a token non-interactively, reading the value from
    standard input so it stays out of the shell history and the process list, and
    reporting its outcome in the exit status.
  - Every store, revoke, and inject is appended to a metadata-only audit log, and
    `workspace exec` scrubs injected token values out of the output it captures.
- **Lifecycle observability and recovery**
  - One operation id per lifecycle request, written to a durable journal with its
    target and phase, returned to the caller, and printed by mutating commands.
  - A cleanup certificate: stop and destroy verify the host afterwards and refuse to
    record success while a runtime, cgroup, mount, loop device, interface, rule, or
    file is still held.
  - Transitional lifecycle states with deterministic recovery: an interrupted start
    is rolled back and an interrupted stop is completed, never resumed, except for a
    start whose own record was already written, which the next daemon completes.
  - A published p50/p95/p99 lifecycle report taken with the release binary and the
    host it was measured on, with a per-tier benchmark for each lifecycle tier.
- **Host capability diagnostics**
  - `enclave doctor` reports overlayfs, idmapped mounts, cgroup v2 and its
    controllers, loop devices, the firewall, and netlink in one check, naming what is
    lost when one is missing.
  - It also inventories nested workspace cgroups, workspace networking, loop devices,
    orphan runtimes, and the operation journal.
- **Cleanup that proves itself**
  - `enclave doctor --repair` resolves an interrupted lifecycle transition rather than
    only reporting it.
  - Network and cgroup teardown retries transient kernel busy states and reports the
    attempt count, the delay, and the final errno.
  - Published-port connections are bounded per port and in total, so a connection
    flood is refused rather than exhausting threads.

- **Runtime repair and ownership safety**
  - Exclusive state-directory daemon locking with inspectable ownership metadata.
  - `enclave doctor --repair` for mount-first stale-state cleanup and registry reconciliation.
  - Idempotent sandbox and workspace destruction with dead-runtime namespace validation.
- **Host-to-workspace port publishing**
  - Explicit, opt-in TCP publishing from `127.0.0.1:HOST_PORT` on the host to a selected workspace port.
  - Supported through both `Enclavefile` `ports = [...]` declarations and `enclave workspace port ...` commands.
- **Large sandbox lifecycle performance**
  - Batch sandbox shutdown with parallel per-workspace cleanup.
  - Sandbox-local session-helper caching, user-namespace mode caching, host-network readiness caching, and collapsed veth/DNS setup for faster large-sandbox startup.
