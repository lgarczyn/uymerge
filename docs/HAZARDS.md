# Unity YAML hazards

This catalogs the known ways Unity YAML data gets destroyed or corrupted.
It covers standard YAML libraries, the native UnityYAMLMerge, git driver
plumbing, and traps for anyone reimplementing this tool. Each entry says
whether the damage is silent or loud. Silent means no error and wrong data.
Loud means an error or a conflict surfaces.

Sources: this repo's tests and SPEC, the frozen python stopgap and its git
history, the unity-yaml-parser project, and real Trailblazers incident
history. Items marked "real" were observed in production, not just testing.

## 1. Standard YAML libraries, reading

The stopgap's diagnostics reached a verdict that still stands: PyYAML is a
lossy oracle, never validate fidelity with it. Every entry below is also
confirmed by a patch in unity-yaml-parser, a project that adapts PyYAML to
Unity files one workaround at a time.

1.1 Fold trailing-whitespace loss, plain scalars. Silent. The editor folds
a long value at the last space of a space run. The earlier spaces of the
run stay on the folded line as trailing whitespace, and they are content.
Spec folding strips trailing whitespace and joins with one space, so an
interior multi-space run touched by a fold loses spaces. PyYAML reads the
wrong string with no error. Pinned by `scalar-double-space.asset` and
SPEC 2; reproduced live against a real corpus asset.

1.2 Fold whitespace loss, quoted scalars. Silent. Stock PyYAML drops the
whitespace around a fold inside quoted scalars too. unity-yaml-parser
overrides `scan_flow_scalar_spaces` to keep it, noting that standard YAML
loses content there.

1.3 Implicit retyping on load. Silent. YAML 1.1 resolvers retype unquoted
scalars: `y`, `on`, `yes` become booleans, the version string `1.10`
becomes the float `1.1`, `1:20` becomes the integer 80. unity-yaml-parser
strips every implicit resolver because Unity does not follow YAML value
conventions. A scan of 5,584 real assets found zero live instances, so the
class is real but currently empty in this corpus.

1.4 Null semantics. Silent. Spec resolvers turn `~` and the string `null`
into a null value. Unity means null only when the value is absent. A field
whose value is literally the text `null` disappears on load.

1.5 fileID numification. Silent. Anchors are signed 64-bit integers such
as `&-8679921383154817987`. Reading them as numbers risks precision loss
and reformatting. They must stay strings. See `doc_anchor` in model.rs.

1.6 Loud parse failures. Safe but blocking.
- `%TAG` resets at every `---` per spec, but Unity writes it once per
  file, so every multi-document file fails a conforming parser at
  document 2. Measured on 400 real assets: 43 failures, all this cause.
- The `stripped` suffix on a document header is invalid YAML.
- `: Any` entries with an empty key, found in PluginImporter sections of
  `.dll.meta` files, crash stock PyYAML.

## 2. Standard YAML libraries, writing back

All silent. The parse succeeds and the write-back diverges from what the
editor writes. Each is an emitter patch in unity-yaml-parser, and even
with every patch applied that project still documents multi-line quoted
scalars as not byte-exact. This is why byte equality with the editor is
the only safe oracle.

- None becomes `!!null ''` where the editor writes a bare empty value.
- One fold width for everything. The editor folds plain scalars at 79,
  single-quoted at 80, double-quoted at 79.
- Long ASCII double-quoted values fold on width. The editor keeps them on
  one line unless an escape is present.
- Fold breaks are normalized. The editor breaks at the last space of a
  run and keeps the earlier spaces as content.
- Values equal to `---` or `...` get defensive quotes. The editor writes
  them plain. Spurious `...` document-end markers appear.
- Anchor is emitted before the tag. The editor writes `!u!114 &11400000`.
- Keys reorder and flow style flips between `{x: 0}` and block form.
- Quote style is re-derived, flipping single, double, and plain.
- Line endings are normalized, rewriting CRLF and mixed files wholesale.

