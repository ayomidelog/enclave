#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DIR="/usr/local/bin"
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
USER_BIN_PATH="$CARGO_BIN_DIR/enclave"
SYSTEM_BIN_PATH="$INSTALL_DIR/enclave"

log_info() {
  echo "[install] $*"
}

log_warn() {
  echo "[install][warn] $*" >&2
}

log_error() {
  echo "[install][error] $*" >&2
}

log_info "Starting Enclave installation"
log_info "Repo: $REPO_DIR"
log_info "User binary path: $USER_BIN_PATH"
log_info "System binary path: $SYSTEM_BIN_PATH"

log_info "Step 1/5: Checking prerequisites"
if ! command -v cargo >/dev/null 2>&1; then
  log_error "cargo is required but was not found on PATH"
  exit 1
fi
log_info "cargo found: $(command -v cargo)"

log_info "Step 2/5: Building Enclave (release)"
cargo build --release --manifest-path "$REPO_DIR/Cargo.toml"

RELEASE_BIN="$REPO_DIR/target/release/enclave"
if [ ! -x "$RELEASE_BIN" ]; then
  log_error "build finished but $RELEASE_BIN is missing"
  exit 1
fi

log_info "Step 3/5: Installing user binary"
mkdir -p "$CARGO_BIN_DIR"
install -m 0755 "$RELEASE_BIN" "$USER_BIN_PATH"
log_info "Installed user binary: $USER_BIN_PATH"

SYSTEM_INSTALLED=0
log_info "Step 4/5: Installing system binary for sudo usage"
if [ -w "$INSTALL_DIR" ] || [ "$(id -u)" -eq 0 ]; then
  install -m 0755 "$RELEASE_BIN" "$SYSTEM_BIN_PATH"
  SYSTEM_INSTALLED=1
elif command -v sudo >/dev/null 2>&1; then
  log_info "Running as non-root; attempting sudo install to $SYSTEM_BIN_PATH"
  if sudo install -m 0755 "$RELEASE_BIN" "$SYSTEM_BIN_PATH"; then
    SYSTEM_INSTALLED=1
  else
    log_warn "failed to install $SYSTEM_BIN_PATH; sudo enclave may not work"
  fi
else
  log_warn "sudo not found, skipping $SYSTEM_BIN_PATH install"
fi

if [ "$SYSTEM_INSTALLED" -eq 1 ]; then
  log_info "Installed system binary: $SYSTEM_BIN_PATH"
fi

log_info "Step 5/5: Installation complete"
case ":$PATH:" in
  *":$CARGO_BIN_DIR:"*)
    log_info "PATH already contains $CARGO_BIN_DIR"
    ;;
  *)
    log_warn "PATH does not include $CARGO_BIN_DIR"
    echo "export PATH=\"$CARGO_BIN_DIR:\$PATH\""
    ;;
esac

log_info "Try:"
echo "enclave --help"
if [ "$SYSTEM_INSTALLED" -eq 1 ]; then
  echo "sudo enclave --help"
else
  log_warn "system install was not completed; sudo enclave may fail"
fi
