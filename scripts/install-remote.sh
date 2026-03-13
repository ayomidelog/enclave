#!/usr/bin/env sh
set -eu




REPO="https://github.com/ayomidelog/enclave.git"
INSTALL_DIR="/usr/local/bin"
TMP_DIR=""

info() { printf '[enclave] %s\n' "$*"; }
warn() { printf '[enclave][warn] %s\n' "$*" >&2; }
err()  { printf '[enclave][error] %s\n' "$*" >&2; exit 1; }

cleanup() {
  if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT


command -v git  >/dev/null 2>&1 || err "git is required but not found"
command -v cargo >/dev/null 2>&1 || err "cargo is required but not found. Install Rust: https://rustup.rs"


case "$(uname -s)" in
  Linux) ;;
  *) err "Enclave only supports Linux" ;;
esac

info "Cloning Enclave..."
TMP_DIR="$(mktemp -d)"
if ! git clone --depth 1 "$REPO" "$TMP_DIR/enclave"; then
  err "Failed to clone repository"
fi

info "Building (release mode)..."
if ! cargo build --release --manifest-path "$TMP_DIR/enclave/Cargo.toml"; then
  err "Build failed"
fi

BINARY="$TMP_DIR/enclave/target/release/enclave"
if [ ! -x "$BINARY" ]; then
  err "Build succeeded but binary not found at $BINARY"
fi

info "Installing to $INSTALL_DIR/enclave..."
if [ "$(id -u)" -eq 0 ]; then
  install -m 0755 "$BINARY" "$INSTALL_DIR/enclave"
elif command -v sudo >/dev/null 2>&1; then
  sudo install -m 0755 "$BINARY" "$INSTALL_DIR/enclave"
else
  err "Root access required to install to $INSTALL_DIR. Run with sudo."
fi

info "Checking runtime dependencies..."
for dep in debootstrap unshare nsenter ip; do
  if ! command -v "$dep" >/dev/null 2>&1; then
    warn "missing: $dep — install with: sudo apt-get install -y debootstrap util-linux iproute2"
  fi
done

info "Installed successfully!"
info ""
info "  enclave --help"
info ""
