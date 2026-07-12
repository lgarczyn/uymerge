//! CLI driver contract.
//! SPEC section 5.
//! Packet P9.
//!
//! `uymerge BASE REMOTE LOCAL OUTPUT` runs the full pipeline: read, unwrap,
//! merge_file, rewrap, self-check with validate_merge, restore CRLF, write.
//! The `--batch-reserialize` and `--batch-unwrap` modes mirror
//! oracle/py_batch.py so oracle/differential.sh can byte-compare us to it.
//! The `merge` subcommand accepts the native UnityYAMLMerge argv, SPEC 5.6.
//! The `format` subcommand rewraps files into editor form with no merge,
//! SPEC 5.7.
//!
//! Exit 0 only on a conflict-free merge that also passes the self-check,
//! SPEC 5.1-5.3.
//! Any conflict or self-check failure exits 1 with a marked-up OUTPUT.
//! A silent failure once reverted localization work, so OUTPUT must never
//! look like clean ours.
//! Release builds unwind, and the driver catches any panic as the same
//! whole-file conflict.

use std::io::{self, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::process::ExitCode;

use crate::codec::reserialize;
use crate::merge;
use crate::verify;

const PLAIN_WIDTH: usize = 79;
const QUOTED_WIDTH: usize = 80;
// "unwrap" width, large enough never to fold.
// Mirrors the reference INF.
const INF: usize = 1_000_000_000;

const USAGE_RC: u8 = 2;

/// Dispatch on argv.
/// A batch mode when arg 1 names one, else the merge driver.
pub fn run(args: &[String]) -> ExitCode {
    match args.get(1).map(String::as_str) {
        Some("--batch-reserialize") => batch(false, args),
        Some("--batch-unwrap") => batch(true, args),
        Some("merge") => merge_subcommand(args),
        Some("format") => format_subcommand(args),
        _ => driver(args),
    }
}

// Drop-in for the native UnityYAMLMerge CLI, SPEC 5.6.
// The Unity manual wires every VCS as `merge [flags] base left right [dest]`,
// so accepting that argv lets a user swap the binary path into an existing
// config unchanged.
// left is theirs and right is ours, matching our own BASE REMOTE LOCAL order.
// The native flags are accepted and then ignored: uymerge is always headless,
// extension-agnostic, and editor-faithful, and it never runs a fallback tool,
// so --fallback and --rules are swallowed with their file argument.
fn merge_subcommand(args: &[String]) -> ExitCode {
    // These flags take a following file token; every other flag is a boolean.
    const VALUE_FLAGS: [&str; 5] = ["-i", "-o", "--rules", "--fallback", "--typeInfo"];
    let mut positionals: Vec<&str> = Vec::new();
    let mut i = 2;
    while i < args.len() {
        let a = args[i].as_str();
        if VALUE_FLAGS.contains(&a) {
            i += 2;
        } else if a.starts_with('-') && a.len() > 1 {
            i += 1;
        } else {
            positionals.push(a);
            i += 1;
        }
    }
    if positionals.len() < 3 {
        eprintln!("usage: uymerge merge [flags] <base> <theirs> <ours> [output]");
        return ExitCode::from(USAGE_RC);
    }
    // Native order is base, left, right, dest; with no dest the merge is
    // written in place over ours, matching git's %A %A.
    let out = *positionals.get(3).unwrap_or(&positionals[2]);
    let norm = [
        args[0].clone(),
        positionals[0].to_string(),
        positionals[1].to_string(),
        positionals[2].to_string(),
        out.to_string(),
    ];
    driver(&norm)
}

// The editor-faithful rewrap, the canonical form every mode ends on.
// The merge pipeline finishes with it, so a formatted file is byte-identical
// to what a conflict-free merge of that file against itself would write.
fn rewrap(text: &str) -> String {
    reserialize(text, PLAIN_WIDTH, QUOTED_WIDTH, true)
}

// Reformat mode, SPEC 5.7.
// Rewraps a file into editor form in place, with no merge and no second side.
//
// Terminators are left exactly as found: reserialize carries each line's CR
// through, so a CRLF or mixed file keeps its own endings. The driver's
// wholesale CRLF restore is a merge policy, two sides folding into one
// output, and here it would rewrite a mixed file's minority endings.
fn format_subcommand(args: &[String]) -> ExitCode {
    let mut check = false;
    let mut paths: Vec<&str> = Vec::new();
    let mut literal = false;
    for a in &args[2..] {
        let a = a.as_str();
        if literal || a == "-" || !a.starts_with('-') {
            paths.push(a);
        } else if a == "--" {
            literal = true;
        } else if a == "--check" {
            check = true;
        } else {
            eprintln!("uymerge: unknown flag {a}");
            return format_usage();
        }
    }
    if paths.is_empty() {
        return format_usage();
    }

    let mut f = Format {
        check,
        ..Default::default()
    };
    for p in paths {
        if p == "-" {
            f.stdin();
        } else {
            f.walk(Path::new(p), true);
        }
    }

    if f.failed {
        return ExitCode::from(USAGE_RC);
    }
    // --check reports rather than writes, so the changed list is the verdict.
    if check && !f.changed.is_empty() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn format_usage() -> ExitCode {
    eprintln!("usage: uymerge format [--check] <file|dir|->...");
    ExitCode::from(USAGE_RC)
}

// Accumulates a whole run so every bad path is reported, not just the first,
// and the exit code is decided once at the end.
#[derive(Default)]
struct Format {
    check: bool,
    changed: Vec<String>,
    failed: bool,
}

impl Format {
    // A directory recurses; a file is formatted.
    // `named` marks a path the user spelled out. Those are always formatted.
    // Files found by recursing are formatted only if they look like Unity
    // YAML, so pointing at Assets/ cannot rewrite a .cs or a binary asset.
    fn walk(&mut self, path: &Path, named: bool) {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return self.fail(path, &e.to_string()),
        };
        if !meta.is_dir() {
            return self.file(path, named);
        }
        let entries = match std::fs::read_dir(path) {
            Ok(e) => e,
            Err(e) => return self.fail(path, &e.to_string()),
        };
        // Sorted, so a run over a tree reports in a stable order.
        let mut kids: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        kids.sort();
        for k in kids {
            // file_type does not follow symlinks, so a link is neither dir nor
            // file here and is skipped. A cyclic tree cannot hang the walk.
            match std::fs::symlink_metadata(&k) {
                Ok(m) if m.is_dir() => self.walk(&k, false),
                Ok(m) if m.is_file() => self.file(&k, false),
                _ => {}
            }
        }
    }

    fn file(&mut self, path: &Path, named: bool) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return self.fail(path, &e.to_string()),
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            // A named file that cannot be decoded is an error the user asked
            // for. A binary asset found by recursing is simply not ours.
            if named {
                self.fail(path, "not valid UTF-8");
            }
            return;
        };
        if !named && !is_unity_yaml(text) {
            return;
        }
        let out = rewrap(text);
        if out.as_bytes() == bytes {
            return;
        }
        self.changed.push(path.display().to_string());
        if self.check {
            println!("{}", path.display());
            return;
        }
        // Written only when the bytes actually change, so an already-clean
        // asset keeps its mtime and Unity does not reimport it.
        if let Err(e) = std::fs::write(path, out) {
            self.fail(path, &e.to_string());
        }
    }

    // `-` is a filter: stdin to stdout, never in place.
    // --check stays silent on stdout so the exit code is the whole answer.
    fn stdin(&mut self) {
        let mut buf = Vec::new();
        if let Err(e) = io::stdin().read_to_end(&mut buf) {
            return self.fail(Path::new("<stdin>"), &e.to_string());
        }
        let Ok(text) = std::str::from_utf8(&buf) else {
            return self.fail(Path::new("<stdin>"), "not valid UTF-8");
        };
        let out = rewrap(text);
        if out.as_bytes() != buf {
            self.changed.push("<stdin>".to_string());
        }
        if self.check {
            return;
        }
        if let Err(e) = io::stdout().write_all(out.as_bytes()) {
            self.fail(Path::new("<stdin>"), &e.to_string());
        }
    }

    fn fail(&mut self, path: &Path, msg: &str) {
        eprintln!("uymerge: {}: {msg}", path.display());
        self.failed = true;
    }
}

