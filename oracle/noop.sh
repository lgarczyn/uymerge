#!/bin/sh
# No-op merge sweep: base == ours == theirs over a corpus sample.
# Every file must be rc 0 and byte-identical.
# Catches verifier false positives and codec churn at scale.
# Hightower only.
#
# usage: oracle/noop.sh <driver-cmd...>
set -eu
CORPUS=${CORPUS_REPO:-$HOME/Projects/Trailblazers-4}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
{
  ls "$CORPUS"/Assets/Data/Localization\ Tables/*/*.asset
  find "$CORPUS/Assets" -name '*.unity' | head -30
  find "$CORPUS/Assets" -name '*.prefab' | sort | awk 'NR % 20 == 0'
  find "$CORPUS/Assets" -name '*.asset' | sort | awk 'NR % 40 == 0'
} > "$WORK/list.txt"
total=0; bad=0
while IFS= read -r f; do
  head -c5 "$f" | grep -q '^%YAML' || continue
  total=$((total + 1))
  cp "$f" "$WORK/out"
  if "$@" "$f" "$f" "$f" "$WORK/out" 2>"$WORK/err" && cmp -s "$f" "$WORK/out" \
     && [ ! -s "$WORK/err" ]; then :; else
    bad=$((bad + 1))
    echo "FAIL: $f"
  fi
done < "$WORK/list.txt"
echo "noop sweep: $total files, $bad failures"
[ "$bad" -eq 0 ]
