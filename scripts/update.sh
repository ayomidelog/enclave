#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SKIP_PULL=0
RESTART_DAEMON=1

log_info() {
  echo "[update] $*"
}

log_warn() {
  echo "[update][warn] $*" >&2
}

log_error() {
  echo "[update][error] $*" >&2
}

run_as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  else
    sudo "$@"
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --no-pull)
      SKIP_PULL=1
      ;;
    --no-restart)
      RESTART_DAEMON=0
      ;;
    *)
      log_error "unknown argument: $1"
      echo "usage: ./scripts/update.sh [--no-pull] [--no-restart]" >&2
      exit 1
      ;;
  esac
  shift
done

log_info "Starting Enclave update"
log_info "Repo: $REPO_DIR"

if ! command -v git >/dev/null 2>&1; then
  log_error "git is required but was not found on PATH"
  exit 1
fi

if [ "$SKIP_PULL" -eq 0 ]; then
  log_info "Step 1/4: Pulling latest code (fast-forward only)"
  if ! git -C "$REPO_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    log_error "$REPO_DIR is not a git repository"
    exit 1
  fi

  if ! git -C "$REPO_DIR" diff --quiet || ! git -C "$REPO_DIR" diff --cached --quiet; then
    log_error "working tree has local changes; commit or stash before update"
    exit 1
  fi

  if ! git -C "$REPO_DIR" pull --ff-only; then
    log_error "git pull --ff-only failed"
    exit 1
  fi
else
  log_info "Step 1/4: Skipping git pull (--no-pull)"
fi

DAEMON_WAS_RUNNING=0
if command -v enclave >/dev/null 2>&1; then
  if run_as_root enclave daemon status >/dev/null 2>&1; then
    DAEMON_WAS_RUNNING=1
  fi
fi

if [ "$DAEMON_WAS_RUNNING" -eq 1 ] && [ "$RESTART_DAEMON" -eq 1 ]; then
  log_info "Step 2/4: Stopping running daemon"
  run_as_root enclave daemon stop >/dev/null 2>&1 || true
else
  log_info "Step 2/4: Daemon stop skipped"
fi

log_info "Step 3/4: Reinstalling Enclave"
"$REPO_DIR/scripts/install.sh"

if [ "$DAEMON_WAS_RUNNING" -eq 1 ] && [ "$RESTART_DAEMON" -eq 1 ]; then
  log_info "Step 4/4: Restarting daemon"
  run_as_root enclave daemon start
else
  log_info "Step 4/4: Daemon restart skipped"
  log_info "Start manually if needed: sudo enclave daemon start"
fi

log_info "Update complete"
