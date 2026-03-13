#!/usr/bin/env bash
set -euo pipefail

PURGE_DATA=0
if [ "${1:-}" = "--purge-data" ]; then
  PURGE_DATA=1
fi

INSTALL_DIR="/usr/local/bin"
SYSTEM_BIN="$INSTALL_DIR/enclave"
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
CARGO_BIN="$CARGO_BIN_DIR/enclave"

current_uid() {
  id -u
}

run_as_root() {
  if [ "$(current_uid)" -eq 0 ]; then
    "$@"
  else
    sudo "$@"
  fi
}

unmount_sandbox_rootfs_mounts() {
  local sandboxes_root="$1"
  local use_sudo="${2:-0}"
  if [ ! -d "$sandboxes_root" ]; then
    return 0
  fi

  if [ "$use_sudo" -eq 1 ]; then
    while IFS= read -r -d '' mount_path; do
      if sudo mountpoint -q "$mount_path" 2>/dev/null; then
        if command -v findmnt >/dev/null 2>&1; then
          while IFS= read -r nested; do
            [ -n "$nested" ] || continue
            sudo umount "$nested" 2>/dev/null || sudo umount -l "$nested" 2>/dev/null || true
          done < <(sudo findmnt -R -n -o TARGET "$mount_path" 2>/dev/null | sort -r || true)
        else
          sudo umount "$mount_path" 2>/dev/null || sudo umount -l "$mount_path" 2>/dev/null || true
        fi
      fi
    done < <(sudo find "$sandboxes_root" -type d -path "*/runtime/rootfs.mnt" -print0 2>/dev/null || true)
  else
    while IFS= read -r -d '' mount_path; do
      if mountpoint -q "$mount_path" 2>/dev/null; then
        if command -v findmnt >/dev/null 2>&1; then
          while IFS= read -r nested; do
            [ -n "$nested" ] || continue
            umount "$nested" 2>/dev/null || umount -l "$nested" 2>/dev/null || true
          done < <(findmnt -R -n -o TARGET "$mount_path" 2>/dev/null | sort -r || true)
        else
          umount "$mount_path" 2>/dev/null || umount -l "$mount_path" 2>/dev/null || true
        fi
      fi
    done < <(find "$sandboxes_root" -type d -path "*/runtime/rootfs.mnt" -print0 2>/dev/null || true)
  fi
}


if command -v enclave >/dev/null 2>&1; then
  echo "Stopping enclave daemon..."
  run_as_root enclave daemon stop >/dev/null 2>&1 || true
fi


if command -v pgrep >/dev/null 2>&1; then
  PIDS="$(pgrep -x enclave 2>/dev/null || true)"
  if [ -n "$PIDS" ]; then
    echo "Killing lingering enclave processes..."
    for pid in $PIDS; do
      run_as_root kill "$pid" 2>/dev/null || true
    done
    sleep 1

    PIDS="$(pgrep -x enclave 2>/dev/null || true)"
    for pid in $PIDS; do
      run_as_root kill -9 "$pid" 2>/dev/null || true
    done
  fi
fi


BINARY_REMOVED=0

if command -v cargo >/dev/null 2>&1; then
  if cargo uninstall enclave >/dev/null 2>&1; then
    BINARY_REMOVED=1
  fi
fi

if [ -f "$CARGO_BIN" ]; then
  rm -f "$CARGO_BIN"
  BINARY_REMOVED=1
  echo "Removed $CARGO_BIN"
fi

if [ -f "$SYSTEM_BIN" ]; then
  run_as_root rm -f "$SYSTEM_BIN"
  BINARY_REMOVED=1
  echo "Removed $SYSTEM_BIN"
fi

if [ "$BINARY_REMOVED" -eq 0 ]; then
  echo "No enclave binary found to remove"
fi


ROOT_RUNTIME_DIR="/run/enclave"
if [ -n "${XDG_RUNTIME_DIR:-}" ]; then
  USER_RUNTIME_DIR="${XDG_RUNTIME_DIR}/enclave"
else
  USER_RUNTIME_DIR="/tmp/enclave-$(id -u)"
fi

RUNTIME_CLEANED=0
for dir in "$USER_RUNTIME_DIR" "$ROOT_RUNTIME_DIR"; do
  if [ -d "$dir" ]; then
    run_as_root rm -rf "$dir"
    RUNTIME_CLEANED=1
    echo "Removed runtime directory: $dir"
  fi
done

if [ "$RUNTIME_CLEANED" -eq 0 ]; then
  echo "No runtime artifacts found"
fi


if [ "$PURGE_DATA" -eq 1 ]; then
  STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/enclave"
  CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/enclave"
  ROOT_STATE_DIR="/root/.local/state/enclave"
  ROOT_CONFIG_DIR="/root/.config/enclave"

  USER_SANDBOXES_DIR="$STATE_DIR/sandboxes"
  ROOT_SANDBOXES_DIR="$ROOT_STATE_DIR/sandboxes"

  echo "Unmounting sandbox filesystems..."
  unmount_sandbox_rootfs_mounts "$USER_SANDBOXES_DIR" 0
  if [ "$(current_uid)" -eq 0 ]; then
    unmount_sandbox_rootfs_mounts "$ROOT_SANDBOXES_DIR" 0
  else
    unmount_sandbox_rootfs_mounts "$ROOT_SANDBOXES_DIR" 1
  fi

  DATA_PURGED=0
  for dir in "$STATE_DIR" "$CONFIG_DIR"; do
    if [ -d "$dir" ]; then
      rm -rf "$dir"
      DATA_PURGED=1
      echo "Removed $dir"
    fi
  done

  for dir in "$ROOT_STATE_DIR" "$ROOT_CONFIG_DIR"; do
    if [ -d "$dir" ]; then
      run_as_root rm -rf "$dir"
      DATA_PURGED=1
      echo "Removed $dir"
    fi
  done


  for tmp_dir in /tmp/enclave-*; do
    if [ -d "$tmp_dir" ]; then
      run_as_root rm -rf "$tmp_dir"
      DATA_PURGED=1
      echo "Removed $tmp_dir"
    fi
  done


  for run_dir in /run/user/*/enclave; do
    if [ -d "$run_dir" ]; then
      run_as_root rm -rf "$run_dir"
      DATA_PURGED=1
      echo "Removed $run_dir"
    fi
  done

  if [ "$DATA_PURGED" -eq 0 ]; then
    echo "No enclave data found to purge"
  else
    echo "Purged all enclave data"
  fi
else
  echo "Data was not removed. To purge data too, run:"
  echo "  ./scripts/uninstall.sh --purge-data"
fi

echo "Enclave uninstall complete"
