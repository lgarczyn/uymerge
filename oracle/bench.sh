#!/bin/sh
# Reproducible timing for a no-op merge of a large corpus file.
# base == ours == theirs drives the full unwrap, rewrap, and parse with no
# conflicts: the P11 budget case.
# Reports the best wall time over several runs; the minimum is the least noisy.
# Hightower only; needs a checkout.
#
# usage: oracle/bench.sh <driver-cmd...>
# example: oracle/bench.sh ./target/release/uymerge
# knobs: BENCH_FILE, BENCH_RUNS, CORPUS_REPO.
set -eu
CORPUS=${CORPUS_REPO:-$HOME/Projects/Trailblazers-4}
F=${BENCH_FILE:-$CORPUS/Assets/Data/Localization Tables/JourneyEventStrings/GraphStrings_en.asset}
RUNS=${BENCH_RUNS:-7}
OUT=$(mktemp)
trap 'rm -f "$OUT"' EXIT

if [ ! -f "$F" ]; then
  echo "bench file not found: $F" >&2
  exit 1
fi

best=
for _ in $(seq 1 "$RUNS"); do
  start=$(date +%s%N)
  "$@" "$F" "$F" "$F" "$OUT"
  end=$(date +%s%N)
  ms=$(( (end - start) / 1000000 ))
  if [ -z "$best" ] || [ "$ms" -lt "$best" ]; then
    best=$ms
  fi
done

if ! cmp -s "$F" "$OUT"; then
  echo "FAIL: no-op output is not byte-identical to input" >&2
  exit 1
fi

size=$(wc -c < "$F")
echo "best of $RUNS runs: ${best} ms  (${size} bytes)  $F"
