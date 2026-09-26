#!/usr/bin/env bash
# Where a lifecycle request's time goes, read from a daemon log.
#
# With ENCLAVE_PERF=1 the daemon writes one line per timed phase:
#
#   timing operation=<id> phase=<name> elapsed_us=<microseconds>
#
# A phase's share of the request is the answer to "what would I get for making this
# faster", which a total alone cannot give: two phases that each cost a third of a
# request are worth different work than one that costs two thirds. The request total is
# the daemon.request phase, so the shares are of the number a client sees.
#
# Usage: phases.sh DAEMON_LOG [PHASE_PREFIX]
#
# The optional prefix limits the report to the phases of one operation, for example
# workspace.start.
set -euo pipefail

if (($# < 1)); then
  printf 'usage: %s DAEMON_LOG [PHASE_PREFIX]\n' "$0" >&2
  exit 2
fi

log=$1
prefix=${2:-}
if [[ ! -f "$log" ]]; then
  printf 'error: no daemon log at %s\n' "$log" >&2
  exit 1
fi

python3 - "$log" "$prefix" <<'PY'
import re
import statistics
import sys

path, prefix = sys.argv[1], sys.argv[2]
# The lines are written to stderr and the daemon may interleave its own, so the phase
# lines are picked out rather than parsed positionally.
pattern = re.compile(r"operation=(\S*) phase=([a-z._]+) elapsed_us=(\d+)")

# Every phase is recorded, including the request total, and the prefix only decides which
# phases are reported. Filtering as the lines are read would drop the total as well and
# leave each share measured against the largest phase in its own block, which reads as
# "this phase is all of the request" for whichever phase happened to be largest.
blocks = []
by_operation = {}
for line in open(path, errors="replace"):
    match = pattern.search(line)
    if not match:
        continue
    operation, phase, elapsed = match.group(1), match.group(2), int(match.group(3))
    key = operation or f"unattributed-{len(blocks)}"
    if operation:
        block = by_operation.get(key)
        if block is None:
            block = {}
            by_operation[key] = block
            blocks.append(block)
    else:
        block = {}
        blocks.append(block)
    block[phase] = elapsed

reported = [
    block
    for block in blocks
    if any(phase.startswith(prefix) for phase in block) or not prefix
]
if not reported:
    suffix = f" for {prefix}" if prefix else ""
    print(f"no phase lines in {path}{suffix}")
    raise SystemExit(1)

# The request total is the phase the client waits on. A block without one still has
# its phases reported, with the largest as the denominator.
totals = [b["daemon.request"] for b in reported if "daemon.request" in b]
print(f"requests={len(reported)}")
if totals:
    ordered = sorted(totals)

    def rank(percent):
        return ordered[min(len(ordered) - 1, int((percent / 100) * (len(ordered) - 1)))]

    print(f"request_p50_us={int(statistics.median(ordered))}")
    print(f"request_p95_us={rank(95)}")
    print(f"request_p99_us={rank(99)}")
    print(f"request_max_us={ordered[-1]}")

phases = {}
for block in reported:
    denominator = block.get("daemon.request") or max(block.values())
    for phase, elapsed in block.items():
        if phase == "daemon.request":
            continue
        if prefix and not phase.startswith(prefix):
            continue
        phases.setdefault(phase, []).append((elapsed, denominator))

# Sorted by the phase that contributes the most time to a request on average, which is
# the order the phases are worth looking at.
rows = []
for phase, samples in phases.items():
    elapsed = [value for value, _ in samples]
    share = statistics.mean(value / denominator for value, denominator in samples) * 100
    rows.append((share, phase, statistics.median(elapsed), len(samples)))
rows.sort(reverse=True)

print()
print(f"{'phase':32s} {'median_us':>10s} {'share':>7s} {'samples':>8s}")
for share, phase, median, count in rows:
    print(f"{phase:32s} {int(median):10d} {share:6.1f}% {count:8d}")
PY
