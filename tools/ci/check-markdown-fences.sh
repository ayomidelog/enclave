#!/usr/bin/env bash
# Fail on a Markdown file whose code fences do not balance.
#
# An unclosed fence does not fail anything else: the document still renders, it just
# renders wrong, and every line between the two fences is shown as code. That is how a
# README came to swallow fifty lines of prose, and no check noticed because nothing
# reads a document for structure.
#
# A closing fence has to be exactly three backticks with nothing after them. A line that
# opens with a fence and carries an info string is an opener, so a fence that looks
# balanced to a reader scrolling past is caught here.
#
# Usage: check-markdown-fences.sh
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
cd "$repo_dir"

python3 - <<'PY'
import os
import sys

problems = []
for root, dirs, files in os.walk('.'):
    dirs[:] = [d for d in dirs if d not in ('.git', 'target')]
    for name in sorted(files):
        if not name.endswith('.md'):
            continue
        path = os.path.join(root, name)
        in_block = False
        for number, line in enumerate(open(path, errors='replace').read().split('\n'), 1):
            stripped = line.strip()
            if not stripped.startswith('```'):
                continue
            if not in_block:
                in_block = True
            elif stripped == '```':
                in_block = False
            else:
                problems.append(f'{path}:{number}: a closing fence carries {stripped!r}')
        if in_block:
            problems.append(f'{path}: a code fence is never closed')

if problems:
    for problem in problems:
        print(f'error: {problem}', file=sys.stderr)
    sys.exit(1)
print('markdown fences are balanced')
PY
