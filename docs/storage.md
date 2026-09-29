# Storage Layout

Enclave keeps durable state under its configured `state_dir` and uses a small number of predictable directories for shared root filesystems, per-workspace writable layers, and runtime metadata.

## At a glance

```text
<state_dir>/
├── auth/
│   ├── <provider>.token         # Shared-namespace provider tokens (0600)
│   ├── audit.log                # Store, revoke, and inject events (0600)
│   └── users/
│       └── <user_id>/
│           └── <provider>.token # One user's tokens (0600, in a 0700 directory)
├── registry.json                # Compact sandbox & workspace metadata index
├── registry.lock                # Advisory file lock
├── daemon.lock                  # Exclusive daemon owner record while the daemon runs
├── policy.json                  # Authorization rules
├── policy.lock                  # Advisory file lock
└── sandboxes/
    ├── rootfs-cache/
    │   ├── <suite>/             # Suite-specific cached rootfs
    │   └── base/                # Optional generic cached rootfs
    └── <sandbox-id>/
        ├── rootfs/              # Sandbox root filesystem (overlay mount when a shared base is used)
        ├── rootfs-upper/        # Sandbox-specific rootfs writes (shared-base sandboxes)
        ├── rootfs-work/         # OverlayFS work directory for the rootfs overlay
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
  - `cached_rootfs` uses either a suite-specific cache or the generic `base/` cache.

### Shared base layer

When a sandbox is created from a cached rootfs, `rootfs/` is an OverlayFS mount
whose lower layer is the cache directory and whose upper and work directories
live in the sandbox directory. Nothing is copied, so creation cost does not
scale with the number of files in the cached rootfs.

The cache is only ever a lower layer, so writes made through a sandbox rootfs
(setup commands, package installs, or anything else) land in that sandbox's
`rootfs-upper/` and are invisible to every other sandbox and to the cache
itself. `enclave sandbox status` reports the size of the sandbox's own writes.

The overlay is mounted for the sandbox's whole lifetime, running or not, and is
remounted by the daemon on startup, so it survives a host reboot. If the mount
cannot be created, sandbox creation falls back to copying the cached rootfs.
  - `rootfs-cache/index.json` records indexed cache entries, suite/source metadata, architecture, creation time, tool version, content digest, and required-directory fingerprints so bootstrap avoids repeated recursive discovery while still detecting stale cache roots.
- `sandboxes/<sandbox-id>/rootfs/` is the sandbox's on-disk root filesystem.
- `sandboxes/<sandbox-id>/runtime/rootfs.mnt/` is the active mount point used while the sandbox is running.
- `sandboxes/<sandbox-id>/runtime/session-helper` caches the internal helper binary once per sandbox so workspace starts do not recopy it for every workspace.
- `registry.json` is written as compact JSON to reduce serialization and atomic-write overhead; use the per-sandbox and per-workspace metadata files for human-readable inspection.

### Registry schema versions

`registry.json` carries a `version` field. A record written by an older release is
brought up to the schema this binary writes as it is read, one step per version,
so every path that loads the registry sees the same shape. Migration is in
memory: a read-only command never rewrites the record, and the migrated version
is persisted by the next mutation, which already writes the whole file
atomically. A record whose version is newer than this binary supports is refused
before anything is read or written, and a mutation refuses to write a version
this binary does not define, so a state file it could not read is never created.

An older record that has no migration step is an error rather than a pass
through. That is deliberate: it forces a new schema version to come with the
code that transforms the previous one, instead of being read as if the shapes
matched.

## Workspace writable data

Each workspace starts from the shared sandbox rootfs, but its writable home area is isolated:

- `home-base/` is the shared lower layer for workspace home directories.
- `workspaces/<workspace-id>/home-upper/` contains writes made by that workspace.
- `workspaces/<workspace-id>/home-work/` is the OverlayFS bookkeeping directory.
- `workspaces/<workspace-id>/home-merged/` is the merged OverlayFS view.
- `workspaces/<workspace-id>/fs/` is the default workspace source directory mounted into `/home` inside the workspace.
- When `disk_mb` is configured, that `fs/` mount target is backed by the workspace's `fs.img` loop-mounted ext4 image, and the workspace-private `/tmp` is bind-mounted from `.enclave-tmp/` on the same filesystem so both paths consume the same quota. The backing directory is hidden so it cannot be reached from inside the workspace: the workspace filesystem is mounted at `/home`, so a visible entry there would be the same directory as the `/tmp` mount, and removing it would leave `/tmp` pointing at an unlinked directory. `enclave doctor` reports a workspace whose `/tmp` is in that state and a workspace restart repairs it.
- When `disk_mb` is configured, the workspace also mounts an OverlayFS root whose upper and work directories live on that same ext4 image. Writable paths such as `/opt`, `/var`, `/etc`, and `/root` therefore consume the workspace quota while the shared sandbox rootfs remains the read-only lower layer.
- Managed workspace `/tmp` can be cleared after a successful workspace or sandbox stop by setting `clear_tmp_on_restart = true` in the workspace's Enclavefile section. The setting is opt-in. If mount cleanup fails, Enclave leaves the directory intact and logs the cleanup error rather than deleting through a potentially live mount.
- Quota-backed workspace storage is initialized and mounted when the workspace is created so the returned `filesystem_path` always refers to the actual ext4 data volume, even before the runtime is started.
- An existing quota-backed allocation can be changed with `enclave workspace resize <sandbox> <workspace> --disk-mb N`; the command resizes both the sparse image and its ext4 filesystem and updates the persisted workspace limit. A workspace created without `disk_mb` has no managed disk, so a disk resize is refused for it, but its memory limit is independent of storage and can still be changed.
- Growing and shrinking are the same two steps in opposite order. A grow enlarges the image file first, because the filesystem cannot be resized past the device it lives on. A shrink shrinks the filesystem first, because truncating the file first would cut the filesystem off mid-block. Either way the result is verified from the ext4 superblock rather than from the image file size, so an image and a filesystem that disagree are reported instead of accepted.
- A shrink is refused before anything is written when the filesystem holds more data than the target. The floor comes from `resize2fs -P`, so the message names the smallest allocation that would work rather than a tool refusal to decode.
- A resize that cannot finish restores the image to the size it had, so a workspace is never left with an image that disagrees with its filesystem.
- Resizing a running workspace temporarily stops and restarts its runtime through the normal lifecycle cleanup and hardening path. Every reason a request could be refused is checked before that stop, so a refused resize leaves the workspace running.
- A sandbox has no image of its own: its rootfs is a shared lower layer on the host filesystem. A sandbox disk size is therefore a budget, `enclave resize <sandbox> --disk-mb N`, measured against the sum of its workspaces' `disk_mb` allocations. It is enforced where an allocation is granted, so a workspace cannot be created or grown past it, and a budget below what the sandbox already allocates is refused rather than stored.
- `--no-disk-budget` removes the budget so its workspaces may allocate any size, and `--no-memory-limit` removes a memory limit. Both are flags rather than a size of zero, which is refused as a size that cannot work.
- If `workspace_dir` is configured, Enclave mounts that directory instead.
- Host-backed `workspace_dir`/`path` workspaces do not use the quota-backed root overlay and remain subject to the host directory's storage policy.
- In both cases, `/home` is presented through an idmapped bind mount rather than a raw host bind.
- `enclave workspace cp` operates on the live mounted workspace filesystem. It is a streaming transfer and does not create a separate archive or durable copy in Enclave state.

### What the quota-backed backend costs

`disk_mb` selects a different storage backend rather than a size on the same one:
the workspace's `/home`, its private `/tmp`, and its root OverlayFS writes all
live on a sparse ext4 image attached to a loop device, instead of on a directory
in the state tree. That buys enforced quota, and it costs the loop attach, the
mount, and the overlay setup on every start.

Measured on the validation host (AMD EPYC, kernel 6.8.0, ext2/ext3 state
directory), with a 256 MiB quota against a default directory-backed workspace in
the same sandbox, one sample each and no special tuning:

| step | directory-backed | quota-backed |
|---|---|---|
| create | 173 ms | 438 ms |
| first start | 21 ms | 21 ms |
| stop after create | 74 ms | 109 ms |
| start after a stop | 114–138 ms | 140–169 ms |
| stop after a start | 101–106 ms | 97–108 ms |
| destroy | 40 ms | 49 ms |

Two of those rows are worth reading carefully. Creation is the expensive step,
because it is where `mkfs.ext4` runs; a start that finds the image already
attached skips the mount entirely, which is why the first start matches the
directory tier. Every start after a stop attaches the image again, which is the
55 ms difference in that row, and it is the number to compare against a pause and
resume rather than against a cold boot.

Cleanup is the other half of a backend's behavior. A stop unmounts the image and
detaches its loop device; the kernel releases an autoclear device 22–48 ms after
the unmount on this host, and Enclave waits for that release and verifies it
rather than reporting success on the unmount alone. `enclave workspace destroy`
proves the same thing through its certificate, so a workspace whose loop device
survived is reported as an incomplete cleanup rather than a completed one.
`enclave doctor` reports any loop device still backing an image under the state
directory. `tools/perf/live-quota.sh` runs the whole sequence and fails if a loop
device is left behind.

## Runtime and snapshot data

- `workspaces/<workspace-id>/runtime/` stores runtime metadata such as PID, logs, and readiness markers. Persistent `workspace exec` diagnostics are appended to `session-helper.log`; the helper socket itself uses a short private path under `/run/enclave` and is removed when the runtime identity is invalidated.
- `workspaces/<workspace-id>/ns/` stores mount and PID namespace references while a runtime is active. Enclave validates these references with the recorded runtime PID and start time before treating a workspace as active.
- `daemon.lock` is held exclusively while the daemon owns the state directory. Its JSON record identifies the daemon PID, socket, binary version, binary path, and start time; the file is removed when the owning daemon exits normally.
- `workspaces/<workspace-id>/snapshots/` stores copy-based workspace snapshots.
- `snapshot export` packages one of those snapshot directories as a tar or tar.gz archive for transfer or backup.
- Snapshot data is currently a full copy of the workspace overlay data, which is why snapshot retention matters for disk usage.
- Regular snapshot files use reflink or `copy_file_range` acceleration when the filesystem supports it, while preserving the existing staged copy semantics.

## Repairing stale state

Use `enclave doctor --repair` after a host reboot, daemon crash, or interrupted cleanup. It validates daemon ownership, removes stale nested workspace mounts before filesystem reconciliation, clears dead namespace state, and reconciles registry records with safe on-disk sandbox and workspace data. It does not remove a directory while it or one of its descendants is still mounted.

A `workspace start` on a record whose runtime is gone releases that runtime's host resources before it launches anything, using the same verified teardown a stop runs. The kernel destroys a veth pair when the network namespace dies with its process, but the anti-spoofing rules that name it, the cgroup, and the storage mounts all outlive it, and a start is the command an operator runs first after a crash. A start that cannot release the old resources fails rather than stacking a second runtime on top of them.

Enclave unmounts only mounts it created, and it decides that from the mount itself rather than from its path. Every mount Enclave makes below the state directory is one of three things: an OverlayFS mount, a bind mount whose mount root lies inside the state directory, or a loop device backed by a file inside the state directory. A mount matching none of those is treated as foreign. `doctor` reports foreign mounts separately, `doctor --repair` leaves them in place, and a workspace or sandbox destroy refuses to delete a directory while a foreign mount is below it, because removing the directory would recurse into that mount and delete files Enclave did not create. `enclave doctor --repair` reports what it left behind in `foreign_stale_mounts`.

## Auth data

- Provider tokens are stored on the host at `<state_dir>/auth/<provider>.token`,
  or, for a workspace with an `owner`, at
  `<state_dir>/auth/users/<owner>/<provider>.token`.
- The file name is a provider name or the slot an environment token derives from
  its variable name (`NETFLIX_PASSWORD` reads `netflix-password`), so a credential
  Enclave has no provider for is stored and injected the same way one it does. A
  leading `_` in the variable name is kept in the slot, so a slot never begins
  with `-`.
- A token file is only read after its ownership and mode are checked: a regular
  file, not a symlink, owned by the effective uid, mode `0600`. A user's
  namespace directory is additionally required to be `0700` before it is read
  from, so a directory anyone could have replaced is refused rather than trusted.
- Every store, revoke, and inject appends one JSON line to
  `<state_dir>/auth/audit.log`. The event names the action, the namespace, the
  provider, and the workspace, and never the token value, so the log can be read
  and shipped without handling a secret.
- When a workspace starts, Enclave copies only the declared providers into a namespace-private tmpfs mounted at `/run/enclave/auth` inside the workspace rootfs.
- Declared `env_tokens` are copied into a separate namespace-private tmpfs mounted at `/run/enclave/env` inside the workspace rootfs.
- Those files are rewritten from the store before every command the daemon runs,
  not only when the workspace starts, so a revoked token stops being exported on
  the next command.
- This keeps the persisted host-side token store separate from the workspace runtime mount.
