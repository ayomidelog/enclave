# Changelog

## Unreleased

### Added
- Portable workspace snapshot archive support via `enclave snapshot export` and `enclave snapshot import`.
- `enclave workspace resize` for increasing Enclave-managed ext4 workspace disk allocations without recreating the workspace.
- `enclave workspace cp <sandbox> <workspace> <src> <dst>` for streaming files and directories between the host and a running workspace with `ws:/` workspace paths.
- `enclave doctor --repair` for reconciling stale registry entries, orphaned state directories, dead namespace references, stale workspace mounts, and daemon ownership metadata.

### Changed
- Destructive commands (`destroy`, `remove`, and `wipe`) now require an already-running daemon unless the global `--start-daemon` flag is supplied explicitly.
- Daemons now take an exclusive lock for their state directory and record the owning PID, socket, binary version, binary path, and start time in `daemon.lock`.

### Fixed
- Sandbox and workspace destruction are idempotent when mounts, rootfs data, or runtime artifacts are already absent.
- Cleanup now handles nested workspace mounts deepest-first, lazily detaches mounts owned by dead runtimes, and preserves workspace directories while a mount remains active.
- Registry repair now removes safe invalid-metadata or orphan directories, persists corrected metadata, and avoids deleting paths that still contain mounts.
- Unmount failures now identify the mount target, kernel errno, and namespace processes holding the mount while independent cleanup continues.

## 1.0.2 - 2026-07-12

### Changed
- Repository verification now allows normal explanatory comments and only rejects unresolved `TODO`/`FIXME`/`XXX` markers in tracked Rust, shell, and workflow files.

### Fixed
- Rust CI is green again after the 1.0.1 release by aligning tests and lockfile metadata with the released version.
- Batch-stop session tests now assert the stable contract instead of a CI-sensitive transient PID outcome.
- Stability tests now track the current released version string.

## 1.0.1 - 2026-07-12

### Added
- `enclave rootfs export`, `enclave rootfs import`, and `enclave rootfs fetch` for distributing prebuilt cached rootfs archives.
- Documentation for hosting a prebuilt rootfs archive on GitHub Releases and reusing it with `bootstrap_method = "cached_rootfs"`.

### Changed
- Workspace startup now fails closed if networking is not actually ready, including missing default-route validation.
- `enclave up`/`enclave down` auto-start paths now respect project configuration more consistently and are less dependent on manual runtime-dir ownership fixes.
- Runtime workspace files are written against the live workspace root instead of the shared sandbox rootfs.
- Sandbox shutdown now stops workspace runtimes as a coordinated batch and cleans up per-workspace resources in parallel instead of waiting through serial per-workspace stop windows.
- Existing-workspace startup is substantially faster on large sandboxes by reusing the session helper at the sandbox level, caching host networking readiness, caching user-namespace mode detection, collapsing repeated host/netns veth setup commands, and skipping unchanged DNS file rewrites.

### Fixed
- Registry mutation no longer silently replaces invalid registry data with an empty registry.
- Session helper resolution now works correctly for library/test-driven startup flows.
- Auth provider visibility now reflects only usable configured tokens.
- Large sandbox stop/start paths no longer scale as poorly with workspace count during normal lifecycle operations.
