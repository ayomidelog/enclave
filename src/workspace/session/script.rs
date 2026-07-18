pub(crate) const WORKSPACE_SESSION_SCRIPT: &str = r#"
ROOTFS="$1"
WS_FS="$2"
MOUNT_TARGET="$3"
MNT_REF="$4"
PID_REF="$5"
PID_FILE="$6"
READY_FILE="$7"
CPU_LIMIT="$8"
MEMORY_LIMIT_KB="$9"
PROC_LIMIT="${10}"
NOFILE_LIMIT="${11}"
WORKSPACE_HOSTNAME="${12}"
SESSION_HELPER="${13}"
APPARMOR_PROFILE="${14}"
SELINUX_LABEL="${15}"
WORKSPACE_IDMAP_OPTION="${16}"
DISK_BACKED_TMP="${17}"

log() {
  printf '%s\n' "$*" >&2
}

log "workspace session bootstrap starting"
log "rootfs=$ROOTFS"
log "workspace_fs=$WS_FS"

if [ -n "$CPU_LIMIT" ]; then
  ulimit -t "$CPU_LIMIT" || true
fi
if [ -n "$MEMORY_LIMIT_KB" ]; then
  ulimit -v "$MEMORY_LIMIT_KB" || true
fi
if [ -n "$PROC_LIMIT" ]; then
  if command -v prlimit >/dev/null 2>&1; then
    prlimit --nproc="$PROC_LIMIT" --pid=$$ || true
  else
    echo "prlimit not found; max_processes limit will not be enforced" >&2
  fi
fi
if [ -n "$NOFILE_LIMIT" ]; then
  ulimit -n "$NOFILE_LIMIT" || true
fi

mount --make-rprivate /
if [ -n "$WORKSPACE_HOSTNAME" ]; then
  hostname "$WORKSPACE_HOSTNAME" >/dev/null 2>&1 || true
else
  hostname "workspace" >/dev/null 2>&1 || true
fi
if command -v ip >/dev/null 2>&1; then
  ip link set lo up >/dev/null 2>&1 || true
fi

readlink /proc/self/ns/mnt > "$MNT_REF"
readlink /proc/self/ns/pid > "$PID_REF"
HOST_PID=""
while read -r key val rest; do
  if [ "$key" = "NSpid:" ]; then
    HOST_PID="$val"
    break
  fi
done < /proc/self/status
if [ -z "$HOST_PID" ]; then
  echo "failed to resolve host pid from /proc/self/status" >&2
  exit 1
fi

echo "$HOST_PID" > "$PID_FILE"

log "workspace session bootstrap complete; switching to hardened runtime helper"
BOOTSTRAP_TMP_ARGS=""
if [ "$DISK_BACKED_TMP" = "true" ]; then
  BOOTSTRAP_TMP_ARGS="--disk-backed-tmp"
fi
if [ -n "$APPARMOR_PROFILE" ] || [ -n "$SELINUX_LABEL" ]; then
  setpriv_args="--nnp"
  if [ -n "$APPARMOR_PROFILE" ]; then
    setpriv_args="$setpriv_args --apparmor-profile=$APPARMOR_PROFILE"
  fi
  if [ -n "$SELINUX_LABEL" ]; then
    setpriv_args="$setpriv_args --selinux-label=$SELINUX_LABEL"
  fi
  # shellcheck disable=SC2086
  exec setpriv $setpriv_args "$SESSION_HELPER" internal workspace-session-bootstrap \
    --rootfs "$ROOTFS" \
    --workspace-fs "$WS_FS" \
    --mount-target "$MOUNT_TARGET" \
    --workspace-idmap-option "$WORKSPACE_IDMAP_OPTION" \
    $BOOTSTRAP_TMP_ARGS \
    --ready-file "$READY_FILE"
fi
exec "$SESSION_HELPER" internal workspace-session-bootstrap \
  --rootfs "$ROOTFS" \
  --workspace-fs "$WS_FS" \
  --mount-target "$MOUNT_TARGET" \
  --workspace-idmap-option "$WORKSPACE_IDMAP_OPTION" \
  $BOOTSTRAP_TMP_ARGS \
  --ready-file "$READY_FILE"
"#;

#[cfg(test)]
#[path = "../../../tests/src/workspace/session/script.rs"]
mod tests;
