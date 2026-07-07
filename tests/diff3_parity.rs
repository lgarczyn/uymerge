//! Parity of the P5 diff3 engine with git merge-file.
//!
//! Fixtures under tests/fixtures/diff3 come from oracle/gen_diff3_cases.sh,
//! with expected outputs from git itself.
//! Each case is replayed in both marker styles and compared byte for byte,
//! including the conflict exit status.

use std::fs;
use std::path::{Path, PathBuf};

use uymerge::diff3::{diff3, render_diff3, render_merge, split_keep, Labels};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diff3")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn expected_rc(path: &Path) -> bool {
    // git merge-file exits with the conflict count.
    // Nonzero means a conflict was emitted, 0 a clean merge.
    read(path).trim() != "0"
}

fn check_case(dir: &Path) {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    let base_text = read(&dir.join("base"));
    let ours_text = read(&dir.join("ours"));
    let theirs_text = read(&dir.join("theirs"));
    let base = split_keep(&base_text);
    let ours = split_keep(&ours_text);
    let theirs = split_keep(&theirs_text);
    let labels = Labels::default();

    let regions = diff3(&base, &ours, &theirs);

    // diff3 style is the authoritative oracle: byte parity on every case.
    let (diff3_text, diff3_conflict) = render_diff3(&regions, &labels);
    assert_eq!(
        diff3_text,
        read(&dir.join("expected.diff3")),
        "diff3 output mismatch for case {name}"
    );
    assert_eq!(
        diff3_conflict,
        expected_rc(&dir.join("expected.diff3.rc")),
        "diff3 conflict flag mismatch for case {name}"
    );

    // Two-way parity holds only where git did not fuse conflicts diff3 keeps
    // apart; the generator drops the marker file when it did.
    let (merge_text, merge_conflict) = render_merge(&regions, &labels);
    if dir.join("expected.merge.parity").exists() {
        assert_eq!(
            merge_text,
            read(&dir.join("expected.merge")),
            "two-way output mismatch for case {name}"
        );
        assert_eq!(
            merge_conflict,
            expected_rc(&dir.join("expected.merge.rc")),
            "two-way conflict flag mismatch for case {name}"
        );
    } else {
        // uymerge keeps the smaller diff3-hunk conflicts, but still flags a
        // conflict exactly when git's two-way merge did.
        assert_eq!(
            merge_conflict,
            expected_rc(&dir.join("expected.merge.rc")),
            "two-way conflict flag mismatch for fused case {name}"
        );
    }
}

#[test]
fn matches_git_merge_file_on_all_cases() {
    let root = fixtures_dir();
    let mut count = 0;
    let mut entries: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", root.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();
    for dir in &entries {
        check_case(dir);
        count += 1;
    }
    assert!(
        count > 0,
        "no diff3 fixtures found under {}",
        root.display()
    );
}
