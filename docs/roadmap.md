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

4. **Richer operational visibility**
   - Expand runtime inspection around networking, workspace health, and resource usage.
   - Make it easier to understand what changed across many parallel workspaces.

5. **Host capability diagnostics**
   - Surface clearer checks for subordinate ID ranges, idmapped mount support, and optional AppArmor/SELinux profile availability.
   - Make host compatibility failures easier to diagnose before `workspace start`.

## Not planned for v1.0

- Rootless daemon / CLI operation
- VM-style or hardware-backed isolation guarantees
- Cross-platform support beyond Linux kernel primitives

## Recently completed

- **Host-to-workspace port publishing**
  - Explicit, opt-in TCP publishing from `127.0.0.1:HOST_PORT` on the host to a selected workspace port.
  - Supported through both `Enclavefile` `ports = [...]` declarations and `enclave workspace port ...` commands.
- **Large sandbox lifecycle performance**
  - Batch sandbox shutdown with parallel per-workspace cleanup.
  - Sandbox-local session-helper caching, user-namespace mode caching, host-network readiness caching, and collapsed veth/DNS setup for faster large-sandbox startup.