// Unity writes every text asset with the YAML directive on line one.
// A .meta, a .cs or a force-binary asset does not have it, so this is what
// makes recursing over a project directory safe.
fn is_unity_yaml(text: &str) -> bool {
    text.starts_with("%YAML")
}

// Outcome of the merge pipeline.
// Conflict carries the marked-up output; VerifyFailed carries the violations.
enum MergeOutcome {
    Clean(String),
    Conflict(String),
    VerifyFailed(Vec<String>),
}

fn driver(args: &[String]) -> ExitCode {
    if args.len() != 5 {
        eprintln!("usage: uymerge BASE REMOTE LOCAL OUTPUT");
        return ExitCode::from(USAGE_RC);
    }
    // git passes %O %B %A %A: BASE, REMOTE is theirs, LOCAL is ours, OUTPUT.
    let base_p = &args[1];
    let remote_p = &args[2];
    let local_p = &args[3];
    let out_p = &args[4];

    let (Some(base_b), Some(remote_b), Some(local_b)) = (
        read_bytes(base_p),
        read_bytes(remote_p),
        read_bytes(local_p),
    ) else {
        return ExitCode::FAILURE;
    };

    // CRLF is decided from the original ours, SPEC 5.4.
    // Lossy decode is fine: it never alters the ASCII terminators counted.
    let ours_lossy = String::from_utf8_lossy(&local_b);
    let theirs_lossy = String::from_utf8_lossy(&remote_b);
    let crlf = is_crlf(&ours_lossy);

    // Undecodable input is an internal error, SPEC 5.3: leave a whole-file
    // conflict rather than risk a marker-less keep-ours.
    // Feed the raw sides through unchanged; the wholesale CRLF restore below
    // is the only place a terminator is rewritten.
    let (Ok(base), Ok(ours), Ok(theirs)) = (
        std::str::from_utf8(&base_b),
        std::str::from_utf8(&local_b),
        std::str::from_utf8(&remote_b),
    ) else {
        eprintln!("uymerge: input is not valid UTF-8; leaving a whole-file conflict");
        let text = conflict_file(&ours_lossy, &theirs_lossy);
        return finish(out_p, text, crlf, ExitCode::FAILURE);
    };

    // No pre-normalization.
    // The merge and self-check run on the raw unwrapped lines, each still
    // carrying any trailing CR, so a mixed or pure CRLF file keeps its
    // per-line terminators through a no-op merge, SPEC 2.5.
    // The parsers tolerate a trailing CR; validate_merge normalizes internally.
    // CRLF restore stays wholesale and ours-driven, SPEC 5.4.
    //
    // Release profile unwinds, so a latent panic becomes a caught conflict
    // rather than a dead driver leaving OUTPUT as marker-less ours.
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| run_merge(base, ours, theirs)));
    let (text, code) = match outcome {
        Ok(MergeOutcome::Clean(text)) => (text, ExitCode::SUCCESS),
        Ok(MergeOutcome::Conflict(text)) => (text, ExitCode::FAILURE),
        Ok(MergeOutcome::VerifyFailed(viols)) => {
            report_verify(&viols);
            (conflict_file(ours, theirs), ExitCode::FAILURE)
        }
        Err(_) => {
            eprintln!("uymerge: internal error; leaving a whole-file conflict");
            (conflict_file(ours, theirs), ExitCode::FAILURE)
        }
    };
    finish(out_p, text, crlf, code)
}

