#!/usr/bin/env bash
# Fail when the published lifecycle report predates the code that produces it.
#
# The report is a measurement, and it can only be taken on a privileged host, so
# it is committed rather than regenerated in CI. What CI can check is that nobody
# changed the generator after the report was last written: a report whose script,
# template, or shared metadata helper has moved on carries numbers that no longer
# describe the command a reader would run to reproduce them.
#
# The comparison is on commit times, not file modification times. A fresh checkout
# stamps every file with the checkout time, so `find -newer` between two committed
# files is decided by the order the checkout happened to write them in, and fails
# on a tree nobody touched. Commit times are the durable answer, which means this
# needs full history: the workflow checks out with `fetch-depth: 0`.
#
# Usage: check-lifecycle-report.sh
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
cd "$repo_dir"

report=docs/lifecycle-report.md
# common.sh is a generator too: it is where the host metadata in the report is
# read, so a change to what the report says about its host makes it stale.
generators=(
  tools/perf/lifecycle-report.sh
  tools/perf/lifecycle-report.tpl
  tools/perf/common.sh
)

if [[ ! -s $report ]]; then
  printf 'error: %s is missing or empty\n' "$report" >&2
  exit 1
fi

if [[ $(git rev-parse --is-shallow-repository 2>/dev/null) == true ]]; then
  printf 'error: this check compares commit times and needs full history\n' >&2
  printf '  check out with fetch-depth: 0\n' >&2
  exit 1
fi

committed_at() {
  git log -1 --format=%ct -- "$1" 2>/dev/null || true
}

report_time=$(committed_at "$report")
if [[ -z $report_time ]]; then
  printf 'error: %s has never been committed\n' "$report" >&2
  exit 1
fi

newest_time=0
newest_generator=
for generator in "${generators[@]}"; do
  time=$(committed_at "$generator")
  [[ -z $time ]] && continue
  if ((time > newest_time)); then
    newest_time=$time
    newest_generator=$generator
  fi
done

if ((newest_time > report_time)); then
  printf 'error: %s was changed after %s was written\n' "$newest_generator" "$report" >&2
  printf '  %s last changed %s\n' \
    "$newest_generator" "$(date -u -d "@$newest_time" '+%Y-%m-%d %H:%M:%SZ')" >&2
  printf '  %s last written %s\n' \
    "$report" "$(date -u -d "@$report_time" '+%Y-%m-%d %H:%M:%SZ')" >&2
  printf '  re-take it on a privileged host and commit the result:\n' >&2
  printf '    ENCLAVE_LIVE_ITERATIONS=12 %s\n' "${generators[0]}" >&2
  exit 1
fi

printf 'lifecycle report is current: %s is at least as new as every generator\n' "$report"
