#!/bin/sh
# Generate diff3 parity fixtures for packet P5.
#
# For each case, write base/ours/theirs and capture git merge-file output and
# exit status in both marker styles, labels ours/base/theirs.
# The Rust parity test replays these, so expected bytes come from git itself.
#
# diff3 style is the authoritative oracle: uymerge matches it byte for byte,
# including the base section with the ours/base/theirs labels.
# git's two-way style fuses conflicts separated by three or fewer common lines.
# SPEC 5.2 wants markers at the smallest diff3-hunk unit, so uymerge does not
# fuse.
# When git fuses, we drop an expected.merge.parity marker and skip the two-way
# byte comparison for that case; diff3 parity still covers it.
#
# usage: oracle/gen_diff3_cases.sh
# Regenerate whenever the case list changes, then commit the fixtures.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEST="$ROOT/tests/fixtures/diff3"
rm -rf "$DEST"
mkdir -p "$DEST"

# emit NAME BASE OURS THEIRS, each body a literal with \n escapes.
emit() {
  name=$1
  dir="$DEST/$name"
  mkdir -p "$dir"
  printf '%b' "$2" > "$dir/base"
  printf '%b' "$3" > "$dir/ours"
  printf '%b' "$4" > "$dir/theirs"
  for style in merge diff3; do
    [ "$style" = diff3 ] && flag=--diff3 || flag=
    # shellcheck disable=SC2086
    git merge-file -p $flag -L ours -L base -L theirs \
      "$dir/ours" "$dir/base" "$dir/theirs" > "$dir/expected.$style" 2>/dev/null \
      && rc=0 || rc=$?
    printf '%s\n' "$rc" > "$dir/expected.$style.rc"
  done
  # Two-way parity holds unless git fused conflicts that diff3 kept apart.
  merge_conf=$(grep -c '^<<<<<<<' "$dir/expected.merge" || true)
  diff3_conf=$(grep -c '^<<<<<<<' "$dir/expected.diff3" || true)
  if [ "$merge_conf" = "$diff3_conf" ]; then
    : > "$dir/expected.merge.parity"
  fi
}

# Identical inputs: pure no-op, must stay clean and byte-identical.
emit noop \
  '1\n2\n3\n' '1\n2\n3\n' '1\n2\n3\n'

# One side only edits a line: take that side, no conflict.
emit ours-only \
  '1\n2\n3\n' '1\nO\n3\n' '1\n2\n3\n'
emit theirs-only \
  '1\n2\n3\n' '1\n2\n3\n' '1\nT\n3\n'

# Both sides make the identical change: collapse to one, no conflict.
emit same-change \
  '1\n2\n3\n' '1\nZ\n3\n' '1\nZ\n3\n'

# Disjoint edits in different regions: both apply cleanly.
emit disjoint-inserts \
  '1\n2\n3\n' '1\nX\n2\n3\n' '1\n2\n3\nY\n'
emit disjoint-edits \
  'a\nb\nc\nd\ne\n' 'A\nb\nc\nd\ne\n' 'a\nb\nc\nd\nE\n'

# Two separate deletions on the two sides: both apply.
emit two-deletes \
  'a\nb\nc\nd\ne\n' 'a\nc\nd\ne\n' 'a\nb\nc\ne\n'

# Classic conflicting edit with shared neighbor lines to trim.
emit conflict-trim \
  '1\n2\n3\n' '1\na\nb\n3\n' '1\na\nc\n3\n'

# Conflict with a common line in the middle: git does not split it.
emit conflict-middle-common \
  '1\n2\n3\n' '1\nA\nM\nB\n3\n' '1\nC\nM\nD\n3\n'

# Prefix and suffix both trim away, isolating the changed middle.
emit conflict-pre-suf \
  'p\n1\n2\n3\ns\n' 'p\nX\nc1\nY\ns\n' 'p\nX\nc2\nY\ns\n'

# Overlapping insertions of different length at one point.
emit conflict-insert \
  '1\n2\n' '1\nA\n2\n' '1\nA\nB\n2\n'

# One side deletes a span, the other edits inside it: conflict.
emit delete-vs-edit \
  '1\n2\n3\n4\n' '1\n4\n' '1\n2\nX\n4\n'

# Adjacent single-line edits on opposite sides: git fuses to one conflict.
emit adjacent-edits \
  '1\n2\n3\n' '1\nA\n3\n' '1\n2\nB\n'

# Duplicate line run: one side trims it, the other appends; conflict at end.
emit dup-run \
  'x\nx\nx\n' 'x\nx\n' 'x\nx\nx\ny\n'

# Insert into an empty base from one side only.
emit add-to-empty \
  '' 'a\nb\n' ''

# Both sides delete the same tail; clean.
emit both-delete-tail \
  '1\n2\n3\n4\n' '1\n2\n' '1\n2\n'

# Leading insertions on both sides that differ: conflict at the head.
emit conflict-head \
  '1\n2\n' 'A\n1\n2\n' 'B\n1\n2\n'

# Trailing appends on both sides that differ: conflict at the tail.
emit conflict-tail \
  '1\n2\n' '1\n2\nA\n' '1\n2\nB\n'

# Larger multi-region file: one clean edit and one conflict together.
emit multi-region \
  'h1\nh2\nk\nm1\nm2\nk\nt1\nt2\n' \
  'h1\nHO\nk\nm1\nm2\nk\nt1\nt2\n' \
  'h1\nh2\nk\nm1\nm2\nk\nTO\nt2\n'

# Two independent conflicts in one file: both must surface.
emit two-conflicts \
  'a\nb\nc\nd\ne\n' 'a\nB1\nc\nd\nE1\n' 'a\nB2\nc\nd\nE2\n'

# Ambiguous duplicate run where the edit can slide: match git's placement.
emit slide-dup \
  'g\ng\ng\ng\n' 'g\ng\ng\n' 'g\nX\ng\ng\ng\n'

# One side inserts a block, the other edits the following line: adjacency.
emit insert-then-edit \
  '1\n2\n3\n' '1\nNEW\n2\n3\n' '1\n2\nEDIT\n'

# Both delete the same middle line; one also edits nearby: clean delete + edit.
emit shared-delete-plus-edit \
  '1\n2\n3\n4\n' '1\n3\n4X\n' '1\n3\n4\n'

# Repeated block edited differently on each side inside the repetition.
emit repeat-block \
  'p\nq\np\nq\np\nq\n' 'p\nQ1\np\nq\np\nq\n' 'p\nq\np\nq\np\nQ2\n'

# Whole file replaced differently on both sides: single spanning conflict.
emit full-replace \
  'o1\no2\no3\n' 'a1\na2\n' 'b1\nb2\nb3\n'

echo "wrote $(find "$DEST" -mindepth 1 -maxdepth 1 -type d | wc -l) diff3 cases to $DEST"