A note on escapes: decoding `\r\n` or `\u2019` inside double quotes is
correct for a consumer. The damage is on re-encoding, and in the changed
physical line count that breaks line-based tools.

## 3. Native UnityYAMLMerge

Line-based, with no record model, and it exits 0 through all of this.
Real items were found by replaying 3,214 merge triples from history.

- Dropped `m_Id` table entries near long multi-line scalars. Silent, real.
- Duplicated `m_Id` entries. Silent, real, three found in replay.
- Duplicated SerializeReference records with divergent content. Silent,
  real, two found in replay.
- Smart-tag strip. The entry text survives but its metadata rid vanishes,
  so the string silently loses smart formatting. Silent, real.
- Silent side-pick when both sides changed the same value. Silent.
- Revert to base when both sides changed the same entry. Silent.
- Cross-entry value swap and record misalignment. Silent.
- A whole document dropped from a multi-document prefab. Silent.
- A `- id:` dropped from an `m_SharedEntries` list. Silent.
- Its own serializer loses pre-fold whitespace, folds at width 80 instead
  of 79, flattens CRLF, and injects `''` into empty flow values.
- A quoted scalar with a missing close quote loses its last character.
  This tool reproduces that on such input for byte parity, see
  `cut_at_last_quote`.

The red-team suite in verify.rs fabricates each merge corruption above and
asserts the verifier catches it.

## 4. Git driver plumbing

Marker-less ours on driver failure. Silent, real, and the worst incident
on record. When a merge driver exits non-zero without writing output, git
leaves the working-tree file as ours with no conflict markers. It looks
resolved, gets committed, and the other side's work is reverted into main.
The trigger was a driver that needed python3 on machines that lacked it.
One PR reverted 8 localization entries and lost 4 smart tags.

This class is inherent to every git merge driver. It is why SPEC 5.1-5.3
require every failure path to write an unmistakable whole-file conflict.
Output must never look like clean ours.

## 5. Traps for reimplementers

Found while building this tool, by fuzzing and model checking.

- Unterminated-quote span swallow. `gather_quoted` delimits by the closing
  quote, so on malformed input the block swallows later scalars and each
  reserialize pass eats more. Reproduced deliberately for parity;
  idempotence is only claimed on editor-form input. See the pinned test
  in codec.rs.
- A whitespace-only continuation is dropped at a fold. Pinned in
  proptest-regressions.
- Emptied-section mask drop. A side that deletes every record in a section
  once caused masked diff3 to drop the other side's additions. Fixed by
  anchoring placeholders at section headers. The same code once looped
  forever on an empty run and ate 22 GB of memory.
- `m_Items: []` masking an addition. The empty flow form is a present
  list. The set rule must own the whole region or a concurrent addition
  silently loses.
- Astral width miscount. Fold columns count code points. Counting bytes or
  UTF-16 units folds at the wrong column.
- Quoted blocks can contain blank lines and prose that looks like
  `key: value`. Any line-based or regex parser reads scalar content as
  structure and shreds the records that follow.
- Wholesale majority CRLF restore rewrites a mixed file's minority
  endings. Correct as a merge policy, destructive in a formatter. The
  format subcommand preserves terminators per line, SPEC 5.7.
- Duplicate keys are inherited corruption. Content-merging a duplicated id
  mangles it, and conflicting on it conflicts forever. Occurrences are
  carried through unchanged; only byte-identical RefIds duplicates are
  collapsed. SPEC 4.

## Bottom line

Unity YAML parses as YAML 1.1 almost everywhere. But its fold semantics
carry meaning that spec folding silently destroys on read, and no standard
emitter reproduces the editor's output on write. Byte equality with the
editor is the only safe oracle. Every silent-loss mechanism above is
either fixed here, caught loud by the verifier, or deliberately reproduced
and documented as inherited behavior.
