# Configuration

Enclave supports an optional TOML configuration file for setting defaults that apply to all commands.

## File Location

The configuration file is loaded from the first path that exists:

1. Explicit path via `--config` flag: `enclave --config /path/to/config.toml ...`
2. `$XDG_CONFIG_HOME/enclave/config.toml`
3. `$HOME/.config/enclave/config.toml`

If no configuration file is found, Enclave uses built-in defaults.

## Example

```toml
# Override defaults when running as root — for example, to store state on a data volume
socket = "/run/enclave/manager.sock"
state_dir = "/opt/enclave/state"
pid_file = "/run/enclave/manager.pid"
debootstrap_binary = "debootstrap"
workspace_apparmor_profile = "enclave-workspace"
workspace_selinux_label = "system_u:system_r:container_t:s0"
suite = "bookworm"
mirror = "http://deb.debian.org/debian"
wait_secs = 5
```

## Options

Since Enclave requires root privileges, the defaults below are the values used when running as root (UID 0). Non-root paths follow the XDG Base Directory convention.

| Key | Default (root) | Description |
|-----|----------------|-------------|
| `socket` | `/run/enclave/manager.sock` | Path to the daemon Unix socket. Non-root: `$XDG_RUNTIME_DIR/enclave/manager.sock`. |
| `state_dir` | `/root/.local/state/enclave` | Directory where Enclave stores all sandbox and workspace data. Non-root: `$XDG_STATE_HOME/enclave` or `$HOME/.local/state/enclave`. |
| `pid_file` | `/run/enclave/manager.pid` | Path to the daemon PID file. Non-root: `$XDG_RUNTIME_DIR/enclave/manager.pid`. |
| `debootstrap_binary` | `debootstrap` | Name or path of the `debootstrap` binary. |
| `workspace_apparmor_profile` | unset | Optional AppArmor profile name applied to workspace runtime helpers via `setpriv`. The profile must already exist on the host. |
| `workspace_selinux_label` | unset | Optional SELinux label applied to workspace runtime helpers via `setpriv`. The label must already exist and be valid on the host. |
| `suite` | `bookworm` | Default Debian suite for sandbox creation. |
| `mirror` | `http://deb.debian.org/debian` | Default Debian mirror URL. |
| `bootstrap_method` | `debootstrap` | Default bootstrap method for sandbox creation (`debootstrap` or `cached_rootfs`). |
| `wait_secs` | `5` | Seconds to wait for the daemon to start during `daemon start`. |

## CLI Flag Overrides

Some options can also be set via CLI flags, which take precedence over the config file:

```bash
enclave --config /path/to/config.toml daemon start
enclave daemon start --state-dir /custom/state
enclave daemon start --pid-file /custom/pid
enclave daemon start --debootstrap-binary /usr/sbin/debootstrap
enclave daemon start --workspace-apparmor-profile enclave-workspace
enclave daemon start --workspace-selinux-label 'system_u:system_r:container_t:s0'
enclave daemon start --wait-secs 10
enclave create mybox --bootstrap-method cached_rootfs
```

## State Directory Layout

The `state_dir` contains all Enclave state:

```text
<state_dir>/
├── registry.json      # Sandbox & workspace metadata
├── registry.lock      # Advisory file lock
├── policy.json        # Authorization rules
├── policy.lock        # Advisory file lock
└── sandboxes/         # Sandbox data directories
```

See [Architecture](architecture.md) for the full state layout.
