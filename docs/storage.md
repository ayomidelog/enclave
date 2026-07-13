# Storage Layout

Enclave keeps durable state under its configured `state_dir` and uses a small number of predictable directories for shared root filesystems, per-workspace writable layers, and runtime metadata.

## At a glance

```text
<state_dir>/
├── auth/
│   └── <provider>.token         # Stored provider tokens (0600)
├── registry.json                # Sandbox & workspace metadata
├── registry.lock                # Advisory file lock
├── policy.json                  # Authorization rules
├── policy.lock                  # Advisory file lock
└── sandboxes/
    ├── rootfs-cache/
    │   ├── <suite>/             # Suite-specific cached rootfs
    │   └── base/                # Optional generic cached rootfs
    └── <sandbox-id>/
        ├── rootfs/              # Sandbox root filesystem on disk
        ├── runtime/rootfs.mnt/  # Active mount point used while running
        ├── runtime/session-helper # Cached sandbox-local copy of the internal session helper
        ├── home-base/           # Shared lower layer for workspace home overlays
        └── workspaces/<workspace-id>/
            ├── fs/              # Default workspace source directory mounted at /home via idmapped bind mount
            ├── home-upper/      # Workspace-specific overlay writes
            ├── home-work/       # OverlayFS work directory
            ├── home-merged/     # OverlayFS merged mount
            ├── ns/              # Namespace reference files
            ├── runtime/         # PID, logs, readiness, and other runtime state
            └── snapshots/<name>/
                └── home-upper/  # Snapshot copy of workspace overlay data
```

## Root filesystem data

- `sandboxes/rootfs-cache/` stores reusable source root filesystems.
  - `debootstrap` automatically populates a suite-specific cache like `bookworm/` after a successful bootstrap.
  - `cached_rootfs` can copy from either a suite-specific cache or the generic `base/` cache.
- `sandboxes/<sandbox-id>/rootfs/` is the sandbox's on-disk root filesystem.
- `sandboxes/<sandbox-id>/runtime/rootfs.mnt/` is the active mount point used while the sandbox is running.
- `sandboxes/<sandbox-id>/runtime/session-helper` caches the internal helper binary once per sandbox so workspace starts do not recopy it for every workspace.

## Workspace writable data

Each workspace starts from the shared sandbox rootfs, but its writable home area is isolated:

- `home-base/` is the shared lower layer for workspace home directories.
- `workspaces/<workspace-id>/home-upper/` contains writes made by that workspace.
- `workspaces/<workspace-id>/home-work/` is the OverlayFS bookkeeping directory.
- `workspaces/<workspace-id>/home-merged/` is the merged OverlayFS view.
- `workspaces/<workspace-id>/fs/` is the default workspace source directory mounted into `/home` inside the workspace.
- If `workspace_dir` is configured, Enclave mounts that directory instead.
- In both cases, `/home` is presented through an idmapped bind mount rather than a raw host bind.

## Runtime and snapshot data

- `workspaces/<workspace-id>/runtime/` stores runtime metadata such as PID, logs, and readiness markers.
- `workspaces/<workspace-id>/snapshots/` stores copy-based workspace snapshots.
- `snapshot export` packages one of those snapshot directories as a tar or tar.gz archive for transfer or backup.
- Snapshot data is currently a full copy of the workspace overlay data, which is why snapshot retention matters for disk usage.

## Auth data

- Provider tokens are stored on the host at `<state_dir>/auth/<provider>.token`.
- When a workspace starts, Enclave copies only the declared providers into a namespace-private tmpfs mounted at `/run/enclave/auth` inside the workspace rootfs.
- Declared `env_tokens` are copied into a separate namespace-private tmpfs mounted at `/run/enclave/env` inside the workspace rootfs.
- This keeps the persisted host-side token store separate from the workspace runtime mount.
