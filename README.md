# uymerge

[![CI](https://github.com/lgarczyn/uymerge/actions/workflows/ci.yml/badge.svg)](https://github.com/lgarczyn/uymerge/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/lgarczyn/uymerge?sort=semver)](https://github.com/lgarczyn/uymerge/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macos%20%7C%20windows-informational)

A structural 3-way merge tool for Unity YAML assets. One dependency-free static
binary per platform, registered as a git merge driver.

Unity's bundled `UnityYAMLMerge` is line-based: dense edits near multi-line
scalars can drop, duplicate, or misalign records and still exit 0. `uymerge`
merges by document anchor, string-table entry, and SerializeReference record,
reserializes byte-for-byte the way the editor would, and self-checks before
reporting success.

That reserialization is also available on its own as
[`uymerge format`](#reformat), a formatter that rewrites an asset the way the
editor would without opening Unity, with a `--check` mode for CI.

## Install

Download the binary for your platform from the
[latest release](https://github.com/lgarczyn/uymerge/releases/latest):

| Platform | Asset |
|----------|-------|
| Linux x86_64 | `uymerge-linux-x86_64` |
| macOS arm64 | `uymerge-macos-arm64` |
| macOS x86_64 | `uymerge-macos-x86_64` |
| Windows x86_64 | `uymerge-windows-x86_64.exe` |

Put it on disk (e.g. `~/bin/uymerge`) and mark it executable, or build from
source (see below).

## Setup

Register `uymerge` as a git merge driver (add `--global` to apply everywhere):

```
git config merge.unityyamlmerge.name   "Unity YAML structural merge"
git config merge.unityyamlmerge.driver "'/path/to/uymerge' %O %B %A %A"
```

Route Unity files to it with `.gitattributes` (commit this so every contributor
uses it):

```
*.unity   merge=unityyamlmerge
*.prefab  merge=unityyamlmerge
*.asset   merge=unityyamlmerge
```

### Drop-in for `UnityYAMLMerge`

`uymerge` also accepts the native tool's invocation, so an existing
`UnityYAMLMerge` config works by pointing it at the `uymerge` binary:

```
uymerge merge [flags] <base> <theirs> <ours> [output]
```

The native flags (`-h`, `-p`, `--force`, `--rules`, `--fallback`, ...) are
accepted and ignored. `uymerge` never hands off to a fallback tool, so
conflicts always surface as markers.

## Manual use

```
uymerge BASE REMOTE LOCAL OUTPUT
```

Exit `0` is a verified, conflict-free merge; non-zero leaves `OUTPUT` with
conflict markers.

## Reformat

The rewrap step is available on its own, with no merge:

```
uymerge format Assets/Scenes/Main.unity     # rewrite one file in place
uymerge format Assets                       # recurse a directory
uymerge format --check Assets               # report, write nothing
uymerge format - < in.unity > out.unity     # filter stdin to stdout
```

This rewrites a file the way the editor would (plain scalars folded at width
79, quoted at 80, inline flow mappings cleaned up) — the same reserialization
the merge pipeline ends on. Useful for taking churn out of a diff, or keeping a
repository canonical so future merges start clean.

A named file is formatted whatever it contains. A directory is recursed and
only files beginning with `%YAML` are touched, so `.cs`, `.meta`, and
force-binary assets are left alone. Line endings are preserved per line, and a
file whose bytes do not change is not rewritten (no Unity reimport).

`--check` writes nothing, lists files that would change, and exits `1` if any
do — a drop-in CI gate or pre-commit hook. Exit `2` on a usage error or
unreadable path.

## How it works

`uymerge` works on raw text lines and never decodes values into a YAML data
model. Byte equality with the editor's serialization is the oracle; this is
deliberately **not** a general YAML library.

1. **Unwrap.** All three inputs are reserialized at infinite width so every
   value is one line, defeating fold-trailing-whitespace loss.
2. **Merge structurally.** Documents keyed by `&anchor`, string-table entries
   by `m_Id`, SerializeReference records by `rid`, each merged with true 3-way
   semantics; `m_SharedEntries` id lists merge as sets. Non-keyed regions fall
   back to a diff3 line merge.
3. **Rewrap.** The result is reserialized exactly as the editor writes it and
   the original line endings are restored from ours.
4. **Self-check.** The output is verified against 3-way semantics before
   success: no dropped or duplicated ids, no dangling rids, "both sides changed
   differently" always conflicts.

If the self-check fails, a decode fails, or the driver panics, it leaves a
whole-file conflict rather than risk a marker-less keep-ours. A zero exit is
always a verified merge.

## Build from source

Requires a Rust toolchain (pinned in `rust-toolchain.toml`):

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

Fixtures live under `tests/fixtures/`; diff3 expectations come from git itself.
The design and behavior are specified in `docs/SPEC.md`.

## Roadmap

- **Other version control systems.** The Perforce, Plastic, and SVN setups from
  the Unity manual should work by swapping the binary path into their
  `UnityYAMLMerge` configs, pending an exit-code compatibility pass.
- **Explicit side resolution.** Honor the native `-l` / `-r` flags to take
  theirs or ours on conflict. Today they are accepted but conflicts still
  surface as markers.
- **Rules-driven array merge.** Support the native `--rules` file that keys
  arbitrary component arrays by `fileID`.
- **`strip` command and the SVN calling convention.**
- **`unity-to-yaml` and `yaml-to-unity`.** Convert a Unity asset to standard
  YAML that any conforming parser reads, and back, so tools like `yq` can edit
  assets with editor-clean output (see `docs/HAZARDS.md`).

Out of scope: the interactive `--fallback` mechanism that shells out to a GUI
merge tool.

## License

MIT. See [LICENSE](LICENSE).
