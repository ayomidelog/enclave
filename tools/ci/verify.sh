#!/usr/bin/env bash
# The unprivileged gate: everything a change has to pass before it is worth
# running the suites that need a host.
#
# This is one definition used twice, by the pull-request workflow and by the
# release workflow, so a release cannot be gated on a weaker check than a branch.
# The pull-request workflow runs the same commands as separate steps so a failure
# names the check that produced it; when you add one here, add it there too.
#
# Usage: verify.sh
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
cd "$repo_dir"

step() {
  printf '\n=== %s ===\n' "$1"
  shift
  "$@"
}

step 'formatting' cargo fmt --all -- --check

# `check` before `clippy` on purpose: a type error reported by rustc is easier to
# read than the same error reported through a lint, and the second command is
# cheap once the first has built the crate.
step 'compiles, all targets' cargo check --all-targets --locked

step 'clippy' cargo clippy --all-targets --locked -- -D warnings

# Documentation is code here: the module docs are where the decisions and the
# measurements behind them are written down, so a broken intra-doc link or an
# unclosed tag is a defect rather than a cosmetic one.
step 'documentation' env RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --locked

# The scripts in scripts/, tools/ci/, and tools/perf/ are part of the product:
# the release workflow and the published benchmarks run them. Warnings are the
# gate; the informational findings are reported by a local run at the default
# severity and are not all worth fixing.
step 'shell scripts' shellcheck -x -S warning scripts/*.sh tools/ci/*.sh tools/perf/*.sh

# `--no-fail-fast` so one failing test does not hide the rest, and the two skipped
# prefixes are the suites that need root; they run in the privileged job.
step 'tests' cargo test --all-targets --locked --no-fail-fast \
  -- --skip integration:: --skip stress::

printf '\nverified\n'
