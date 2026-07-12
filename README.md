# uymerge

[![CI](https://github.com/lgarczyn/uymerge/actions/workflows/ci.yml/badge.svg)](https://github.com/lgarczyn/uymerge/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/lgarczyn/uymerge?sort=semver)](https://github.com/lgarczyn/uymerge/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macos%20%7C%20windows-informational)

A structural 3-way merge tool for Unity YAML assets. One dependency-free static
binary per platform, registered directly as a git merge driver.

Unity's bundled `UnityYAMLMerge` is line-based: dense edits near long
multi-line scalars can drop, duplicate, or misalign records and still exit 0,
and its output diverges from what the editor writes, so merged scenes and
prefabs churn and can silently lose data. `uymerge` merges the same files
**structurally**, by document anchor, string-table entry, and SerializeReference
record, then reserializes byte-for-byte the way the editor would and self-checks
the result before ever reporting success.

That reserialization is also available on its own, as
[`uymerge format`](#reformat): a formatter that rewrites an asset exactly the
way the editor would, without opening Unity, with a `--check` mode for CI.

## Install

Download the binary for your platform from the
[latest release](https://github.com/lgarczyn/uymerge/releases/latest):

| Platform | Asset |
|----------|-------|
| Linux x86_64 | `uymerge-linux-x86_64` |
| macOS arm64 | `uymerge-macos-arm64` |
| macOS x86_64 | `uymerge-macos-x86_64` |
| Windows x86_64 | `uymerge-windows-x86_64.exe` |

Put it somewhere on disk (e.g. `~/bin/uymerge`) and mark it executable. Or
build from source (see below).

## Setup

Register `uymerge` as a git merge driver. Add `--global` to apply it to every
repository:

```
git config merge.unityyamlmerge.name   "Unity YAML structural merge"
git config merge.unityyamlmerge.driver "'/path/to/uymerge' %O %B %A %A"
```

Route Unity files to the driver with `.gitattributes` (commit this in your
repository so every contributor uses it):

```
*.unity   merge=unityyamlmerge
*.prefab  merge=unityyamlmerge
*.asset   merge=unityyamlmerge
```

That is all. Merges of scenes, prefabs, and assets now come out editor-clean.

### Drop-in for `UnityYAMLMerge`

`uymerge` also accepts the native tool's own invocation:

```
uymerge merge [flags] <base> <theirs> <ours> [output]
```

so an existing `UnityYAMLMerge` configuration works by pointing it at the
`uymerge` binary instead. The native flags (`-h`, `-p`, `--force`, `--rules`,
`--fallback`, and the rest) are accepted and ignored; `uymerge` never hands off
to a fallback merge tool, so conflicts always surface as markers. This is what
lets the setups the Unity manual documents for other version control systems
reuse `uymerge` unchanged.

## Manual use

```
uymerge BASE REMOTE LOCAL OUTPUT
```

Exit `0` is a verified, conflict-free merge; a non-zero exit leaves a
marked-up `OUTPUT` with conflict markers, handled like any merge driver.

## Reformat

The rewrap step is available on its own, with no merge and no second side:

```
uymerge format Assets/Scenes/Main.unity     # rewrite one file in place
uymerge format Assets                       # recurse a directory
uymerge format --check Assets               # report, write nothing
uymerge format - < in.unity > out.unity     # filter stdin to stdout
```

This rewrites a file exactly the way the Unity editor would write it: plain
scalars folded at width 79, quoted at 80, inline flow mappings cleaned up. It
is the same reserialization the merge pipeline ends on, so formatting a file
and merging it are consistent by construction.

It is useful for taking the churn out of a diff before you commit — an asset
touched by an external tool, or hand-edited, comes back to editor form without
opening Unity — and for keeping a repository canonical so future merges start
from clean ground.

A file you name is formatted whatever it contains. A directory is recursed,
and only files that begin with `%YAML` are touched, so pointing `format` at a
project directory will not rewrite your `.cs`, your `.meta`, or a force-binary
asset. Line endings are preserved per line, CRLF and mixed files included, and
a file whose bytes do not change is not rewritten at all — so an already-clean
asset keeps its mtime and Unity does not reimport it.

`--check` writes nothing, lists the files that would change, and exits `1` if
there are any. That makes it a drop-in CI gate or pre-commit hook:

```
uymerge format --check Assets || {
    echo "Unity assets are not in editor form; run: uymerge format Assets"
    exit 1
}
```

Exit `0` all clean, `1` under `--check` when a file would change, `2` on a
usage error or an unreadable path.

## How it works

`uymerge` works on raw text lines and never decodes values into a YAML data
model. Byte equality with the editor's own serialization is the oracle, and
this is deliberately **not** a general YAML library.

1. **Unwrap.** All three inputs are reserialized with infinite width so every
   value occupies one line, defeating the fold-trailing-whitespace loss that
   breaks line-based tools.
2. **Merge structurally.** Documents are keyed by `&anchor`, string-table
   entries by `m_Id`, and SerializeReference records by `rid`. Each is merged
   with true 3-way semantics; `m_SharedEntries` id lists merge as sets honoring
   both sides' additions and removals. Non-keyed regions fall back to a
   hand-rolled diff3 line merge.
3. **Rewrap.** The result is reserialized exactly as the editor writes it
   (plain width 79, quoted width 80, inline flow mappings) and the original
   line endings are restored wholesale from ours.
4. **Self-check.** The rewrapped output is verified against true 3-way
   semantics before success is reported: no dropped or duplicated ids, no
   dangling rid references, and "both sides changed differently" always
   conflicts rather than silently picking one.

If the self-check fails, or a decode fails, or the driver panics, it leaves a
whole-file conflict rather than risk a marker-less keep-ours, because git keeps
the driver's output on failure and a clean-looking lossy file gets committed
as-is. A zero exit is always a verified merge.

## Build from source

Requires a Rust toolchain; the pinned version is in `rust-toolchain.toml`.

```
cargo build --release
# binary at target/release/uymerge
```

## Tests

```
make check          # fmt, clippy, full test suite
cargo test          # unit + golden + diff3 parity tests
sh oracle/git-e2e.sh target/release/uymerge   # end-to-end through real git
```

Golden and diff3-parity fixtures are committed under `tests/fixtures/`; the
diff3 expectations come from git itself. Corpus-scale oracles (`oracle/noop.sh`,
`oracle/bench.sh`) run against a local Unity checkout.

The design and behavior are specified in `docs/SPEC.md`.

## Roadmap

The drop-in CLI above is the foundation for wider support. Planned:

- **Other version control systems.** The Perforce, Plastic / Unity Version
  Control, and SVN setups from the Unity manual should work by swapping the
  binary path into their `UnityYAMLMerge` configs. Each still needs testing and
  an exit-code compatibility pass before it is claimed as supported.
- **Explicit side resolution.** Honor the native `-l` / `-r` flags to take
  theirs or ours on conflict, for pipelines that auto-resolve. Today those flags
  are accepted but a conflict still surfaces as markers.
- **Rules-driven array merge.** The native `--rules` file keys arbitrary
  component arrays by `fileID`. `uymerge` uses a fixed structural model
  (documents, string-table entries, SerializeReference records) plus diff3, and
  conflicts on the rest rather than mis-merging. Broader keyed-array merge from a
  rules file is future work.
- **`strip` command and the SVN calling convention.** The native `strip`
  command and the argv shape SVN passes are not implemented yet.

Out of scope by design: the interactive `--fallback` mechanism that shells out
to a GUI merge tool. `uymerge` verifies its own output and surfaces a conflict
rather than handing off.

## License

MIT. See [LICENSE](LICENSE).
