//! Integration tests over the built uymerge binary, SPEC section 5.
//! Covers clean merge, conflict, self-check failure, both batch modes, CRLF.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use uymerge::codec::reserialize;

const BIN: &str = env!("CARGO_BIN_EXE_uymerge");

fn workdir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

// Runs the driver over three inputs; returns exit code and output bytes.
// REMOTE is theirs, LOCAL is ours, matching git merge-driver order.
fn drive(dir: &Path, base: &[u8], theirs: &[u8], ours: &[u8]) -> (i32, Vec<u8>) {
    let bp = dir.join("base");
    let rp = dir.join("remote");
    let lp = dir.join("local");
    let op = dir.join("out");
    fs::write(&bp, base).unwrap();
    fs::write(&rp, theirs).unwrap();
    fs::write(&lp, ours).unwrap();
    let status = Command::new(BIN)
        .args([&bp, &rp, &lp, &op])
        .status()
        .unwrap();
    let out = fs::read(&op).unwrap_or_default();
    (status.code().unwrap_or(-1), out)
}

#[test]
fn clean_merge_exits_zero_and_takes_the_edit() {
    let dir = workdir("clean");
    let base = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n";
    let theirs = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: B\n";
    // ours equals base, so theirs' edit applies cleanly
    let (rc, out) = drive(&dir, base.as_bytes(), theirs.as_bytes(), base.as_bytes());
    let out = String::from_utf8(out).unwrap();
    assert_eq!(rc, 0);
    assert!(out.contains("m_Name: B"));
    assert!(!out.contains("<<<<<<<"));
}

#[test]
fn conflict_exits_one_with_markers() {
    let dir = workdir("conflict");
    let base = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 1\n";
    let ours = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 2\n";
    let theirs = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 3\n";
    let (rc, out) = drive(&dir, base.as_bytes(), theirs.as_bytes(), ours.as_bytes());
    let out = String::from_utf8(out).unwrap();
    assert_eq!(rc, 1);
    assert!(out.contains("<<<<<<< ours"));
    assert!(out.contains("======="));
    assert!(out.contains(">>>>>>> theirs"));
    assert!(out.contains("m_Value: 2"));
    assert!(out.contains("m_Value: 3"));
}

#[test]
fn self_check_failure_leaves_whole_file_conflict() {
    let dir = workdir("verify");
    // Same m_Id added in a new doc on each side.
    // The keyed merge keeps both, but the file-wide self-check rejects the
    // duplicate, so the driver emits a whole-file conflict, SPEC 5.3.
    let base = "%YAML 1.1\n--- !u!1 &1\nGameObject:\n  m_Name: root\n";
    let body = "  m_TableData:\n  - m_Id: 100\n    m_Localized: x\n  references:\n    version: 2\n";
    let ours = format!("{base}--- !u!114 &2\nMonoBehaviour:\n{body}");
    let theirs = format!("{base}--- !u!114 &3\nMonoBehaviour:\n{body}");
    let (rc, out) = drive(&dir, base.as_bytes(), theirs.as_bytes(), ours.as_bytes());
    let out = String::from_utf8(out).unwrap();
    assert_eq!(rc, 1);
    assert!(out.starts_with("<<<<<<< ours\n"));
    assert!(out.contains("=======\n"));
    assert!(out.ends_with(">>>>>>> theirs\n"));
}

#[test]
fn crlf_input_restores_crlf_output() {
    let dir = workdir("crlf");
    // ours is CRLF, so the whole output is CRLF regardless of the other sides.
    let base = "%YAML 1.1\r\n--- !u!1 &100\r\nGameObject:\r\n  m_Name: A\r\n";
    let theirs = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: B\n";
    let (rc, out) = drive(&dir, base.as_bytes(), theirs.as_bytes(), base.as_bytes());
    let text = String::from_utf8(out).unwrap();
    assert_eq!(rc, 0);
    assert!(text.contains("m_Name: B"));
    assert!(text.contains("\r\n"));
    // no bare LF survived the wholesale restore
    assert!(!text.replace("\r\n", "").contains('\n'));
}

// Two-entry list: one valid UTF-8 input, one invalid.
// Exercises both the reserialize output and the .error path.
fn batch_list(dir: &Path) -> (PathBuf, String) {
    let good = dir.join("good.asset");
    // Long plain scalar: folds under the reserialize width, one line unwrapped.
    let long = "word ".repeat(30);
    let good_text = format!("--- !u!1 &1\nGameObject:\n  m_Name: {long}\n");
    fs::write(&good, &good_text).unwrap();
    let bad = dir.join("bad.asset");
    fs::write(&bad, [0x66, 0x6f, 0xff, 0x6f]).unwrap();
    let list = dir.join("list.txt");
    fs::write(&list, format!("{}\n{}\n", good.display(), bad.display())).unwrap();
    (list, good_text)
}

fn run_batch(mode: &str, list: &Path, outdir: &Path) -> i32 {
    Command::new(BIN)
        .args([mode, list.to_str().unwrap(), outdir.to_str().unwrap()])
        .status()
        .unwrap()
        .code()
        .unwrap_or(-1)
}

#[test]
fn batch_reserialize_matches_reference_widths() {
    let dir = workdir("batch-re");
    let (list, good_text) = batch_list(&dir);
    let outdir = dir.join("out");
    let rc = run_batch("--batch-reserialize", &list, &outdir);
    assert_eq!(rc, 0);
    let got = fs::read_to_string(outdir.join("0")).unwrap();
    assert_eq!(got, reserialize(&good_text, 79, 80, true));
    // the decode failure produced an index-named .error, not a "1"
    assert!(outdir.join("1.error").exists());
    assert!(!outdir.join("1").exists());
}

