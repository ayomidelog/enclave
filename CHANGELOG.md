# Changelog

## Unreleased

### Added
- `enclave rootfs export`, `enclave rootfs import`, and `enclave rootfs fetch` for distributing prebuilt cached rootfs archives.
- Documentation for hosting a prebuilt rootfs archive on GitHub Releases and reusing it with `bootstrap_method = "cached_rootfs"`.

### Changed
- Workspace startup now fails closed if networking is not actually ready, including missing default-route validation.
- `enclave up`/`enclave down` auto-start paths now respect project configuration more consistently and are less dependent on manual runtime-dir ownership fixes.
- Runtime workspace files are written against the live workspace root instead of the shared sandbox rootfs.

### Fixed
- Registry mutation no longer silently replaces invalid registry data with an empty registry.
- Session helper resolution now works correctly for library/test-driven startup flows.
- Auth provider visibility now reflects only usable configured tokens.
