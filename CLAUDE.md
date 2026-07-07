# uymerge

`uymerge` is a dependency-free Rust binary that merges Unity YAML assets
structurally and reserializes them byte-for-byte the way the Unity editor
writes them. It replaces the native `UnityYAMLMerge` as a git merge driver.

Start every session by reading `docs/SPEC.md`, which specifies the behavior.
The ground truth is byte-equality with the editor's own output. Work one change
at a time, on a branch.

This tool works on raw text lines and is NOT a YAML library. Byte equality
with the editor's serialization is the oracle; do not introduce a value model.

Commands:
- `make check`: fmt, clippy, full test suite. Must pass before commits.
- `cargo test`: unit, golden, and diff3-parity tests.
- `sh oracle/git-e2e.sh target/release/uymerge`: end-to-end through real git.
- `oracle/noop.sh`, `oracle/bench.sh`: corpus-scale sweeps that need a local
  Unity checkout (`CORPUS_REPO`).

Note: `docs/SPEC.md` predates the extraction into this standalone repo and
still refers to the old Python reference (`unityyamlmerge_fix.py`) as an
executable oracle. That reference now lives in the frozen UnityYAMLMerge-Fix
repo. The SPEC, not the reference, is authoritative here.
