//! P12: the merge pipeline must never panic on arbitrary input.
//! The rewrap and self-check of its output must not panic either.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut parts = data.splitn(3, |&b| b == 0xFF);
    let (Some(b), Some(o), Some(t)) = (parts.next(), parts.next(), parts.next()) else {
        return;
    };
    let (Ok(b), Ok(o), Ok(t)) = (
        std::str::from_utf8(b),
        std::str::from_utf8(o),
        std::str::from_utf8(t),
    ) else {
        return;
    };
    let m = uymerge::merge::merge_file(b, o, t);
    let text = m.lines.join("\n");
    let _ = uymerge::codec::reserialize(&text, 79, 80, true);
    let _ = uymerge::verify::validate_merge(b, o, t, &text);
});
