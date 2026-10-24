#!/bin/bash
# The ways each archive is read in CI (see .github/workflows/bench.yml).
set -euo pipefail
ways=(
  "rars decode=bin/rar-perf read {a} sink-handle"
  "rars extract=bin/rar-perf read {a} rars-disk-pipelined"
  "rars extract, path walk=bin/rar-perf-pathwalk read {a} rars-disk-pipelined"
  "rars extract, 2 writers=bin/rar-perf read {a} rars-disk-pool2"
  "rars extract, 4 writers=bin/rar-perf read {a} rars-disk-pool4"
  "rars extract, 8 writers=bin/rar-perf read {a} rars-disk-pool8"
  "UnRAR decode=bin/rar-perf read {a} unrar-test"
  "UnRAR extract=bin/rar-perf read {a} unrar-pipe-disk"
)
if [ -n "${SEVENZIP:-}" ]; then
  ways+=("7-Zip test=bin/7z.exe t {a}" "7-Zip extract=bin/7z.exe x -y -o{out} {a}")
fi
"$PY" scripts/bench-run.py "$UNPACKED" "$RUNNER_TEMP/rar5.rar" "$RUNNER_TEMP/rar4-lz.rar" -- "${ways[@]}"
