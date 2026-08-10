#!/usr/bin/env bash
set -euo pipefail

if (($# == 0)); then
  printf 'usage: %s COMMAND [ARG...]\n' "$0" >&2
  exit 2
fi

if ! command -v strace >/dev/null 2>&1; then
  printf 'strace is required for syscall profiling\n' >&2
  exit 1
fi

exec strace -f -c -- "$@"
