# uymerge specification

This document specifies behavior and stands alone; the Rust crate is its
only implementation. The ground truth for the codec is byte-equality with
what the Unity editor itself writes, enforced by the corpus no-op sweep and
the editor-generated corpora. The old python stopgap is historical: it stays
frozen in production until the flip retires it, and differential.sh against
it remains useful as independent cross-check evidence, never as the spec.

Everything operates on raw text lines. Values are never decoded into a data
model. The oracle for the codec is byte equality with what the Unity editor
itself writes.

## 1. Definitions

- Document: a block starting at a line matching `^--- !u!\d+ &(-?\d+)`.
  The captured integer is its anchor. Text before the first marker (the
  `%YAML`/`%TAG` header) belongs to a synthetic preamble document.
- Table entry: a record under an `  m_TableData:` section, starting at
  `^  - m_Id: (\d+)$`, ending before the next entry or any 2-indent key.
- RefIds record: a record under `  references:` / `    RefIds:`, starting
  at `^    - rid: (-?\d+)$`, spanning following lines indented 6+ spaces.
- Unwrapped form: reserialize with infinite widths and fix_empty off. Both
  merge sides and base are unwrapped before any comparison or merge, so a
  value occupies one physical line unless its content contains newlines.
- CRLF handling: split lines on `\n`; a trailing `\r` is recorded per line
  and re-emitted. Whole-file CRLF restore happens once at output (5.4).

## 2. Codec

The functions below are named after the historical port; the structure may
now change freely as long as the properties and the corpus oracle hold:

2.1 `split_lines(text)` -> per line (content, had_cr).

2.2 Plain scalars. `reemit_plain(value, prefix, cont_indent, width)` folds
at the last space of a run once the column exceeds `width`; a fold never
splits inside a space run; earlier spaces stay as trailing whitespace.
`join_plain_value` is its inverse: each continuation line contributes one
fold space plus its content beyond `cont_indent`.
Columns count Unicode code points, not UTF-16 units and not bytes.
In Rust: count `char`s.

2.3 `gather_continuations(lines, i, key_indent)`: strictly-more-indented,
non-blank lines that do not themselves match the KEY regex.

2.4 Quoted scalars. `gather_quoted` spans to the matching close quote
scanning the joined tail of the file: `''` is an escape inside single
quotes; `\"` and `\\` are escapes inside double quotes; the block may
contain blank lines and `key:`-looking prose. Single-quoted content maps
newlines to blank lines with the column bookkeeping in `reemit_quoted`
(first blank costs cont+2, each consecutive extra costs 1). Double-quoted
content is one continuous escaped flow re-wrapped at width; `\ ` decodes
to a literal space, all other escapes pass through verbatim.
Bug-compatibility: when the closing quote is missing, the last character
is silently dropped, matching what the editor-era stopgap shipped and the
corpus therefore contains. Preserve exactly.

2.5 Mixed terminators: a quoted block whose lines disagree on `\r` passes
through verbatim, since a terminator is never invented. Fold lines inherit
the block's first-line terminator.

2.6 `EMPTY_FLOW`: with fix_empty on, `: ''` followed by `,` or `}` becomes
`: ` (trailing space kept). Applied to the final joined text only.

2.7 `reserialize(text, width=79, quoted_width=80, fix_empty=true)`:
the dispatch loop over lines using the KEY, SEQ, MAPPINGISH regexes and
the EXCLUDE_FIRST set (`|>{[&*!#%`). Copy the regexes character for
character. Values whose first char is in EXCLUDE_FIRST or that open a
flow mapping pass through untouched.

Required properties (must be property tests):
- Idempotence: reserialize(reserialize(x)) == reserialize(x).
- Unwrap losslessness: rewrap(unwrap(rewrap(x))) == rewrap(x).
- Unwrap canonicalization: unwrap(x) == unwrap(rewrap(x)).
- No panic on arbitrary byte input (invalid UTF-8 is an error value,
  handled at the CLI layer, never a crash).

## 3. Model

Parsers work on unwrapped text and return, with source line spans:

- documents: anchor -> span. Anchors listed in file order.
- table entries: id -> { localized: raw text between `    m_Localized:`
  and the next entry field; rids: set of `- rid:` values under
  `    m_Metadata:`; loc_count }. Track duplicate ids separately;
  on duplicates the first occurrence's content is kept and subsequent
  occurrences accumulate into it.
- RefIds records: rid -> { payload: record lines minus `- id:` items
  and minus nested list-header lines matching `\s+m_\w+:( \[\])?\s*$`,
  whose bare or [] form is derived from the id set and must not trip
  the payload rule; ids: set of `- id:` values }. A 2-indent key ends
  the section; only `    RefIds:` under `  references:` opens it.

