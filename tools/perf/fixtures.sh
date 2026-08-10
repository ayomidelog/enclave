#!/usr/bin/env bash
set -euo pipefail

output_dir=${1:-}
if [[ -z "$output_dir" ]]; then
  printf 'usage: %s OUTPUT_DIR\n' "$0" >&2
  exit 2
fi

mkdir -p "$output_dir/small" "$output_dir/many" "$output_dir/deep" "$output_dir/sparse"
many_count=${ENCLAVE_PERF_MANY_FILES:-100000}
printf 'enclave performance fixture\n' >"$output_dir/small/4k.txt"
truncate -s 4096 "$output_dir/small/4k.bin"
truncate -s 1M "$output_dir/small/1m.bin"
truncate -s 1G "$output_dir/sparse/1g.sparse"
truncate -s 5G "$output_dir/sparse/5g.sparse"

for index in $(seq -w 1 "$many_count"); do
  truncate -s 4096 "$output_dir/many/file-$index.bin"
done

path="$output_dir/deep"
for depth in $(seq 1 10); do
  path="$path/level-$depth"
  mkdir -p "$path"
done
printf 'deep fixture\n' >"$path/file.txt"

printf 'fixture_dir=%s\nsmall_4k=%s\nsmall_1m=%s\nsparse_1g=%s\nsparse_5g=%s\nmany_files=%s\ndeep_levels=10\n' \
  "$output_dir" \
  "$output_dir/small/4k.bin" \
  "$output_dir/small/1m.bin" \
  "$output_dir/sparse/1g.sparse" \
  "$output_dir/sparse/5g.sparse" \
  "$many_count"