#[test]
fn batch_unwrap_matches_reference_widths() {
    let dir = workdir("batch-un");
    let (list, good_text) = batch_list(&dir);
    let outdir = dir.join("out");
    let rc = run_batch("--batch-unwrap", &list, &outdir);
    assert_eq!(rc, 0);
    let got = fs::read_to_string(outdir.join("0")).unwrap();
    assert_eq!(
        got,
        reserialize(&good_text, 1_000_000_000, 1_000_000_000, false)
    );
    // unwrap keeps the long value on a single physical line
    assert_eq!(got.lines().count(), good_text.lines().count());
    assert!(outdir.join("1.error").exists());
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/inputs")
        .join(name)
}

// A no-op merge (identical sides) collapses to reserialize.
// Runs over every fixture, so mixed and pure CRLF terminators must survive.
#[test]
fn noop_merge_equals_reserialize_over_all_fixtures() {
    let dir = workdir("noop-all");
    let indir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inputs");
    let mut checked = 0;
    for entry in fs::read_dir(&indir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let raw = fs::read(&path).unwrap();
        let text = String::from_utf8(raw.clone()).unwrap();
        let (rc, out) = drive(&dir, &raw, &raw, &raw);
        assert_eq!(rc, 0, "{}", path.display());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            reserialize(&text, 79, 80, true),
            "no-op merge changed {}",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 14,
        "expected the full fixture set, saw {checked}"
    );
}

// Regression pin: canonical fixtures must round-trip byte for byte,
// terminators included.
// mixed-terminators is majority LF with one CRLF line in a quoted block,
// SPEC 2.5; crlf-table is pure CRLF.
// Pre-normalizing to LF used to strip those CRs.
#[test]
fn noop_merge_preserves_terminators_byte_for_byte() {
    let dir = workdir("noop-term");
    for name in [
        "mixed-terminators.asset",
        "crlf-table.asset",
        "table-with-refs.asset",
        "prefab-multidoc.prefab",
    ] {
        let raw = fs::read(fixture(name)).unwrap();
        let (rc, out) = drive(&dir, &raw, &raw, &raw);
        assert_eq!(rc, 0, "{name}");
        assert_eq!(out, raw, "{name} changed under a no-op merge");
    }
}

// Drive through the native argv: `merge [flags] base left right dest`.
// left is theirs, right is ours, so the mapping matches the plain driver.
fn drive_merge(
    dir: &Path,
    flags: &[&str],
    base: &[u8],
    theirs: &[u8],
    ours: &[u8],
) -> (i32, Vec<u8>) {
    let bp = dir.join("base");
    let rp = dir.join("remote");
    let lp = dir.join("local");
    let op = dir.join("out");
    fs::write(&bp, base).unwrap();
    fs::write(&rp, theirs).unwrap();
    fs::write(&lp, ours).unwrap();
    let mut argv = vec!["merge".to_string()];
    argv.extend(flags.iter().map(|f| f.to_string()));
    for p in [&bp, &rp, &lp, &op] {
        argv.push(p.to_str().unwrap().to_string());
    }
    let status = Command::new(BIN).args(&argv).status().unwrap();
    let out = fs::read(&op).unwrap_or_default();
    (status.code().unwrap_or(-1), out)
}

// The exact flags the Unity manual's git config passes before the four paths.
#[test]
fn merge_subcommand_matches_plain_driver() {
    let dir = workdir("dropin-clean");
    let base = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n";
    let theirs = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: B\n";
    let flags = ["-h", "-p", "--force"];
    let (rc, out) = drive_merge(
        &dir,
        &flags,
        base.as_bytes(),
        theirs.as_bytes(),
        base.as_bytes(),
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(rc, 0);
    assert!(out.contains("m_Name: B"));
    assert!(!out.contains("<<<<<<<"));
}

// --rules and --fallback each consume their file argument.
// A conflict still surfaces as markers because uymerge runs no fallback tool.
#[test]
fn merge_subcommand_swallows_value_flags() {
    let dir = workdir("dropin-flags");
    let base = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 1\n";
    let ours = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 2\n";
    let theirs = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 3\n";
    let flags = ["--rules", "rules.txt", "--fallback", "none"];
    let (rc, out) = drive_merge(
        &dir,
        &flags,
        base.as_bytes(),
        theirs.as_bytes(),
        ours.as_bytes(),
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(rc, 1);
    assert!(out.contains("<<<<<<< ours"));
    assert!(out.contains(">>>>>>> theirs"));
}

// With no dest path the merge is written back over right (ours).
#[test]
fn merge_subcommand_without_dest_writes_over_ours() {
    let dir = workdir("dropin-nodest");
    let base = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n";
    let theirs = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: B\n";
    let bp = dir.join("base");
    let rp = dir.join("remote");
    let lp = dir.join("local");
    fs::write(&bp, base).unwrap();
    fs::write(&rp, theirs).unwrap();
    fs::write(&lp, base).unwrap();
    let status = Command::new(BIN)
        .args([
            "merge",
            bp.to_str().unwrap(),
            rp.to_str().unwrap(),
            lp.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code().unwrap_or(-1), 0);
    let out = fs::read_to_string(&lp).unwrap();
    assert!(out.contains("m_Name: B"));
}

#[test]
fn usage_error_exits_two() {
    let dir = workdir("usage");
    let op = dir.join("out");
    let code = Command::new(BIN)
        .args(["only", "three", op.to_str().unwrap()])
        .status()
        .unwrap()
        .code()
        .unwrap_or(-1);
    assert_eq!(code, 2);
}
