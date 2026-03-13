# Limitations

Enclave is intentionally scoped to local Linux development workflows. These are the current product limitations to plan around:

## Isolation and trust boundaries

- **Requires root**: namespace setup requires root privileges. User namespace (rootless) support is not planned for v1.0.
- **Not a VM boundary**: Enclave provides process-level isolation with Linux namespaces and OverlayFS. It does not provide hardware virtualization or protection from a malicious host root user.
- **Setup phase is weaker than runtime**: workspace runtime hardening uses user namespaces, seccomp, capability dropping, and read-only remounts, but sandbox setup commands still run as root in a plain `chroot`.
- **Host LSM policy content is external**: Enclave can request AppArmor or SELinux confinement for workspaces, but the actual host profiles/labels must already exist and are not shipped by Enclave.

## Networking and access control

- **TCP loopback publishing only**: v1 host access is limited to explicit `127.0.0.1` TCP publishes. UDP and non-loopback/public binds are not supported.
- **No first-class service networking**: workspaces share the Enclave bridge, but direct workspace-to-workspace forwarding is blocked by default. There is no built-in service discovery, service mesh, or allow-list workflow for selectively re-enabling cross-workspace traffic.
- **UID-based policy only**: the policy engine operates per-UID. Per-workspace or per-sandbox ACLs are not yet supported.

## Storage and lifecycle

- **Copy-based snapshots**: snapshots are full directory copies. Use `enclave workspace snapshot-gc` to enforce retention and reclaim disk space.
- **Modern mount support required**: workspace `/home` mounts now rely on idmapped bind mounts. Hosts must provide a kernel and `mount` implementation with `X-mount.idmap` support.
- **Mixed-ownership host trees are less predictable**: host-backed `workspace_dir` mounts work best when the project tree has a consistent owner/group at the root. Files owned by unrelated host IDs may appear as overflow IDs inside the workspace.
- **No per-workspace disk quota yet**: Enclave does not currently enforce filesystem quotas for workspace storage. A large workspace can still consume host disk space until normal filesystem capacity limits are reached.
- **Linux only**: Enclave depends on Linux namespace, mount, and networking primitives. macOS and Windows are not supported.

See [Roadmap](roadmap.md) for the features planned to address some of these gaps.