// The merge pipeline on LF-normalized inputs.
// Unwrap all three, merge by document and record, rewrap, then self-check
// the result against the unwrapped inputs, SPEC 5.1.
// A clean merge that fails the check is a bug, resolved to VerifyFailed,
// never a silent exit 0.
fn run_merge(base: &str, ours: &str, theirs: &str) -> MergeOutcome {
    let bu = reserialize(base, INF, INF, false);
    let ou = reserialize(ours, INF, INF, false);
    let tu = reserialize(theirs, INF, INF, false);
    let merged = merge::merge_file(&bu, &ou, &tu);
    let text = rewrap(&merged.lines.join("\n"));
    if merged.conflict {
        return MergeOutcome::Conflict(text);
    }
    let check = reserialize(&text, INF, INF, false);
    let viols = verify::validate_merge(&bu, &ou, &tu, &check);
    if viols.is_empty() {
        MergeOutcome::Clean(text)
    } else {
        MergeOutcome::VerifyFailed(viols)
    }
}

// Batch test mode.
// One output file per non-blank list entry, named by index, LF-preserving.
// A decode failure writes an empty `<i>.error` instead.
// Mirrors oracle/py_batch.py exactly for the differential oracle.
fn batch(unwrap: bool, args: &[String]) -> ExitCode {
    if args.len() != 4 {
        eprintln!("usage: uymerge {} LIST OUTDIR", args[1]);
        return ExitCode::from(USAGE_RC);
    }
    let listfile = &args[2];
    let outdir = &args[3];
    let list = match std::fs::read_to_string(listfile) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("uymerge: cannot read {listfile}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::create_dir_all(outdir) {
        eprintln!("uymerge: cannot create {outdir}: {e}");
        return ExitCode::FAILURE;
    }
    for (i, p) in list.split('\n').enumerate() {
        if p.is_empty() {
            continue;
        }
        let ok = std::fs::read(p)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .map(|text| {
                if unwrap {
                    reserialize(&text, INF, INF, false)
                } else {
                    rewrap(&text)
                }
            });
        match ok {
            Some(r) => {
                let out = Path::new(outdir).join(i.to_string());
                if let Err(e) = std::fs::write(&out, r) {
                    eprintln!("uymerge: cannot write {}: {e}", out.display());
                    return ExitCode::FAILURE;
                }
            }
            None => {
                let err = Path::new(outdir).join(format!("{i}.error"));
                let _ = std::fs::write(err, "");
            }
        }
    }
    ExitCode::SUCCESS
}

