//! Byte-parity of reserialize against the committed golden fixtures.
//! Goldens come from the Python reference via oracle/gen_goldens.py.
//! P3 acceptance: the Rust port matches it byte for byte.

use std::fs;
use std::path::Path;
use uymerge::codec::{reserialize, INF, PLAIN_WIDTH, QUOTED_WIDTH};

fn rewrap(text: &str) -> String {
    reserialize(text, PLAIN_WIDTH, QUOTED_WIDTH, true)
}

fn unwrap(text: &str) -> String {
    reserialize(text, INF, INF, false)
}

#[test]
fn reserialize_matches_goldens() {
    let inputs = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inputs");
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden");
    let mut checked = 0;
    let mut entries: Vec<_> = fs::read_dir(&inputs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no fixtures found");
    for p in entries {
        let name = p.file_name().unwrap().to_str().unwrap();
        let text = fs::read_to_string(&p).unwrap();
        let want_rewrap = fs::read_to_string(golden.join(format!("{name}.rewrap"))).unwrap();
        let want_unwrap = fs::read_to_string(golden.join(format!("{name}.unwrap"))).unwrap();
        assert_eq!(rewrap(&text), want_rewrap, "rewrap mismatch on {name}");
        assert_eq!(unwrap(&text), want_unwrap, "unwrap mismatch on {name}");
        checked += 1;
    }
    eprintln!("golden byte-parity: {checked} fixtures");
}