Section boundary rule everywhere: a line is a section key iff it starts
with exactly two spaces, is not `  -`, and its third character is not
whitespace.

## 4. Merge semantics

All rules operate on unwrapped forms. `b`, `o`, `t` are base, ours,
theirs; `m` is the constructed result.

4.1 Presence (documents, entries, records, any keyed item):
- in o and t: keep; merge content by 4.2/4.3/4.4.
- in one side only, not in b: it was added; keep the adding side's copy.
- in one side only, in b: the other side deleted it. If the keeping
  side's copy equals b: apply the deletion. Else: edit/delete conflict.
- in b only: both deleted; absent from m.

4.2 Scalar rule (entry text, record payload, non-keyed doc content at
record granularity): o == t -> that value; o == b -> t; t == b -> o;
otherwise conflict. A conflict is never resolved by picking a side.

4.3 Set rule (entry metadata rids, record m_SharedEntries ids):
additions and removals from b on both sides all apply:
m = (b intersect o intersect t) union (o minus b) union (t minus b).
The formula is total: with a shared base an add/remove contradiction on
one element cannot exist, since adding needs absence from base and
removing needs presence. P8 review proved the reference's contradiction
branch unreachable; it has been removed everywhere.
Emission order for a merged set: elements that exist in ours keep ours'
order; new elements from theirs append in theirs' order.

4.4 Duplicates: identical duplicate RefIds records are collapsed to one.
Keys duplicated in any input are inherited corruption: presence rules
apply, content rules are skipped for them, and the output carries them
through unchanged rather than conflicting forever.

4.5 Reassembly: within m_TableData and RefIds, records that exist in
ours keep ours' relative order; records only in theirs are inserted
following theirs' neighbor order (after the nearest preceding common
record). Non-record document content merges by diff3 with the record
sections treated as atomic placeholders.

4.6 Everything not keyed (arbitrary MonoBehaviour fields, scene doc
bodies) merges by diff3 within its document. This is where genuine
line-level conflicts surface with markers.

4.7 Guid-reference list salvage: a diff3 conflict region whose every
line, on all three sides, is a same-indent `- {fileID: N, guid: H,
type: N}` item merges as an ordered set instead of conflicting: ours'
items in ours' order, theirs' additions appended in theirs' order,
removals from base honored. Registries and collections are guid-unique
by construction; a within-side duplicate or any non-item line falls
back to markers. This resolves the classic registry append conflict.

## 5. CLI contract

`uymerge BASE REMOTE LOCAL OUTPUT`, where REMOTE is theirs and LOCAL is
ours, matching git merge-driver `%O %B %A %A`.

5.1 Exit 0 only when: merge produced no conflicts AND the built-in
verifier (ported validate_merge) passes on the output against the three
inputs. Exit 0 is a guarantee, not a hope.

5.2 Conflicts: standard markers `<<<<<<< ours` / `=======` /
`>>>>>>> theirs`, exit 1. Marker context is the smallest sensible unit:
a record, or a diff3 hunk.

5.3 Internal errors (undecodable input, verifier failure on our own
output): write a whole-file ours/theirs conflict to OUTPUT, message to
stderr, exit 1. Never exit 0 with unverified content, never leave OUTPUT
equal to plain ours without markers.

5.4 Line endings: if LOCAL's original text has more CRLF than bare-LF
lines, the final output is converted to CRLF wholesale (reference:
`original.count("\r\n") * 2 > original.count("\n")`).

5.5 Test modes: `--batch-reserialize LIST OUTDIR` and `--batch-unwrap
LIST OUTDIR` behave exactly like the reference harness: one output file
per input, named by list index, `.error` suffix on decode failure.

5.6 Drop-in CLI: `uymerge merge [flags] <base> <left> <right> [dest]`
mirrors the native UnityYAMLMerge invocation, so an existing config that
launches UnityYAMLMerge works unchanged when the binary path is swapped.
`left` is theirs, `right` is ours, `dest` is the output and defaults to
`right` in place. The native flags `-l -r -i -o -p -h --force --rules
--fallback --typeInfo --nomappinginoneline --describe` are parsed and
ignored; the value-taking flags `-i -o --rules --fallback --typeInfo` also
consume their argument. uymerge never launches a fallback tool, so
`--fallback` is a no-op and conflicts always surface as markers (5.2). `-l`
and `-r` explicit side resolution is not honored yet: a conflict surfaces
rather than auto-picking a side.

## 6. Non-goals

No YAML data model. No schema knowledge beyond sections 1 and 3. No
network, no config files, no environment variables. Determinism: same
inputs, same bytes out, on every platform.