// Read a file, reporting the path so a missing input is diagnosable.
fn read_bytes(path: &str) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("uymerge: cannot read {path}: {e}");
            None
        }
    }
}

// Write OUTPUT, restoring CRLF wholesale first when ours was CRLF, SPEC 5.4.
fn finish(out: &str, text: String, crlf: bool, code: ExitCode) -> ExitCode {
    let text = if crlf { to_crlf(&text) } else { text };
    if let Err(e) = std::fs::write(out, text) {
        eprintln!("uymerge: cannot write {out}: {e}");
        return ExitCode::FAILURE;
    }
    code
}

fn report_verify(viols: &[String]) {
    eprintln!("uymerge: merged output failed the self-check:");
    for v in viols.iter().take(8) {
        eprintln!("  - {v}");
    }
}

// A whole-file ours/theirs conflict, SPEC 5.3.
// Ported from the reference conflict_file.
// git leaves OUTPUT as the working-tree file, so it must be unmistakable:
// a marker-less file reads as resolved.
fn conflict_file(ours: &str, theirs: &str) -> String {
    let mut o = ours.to_string();
    if !o.ends_with('\n') {
        o.push('\n');
    }
    let mut t = theirs.to_string();
    if !t.ends_with('\n') {
        t.push('\n');
    }
    format!("<<<<<<< ours\n{o}=======\n{t}>>>>>>> theirs\n")
}

// SPEC 5.4: ours is CRLF when its CRLF lines outnumber its bare-LF lines.
fn is_crlf(text: &str) -> bool {
    text.matches("\r\n").count() * 2 > text.matches('\n').count()
}

fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_detected_by_majority() {
        assert!(is_crlf("a\r\nb\r\n"));
        assert!(!is_crlf("a\nb\n"));
        // one CRLF among two bare LF is not a majority
        assert!(!is_crlf("a\r\nb\nc\n"));
    }

    #[test]
    fn to_crlf_normalizes_then_restores() {
        assert_eq!(to_crlf("a\r\nb\n"), "a\r\nb\r\n");
    }

    #[test]
    fn conflict_file_wraps_both_sides() {
        let c = conflict_file("ours", "theirs");
        assert_eq!(c, "<<<<<<< ours\nours\n=======\ntheirs\n>>>>>>> theirs\n");
    }

    #[test]
    fn clean_merge_takes_theirs_edit() {
        let base = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n";
        let ours = base;
        let theirs = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: B\n";
        match run_merge(base, ours, theirs) {
            MergeOutcome::Clean(t) => assert!(t.contains("m_Name: B")),
            _ => panic!("expected a clean merge"),
        }
    }

    #[test]
    fn three_way_body_edit_conflicts() {
        let base = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 1\n";
        let ours = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 2\n";
        let theirs = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 3\n";
        match run_merge(base, ours, theirs) {
            MergeOutcome::Conflict(t) => {
                assert!(t.contains("<<<<<<< ours"));
                assert!(t.contains("m_Value: 2"));
                assert!(t.contains("m_Value: 3"));
            }
            _ => panic!("expected a conflict"),
        }
    }

    // A latent bug the self-check must catch: the same m_Id lives in two
    // documents, one added by each side.
    // The per-document keyed merge keeps both, but the file-wide verifier
    // sees a duplicated entry.
    // The driver must route this to a whole-file conflict, never a clean exit.
    #[test]
    fn clean_merge_that_fails_self_check_is_flagged() {
        let base = "%YAML 1.1\n--- !u!1 &1\nGameObject:\n  m_Name: root\n";
        let entry =
            "  m_TableData:\n  - m_Id: 100\n    m_Localized: x\n  references:\n    version: 2\n";
        let ours = format!("{base}--- !u!114 &2\nMonoBehaviour:\n{entry}");
        let theirs = format!("{base}--- !u!114 &3\nMonoBehaviour:\n{entry}");
        match run_merge(base, &ours, &theirs) {
            MergeOutcome::VerifyFailed(v) => assert!(!v.is_empty()),
            MergeOutcome::Clean(_) => panic!("self-check should have failed"),
            MergeOutcome::Conflict(_) => panic!("merge itself was clean, not a conflict"),
        }
    }
}
