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

## Credentials and command output

- **A token namespace scopes workspaces, not host users**: `owner` is a validated
  name, and Enclave does not check that it corresponds to a host user. The `auth`
  commands run as root and write whichever namespace they are told to, so the
  feature decides which credential a workspace is given, not who is allowed to
  provision one. Treat it as scoping between your own workspaces or agents, not as
  an isolation boundary between mutually untrusting host users.
- **Scrubbing covers captured `workspace exec` output only**: the daemon replaces
  an injected token value in the stdout and stderr it returns. It does not cover an
  interactive `workspace enter` session, a command run with `--no-scrub`, or
  anything a command writes somewhere else — a file, a log of its own, or a
  network request. A workspace holds the token and can always print it; scrubbing
  keeps it out of the paths that travel further than the workspace does, not out of
  the workspace.
- **`workspace exec` buffers, and does not forward standard input**: it runs the
  command through the daemon so the output can be scrubbed, which means output
  arrives when the command finishes rather than as it is written, and only the
  first 16 MiB of it is captured. Use `--no-scrub` for a command that reads
  standard input, streams, needs a terminal, or writes more than that; the
  trade-off is that the output is then printed exactly as the command wrote it.
- **Provider scoping is per workspace, not per command**: a workspace's tokens are
  resolved when it starts, so every command run in it sees the same set. There is
  no way to run one command with a different provider set from the next.
- **A token is replaced, not rotated**: storing a token again overwrites the file,
  and the workspace picks it up on its next start. There is no expiry, no refresh,
  and no overlap window where an old and a new value are both valid.

## Storage and lifecycle

- **Copy-based snapshots**: snapshots are full directory copies. Use `enclave workspace snapshot-gc` to enforce retention and reclaim disk space.
- **Modern mount support required**: workspace `/home` mounts now rely on idmapped bind mounts. Hosts must provide a kernel and `mount` implementation with `X-mount.idmap` support.
- **Mixed-ownership host trees are less predictable**: host-backed `workspace_dir` mounts work best when the project tree has a consistent owner/group at the root. Files owned by unrelated host IDs may appear as overflow IDs inside the workspace.
- **A memory limit is enforced in two places, and both are kept in step**: the cgroup `memory.max` and the workspace session process `RLIMIT_AS`. Changing the limit on a running workspace writes both, so a raise takes effect without a restart. On a host without cgroup v2 the rlimit is the only enforcement, and it is moved the same way, so the limit still changes on a running workspace; what is lost there is the aggregate sandbox cap and the `cpu_percent` share.
- **Per-workspace disk quota requires Enclave-managed workspace storage**: disk quota is supported only when Enclave manages the workspace filesystem itself. On quota-backed workspaces, `/home`, the workspace-private `/tmp`, and root OverlayFS copy-on-write data share that quota-backed filesystem. Host-backed `workspace_dir` / `path` mounts do not support enforced disk quotas. Temporary-directory reset is opt-in through `clear_tmp_on_restart = true`.
- **Managed `/tmp` is repaired by a workspace restart**: the workspace-private `/tmp` is a bind mount of a directory on the workspace filesystem, so it lives and dies with the runtime. `enclave doctor` checks that the mount still references a linked, writable directory and names the affected workspace if it does not; stopping and starting the workspace repairs it.
- **Workspace copy is a basic streaming transfer**: `enclave workspace cp` requires a running workspace and currently supports one host path and one `ws:/` workspace path. Transfers do not overwrite an existing destination entry. `--gzip` compresses directory transfers and `--progress` reports progress on stderr; glob expansion, resumable transfers, and parallel directory transfer are not supported.
- **The Enclave subnet is fixed**: the bridge is `enclave0` and the subnet is `10.200.0.0/24`. Enclave refuses to start on a host whose interfaces already use that subnet rather than choosing another, because the subnet is compiled into the shapes of the firewall rules Enclave compares against the host's rules to prove ownership. A configurable subnet would have to reach every one of those comparisons exactly, or ownership detection starts mis-attributing rules in both directions.
- **Address allocation is host-wide, but the bridge and subnet are not per-daemon**: a workspace address is chosen from the daemon's own registry plus the Enclave interfaces the host already has, so a second daemon with its own state directory cannot hand out an address the first daemon's workspace holds. What is still shared is the `enclave0` bridge and the `10.200.0.0/24` subnet: two daemons on one host put their workspaces on the same network, where they cannot reach each other (cross-workspace traffic is blocked) but do share a bridge. Run one daemon per host unless that sharing is what you want.
- **The published lifecycle numbers are one host's numbers**: `docs/lifecycle-report.md` carries the p50/p95/p99 of the release benchmark together with the kernel, CPU, filesystem, CPU count, and load average it was taken under. The validation host is shared, so the spread is wide and a run on a busy host reports a slower median than a run on an idle one. Compare the medians, and compare them against a run whose host metadata matches.
- **Repair requires daemon ownership**: `enclave doctor --repair` runs through the daemon and validates its state-directory lock. If the daemon is stopped, start it explicitly or use `enclave --start-daemon doctor --repair`.
- **Linux only**: Enclave depends on Linux namespace, mount, and networking primitives. macOS and Windows are not supported.

See [Roadmap](roadmap.md) for the features planned to address some of these gaps.
