//! Plain 3-way line merge, std only.
//! SPEC section 4.6.
//! Packet P5.
//! LCS diffs composed into 3-way hunks, conflicts labeled ours/base/theirs.
//! Parity with git merge-file.
//!
//! Lines are `&str` slices carrying their terminators, so output is
//! byte-reproducible against git.
//! Matching compares exact bytes.
//! Textual only; knows nothing about YAML.

/// One region of the composed 3-way merge.
#[derive(Debug, PartialEq, Eq)]
pub enum Region<'a> {
    /// Agreed output lines, taken from base or a single side.
    Stable(&'a [&'a str]),
    /// A genuine conflict: all three sides differ over this span.
    Conflict {
        ours: &'a [&'a str],
        base: &'a [&'a str],
        theirs: &'a [&'a str],
    },
}

/// Marker labels for rendered conflicts.
pub struct Labels<'a> {
    pub ours: &'a str,
    pub base: &'a str,
    pub theirs: &'a str,
}

impl Default for Labels<'_> {
    fn default() -> Self {
        Labels {
            ours: "ours",
            base: "base",
            theirs: "theirs",
        }
    }
}

/// Split text into lines, keeping each line's trailing `\n` if present.
/// "a\nb\n" -> ["a\n", "b\n"]; "a\nb" -> ["a\n", "b"]; "" -> [].
pub fn split_keep(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            out.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < bytes.len() {
        out.push(&text[start..]);
    }
    out
}

/// A two-way diff change, as a base line range [start, start+len).
struct Change {
    o_start: usize,
    o_len: usize,
}

const UNMATCHED: usize = usize::MAX;

/// Diff base against one side.
/// Returns change regions in base order, plus `match_side[k]`: the aligned
/// side index when base line k matched, else `UNMATCHED`.
fn diff_side(base: &[&str], side: &[&str]) -> (Vec<Change>, Vec<usize>) {
    let pairs = lcs_pairs(base, side);
    let mut match_side = vec![UNMATCHED; base.len()];
    for &(oi, si) in &pairs {
        match_side[oi] = si;
    }
    let mut changes = Vec::new();
    let mut prev_o = 0usize;
    let mut prev_s = 0usize;
    for &(oi, si) in pairs
        .iter()
        .chain(std::iter::once(&(base.len(), side.len())))
    {
        if oi > prev_o || si > prev_s {
            changes.push(Change {
                o_start: prev_o,
                o_len: oi - prev_o,
            });
        }
        prev_o = oi + 1;
        prev_s = si + 1;
    }
    (changes, match_side)
}

/// LCS matched pairs (base_index, side_index), strictly increasing in both.
/// Prefix and suffix are matched greedily first to shrink the DP and keep
/// edge alignment stable.
/// This is LCS, not git's Myers diff, so repetitive input can pick a
/// different equally-long alignment.
/// Parity with git is asserted only on the realistic fixtures, where lines
/// are near-unique.
fn lcs_pairs(o: &[&str], s: &[&str]) -> Vec<(usize, usize)> {
    let mut lo = 0;
    while lo < o.len() && lo < s.len() && o[lo] == s[lo] {
        lo += 1;
    }
    let mut ho = o.len();
    let mut hs = s.len();
    while ho > lo && hs > lo && o[ho - 1] == s[hs - 1] {
        ho -= 1;
        hs -= 1;
    }
    let mut pairs: Vec<(usize, usize)> = (0..lo).map(|i| (i, i)).collect();
    let a = &o[lo..ho];
    let b = &s[lo..hs];
    let n = a.len();
    let m = b.len();
    if n > 0 && m > 0 {
        // dp[i][j] = LCS length of a[i..], b[j..].
        let mut dp = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[i][j] = if a[i] == b[j] {
                    dp[i + 1][j + 1] + 1
                } else {
                    dp[i + 1][j].max(dp[i][j + 1])
                };
            }
        }
        // Backtrack from the front, taking a match while it stays optimal,
        // else advancing base first so ties resolve consistently.
        let mut i = 0;
        let mut j = 0;
        while i < n && j < m {
            if a[i] == b[j] && dp[i][j] == dp[i + 1][j + 1] + 1 {
                pairs.push((lo + i, lo + j));
                i += 1;
                j += 1;
            } else if dp[i + 1][j] >= dp[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    for k in 0..(o.len() - ho) {
        pairs.push((ho + k, hs + k));
    }
    pairs
}

/// Side line range aligned to base region [r_start, r_end).
/// Boundaries are LCS-matched by construction, so the sentinel branch never
/// hits an unmatched line.
fn side_range(
    match_side: &[usize],
    side_len: usize,
    r_start: usize,
    r_end: usize,
) -> (usize, usize) {
    let o_len = match_side.len();
    let start = if r_start == 0 {
        0
    } else {
        // match_side[r_start - 1] is a matched boundary by construction.
        debug_assert_ne!(match_side[r_start - 1], UNMATCHED);
        match_side[r_start - 1].wrapping_add(1)
    };
    let end = if r_end == o_len {
        side_len
    } else {
        debug_assert_ne!(match_side[r_end], UNMATCHED);
        match_side[r_end]
    };
    (start, end)
}

/// Compose a plain 3-way merge from `base`, `ours`, `theirs` line slices.
/// Regions come in output order; conflicts carry all three sides untrimmed.
pub fn diff3<'a>(
    base: &'a [&'a str],
    ours: &'a [&'a str],
    theirs: &'a [&'a str],
) -> Vec<Region<'a>> {
    let (ca, match_a) = diff_side(base, ours);
    let (cb, match_b) = diff_side(base, theirs);

    // side 0 = ours, side 1 = theirs.
    // Sort by base start, ours before theirs.
    let mut hunks: Vec<(usize, usize, u8)> = Vec::with_capacity(ca.len() + cb.len());
    for c in &ca {
        hunks.push((c.o_start, c.o_len, 0));
    }
    for c in &cb {
        hunks.push((c.o_start, c.o_len, 1));
    }
    hunks.sort_by(|x, y| x.0.cmp(&y.0).then(x.2.cmp(&y.2)));

    let mut out = Vec::new();
    let mut curr = 0usize;
    let mut idx = 0usize;
    while idx < hunks.len() {
        let (o_start, o_len, _) = hunks[idx];
        let r_start = o_start;
        let mut r_end = o_start + o_len;
        idx += 1;
        // Absorb every later hunk that overlaps or touches this region.
        while idx < hunks.len() && hunks[idx].0 <= r_end {
            r_end = r_end.max(hunks[idx].0 + hunks[idx].1);
            idx += 1;
        }
        if r_start > curr {
            out.push(Region::Stable(&base[curr..r_start]));
        }
        let (a0, a1) = side_range(&match_a, ours.len(), r_start, r_end);
        let (b0, b1) = side_range(&match_b, theirs.len(), r_start, r_end);
        let o_slice = &base[r_start..r_end];
        let a_slice = &ours[a0..a1];
        let b_slice = &theirs[b0..b1];
        if a_slice == b_slice {
            out.push(Region::Stable(a_slice));
        } else if a_slice == o_slice {
            out.push(Region::Stable(b_slice));
        } else if b_slice == o_slice {
            out.push(Region::Stable(a_slice));
        } else {
            out.push(Region::Conflict {
                ours: a_slice,
                base: o_slice,
                theirs: b_slice,
            });
        }
        curr = r_end;
    }
    if curr < base.len() {
        out.push(Region::Stable(&base[curr..]));
    }
    out
}

fn common_prefix(a: &[&str], b: &[&str]) -> usize {
    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    p
}

fn common_suffix(a: &[&str], b: &[&str], skip: usize) -> usize {
    let mut q = 0;
    while q < a.len() - skip && q < b.len() - skip && a[a.len() - 1 - q] == b[b.len() - 1 - q] {
        q += 1;
    }
    q
}

/// Render two-way conflict markers, ours/theirs, no base section.
/// Conflicts are zealously trimmed of common leading and trailing lines,
/// matching `git merge-file`.
/// Returns the text and whether any conflict was emitted.
pub fn render_merge(regions: &[Region<'_>], labels: &Labels<'_>) -> (String, bool) {
    render_merge_with(regions, labels, |_, _, _| None)
}

/// render_merge with a salvage hook.
/// Before markers, `salvage` may resolve a conflict to replacement lines,
/// each carrying its terminator.
/// SPEC 4.7 resolves guid-reference list regions as ordered sets through this.
pub fn render_merge_with(
    regions: &[Region<'_>],
    labels: &Labels<'_>,
    salvage: impl Fn(&[&str], &[&str], &[&str]) -> Option<Vec<String>>,
) -> (String, bool) {
    let mut out = String::new();
    let mut conflict = false;
    for region in regions {
        match region {
            Region::Stable(lines) => push_lines(&mut out, lines),
            Region::Conflict { ours, base, theirs } => {
                if let Some(lines) = salvage(ours, base, theirs) {
                    for l in &lines {
                        out.push_str(l);
                    }
                    continue;
                }
                conflict = true;
                let p = common_prefix(ours, theirs);
                let q = common_suffix(ours, theirs, p);
                push_lines(&mut out, &ours[..p]);
                push_marker(&mut out, "<<<<<<<", labels.ours);
                push_lines(&mut out, &ours[p..ours.len() - q]);
                out.push_str("=======\n");
                push_lines(&mut out, &theirs[p..theirs.len() - q]);
                push_marker(&mut out, ">>>>>>>", labels.theirs);
                push_lines(&mut out, &ours[ours.len() - q..]);
            }
        }
    }
    (out, conflict)
}

/// Render three-way (diff3) markers, base section between ours and theirs,
/// untrimmed, matching `git merge-file --diff3`.
pub fn render_diff3(regions: &[Region<'_>], labels: &Labels<'_>) -> (String, bool) {
    let mut out = String::new();
    let mut conflict = false;
    for region in regions {
        match region {
            Region::Stable(lines) => push_lines(&mut out, lines),
            Region::Conflict { ours, base, theirs } => {
                conflict = true;
                push_marker(&mut out, "<<<<<<<", labels.ours);
                push_lines(&mut out, ours);
                push_marker(&mut out, "|||||||", labels.base);
                push_lines(&mut out, base);
                out.push_str("=======\n");
                push_lines(&mut out, theirs);
                push_marker(&mut out, ">>>>>>>", labels.theirs);
            }
        }
    }
    (out, conflict)
}

fn push_lines(out: &mut String, lines: &[&str]) {
    for line in lines {
        out.push_str(line);
    }
}

fn push_marker(out: &mut String, marker: &str, label: &str) {
    out.push_str(marker);
    out.push(' ');
    out.push_str(label);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<&str> {
        split_keep(text)
    }

    #[test]
    fn clean_when_only_ours_changes() {
        let b = lines("1\n2\n3\n");
        let o = lines("1\nO\n3\n");
        let t = lines("1\n2\n3\n");
        let (text, conflict) = render_merge(&diff3(&b, &o, &t), &Labels::default());
        assert!(!conflict);
        assert_eq!(text, "1\nO\n3\n");
    }

    #[test]
    fn clean_when_both_make_same_change() {
        let b = lines("1\n2\n3\n");
        let o = lines("1\nZ\n3\n");
        let t = lines("1\nZ\n3\n");
        let (text, conflict) = render_merge(&diff3(&b, &o, &t), &Labels::default());
        assert!(!conflict);
        assert_eq!(text, "1\nZ\n3\n");
    }

    #[test]
    fn conflict_is_trimmed_in_two_way() {
        let b = lines("1\n2\n3\n");
        let o = lines("1\na\nb\n3\n");
        let t = lines("1\na\nc\n3\n");
        let (text, conflict) = render_merge(&diff3(&b, &o, &t), &Labels::default());
        assert!(conflict);
        assert_eq!(
            text,
            "1\na\n<<<<<<< ours\nb\n=======\nc\n>>>>>>> theirs\n3\n"
        );
    }

    #[test]
    fn conflict_keeps_base_in_diff3() {
        let b = lines("1\n2\n3\n");
        let o = lines("1\na\nb\n3\n");
        let t = lines("1\na\nc\n3\n");
        let (text, conflict) = render_diff3(&diff3(&b, &o, &t), &Labels::default());
        assert!(conflict);
        assert_eq!(
            text,
            "1\n<<<<<<< ours\na\nb\n||||||| base\n2\n=======\na\nc\n>>>>>>> theirs\n3\n"
        );
    }

    #[test]
    fn split_keep_preserves_terminators() {
        assert_eq!(split_keep("a\nb\n"), vec!["a\n", "b\n"]);
        assert_eq!(split_keep("a\nb"), vec!["a\n", "b"]);
        assert!(split_keep("").is_empty());
    }

    #[test]
    fn both_sides_change_a_region_always_conflicts() {
        // A clean value is taken only when a side equals base or both agree.
        // A genuine both-changed region is never side-picked.
        let b = lines("k\n");
        let o = lines("O\n");
        let t = lines("T\n");
        let (_, conflict) = render_merge(&diff3(&b, &o, &t), &Labels::default());
        assert!(conflict);
    }

    #[test]
    fn alignment_divergence_from_git_stays_lossless() {
        // On repeated lines, LCS and git's Myers diff can pick different
        // equally-long alignments.
        // Here uymerge aligns theirs' final "d" to base's "d", so theirs'
        // "f f" and ours' "d b e" insert at different spots and never overlap,
        // giving a clean merge.
        // git aligns theirs' first "d", making both append at one spot, which
        // it reports as a conflict.
        // The divergence is a valid alternative alignment, not data loss:
        // every added line survives and no both-changed region is side-picked.
        // The P8 verifier is the backstop against clean output that breaks the
        // merge rules.
        let b = lines("e\nd\n");
        let o = lines("e\nd\nd\nb\ne\n");
        let t = lines("a\ne\nd\nf\nf\nd\n");
        let (text, conflict) = render_merge(&diff3(&b, &o, &t), &Labels::default());
        assert!(!conflict);
        assert_eq!(text, "a\ne\nd\nf\nf\nd\nd\nb\ne\n");
        // Nothing either side introduced is dropped.
        for added in ["a\n", "f\n", "b\n"] {
            assert!(text.contains(added), "lost line {added:?}");
        }
    }

    use proptest::prelude::*;

    // Small line alphabet so random triples overlap and exercise the LCS.
    fn line_seq() -> impl Strategy<Value = Vec<String>> {
        proptest::collection::vec(
            prop_oneof!["a", "b", "c", "d", "e"].prop_map(|s| format!("{s}\n")),
            0..12,
        )
    }

    fn refs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    proptest! {
        #[test]
        fn noop_yields_base_unchanged(b in line_seq()) {
            let base = refs(&b);
            let (text, conflict) = render_merge(&diff3(&base, &base, &base), &Labels::default());
            prop_assert!(!conflict);
            prop_assert_eq!(text, b.concat());
        }

        #[test]
        fn only_ours_changed_takes_ours(b in line_seq(), o in line_seq()) {
            // theirs == base, so the merge must equal ours verbatim.
            let base = refs(&b);
            let ours = refs(&o);
            let (text, conflict) = render_merge(&diff3(&base, &ours, &base), &Labels::default());
            prop_assert!(!conflict);
            prop_assert_eq!(text, o.concat());
        }

        #[test]
        fn only_theirs_changed_takes_theirs(b in line_seq(), t in line_seq()) {
            let base = refs(&b);
            let theirs = refs(&t);
            let (text, conflict) = render_merge(&diff3(&base, &base, &theirs), &Labels::default());
            prop_assert!(!conflict);
            prop_assert_eq!(text, t.concat());
        }

        #[test]
        fn never_panics_and_agrees_on_conflict_flag(
            b in line_seq(), o in line_seq(), t in line_seq()
        ) {
            let base = refs(&b);
            let ours = refs(&o);
            let theirs = refs(&t);
            let regions = diff3(&base, &ours, &theirs);
            let (_, c_merge) = render_merge(&regions, &Labels::default());
            let (_, c_diff3) = render_diff3(&regions, &Labels::default());
            // Both renderers see the same set of conflict regions.
            prop_assert_eq!(c_merge, c_diff3);
        }
    }
}
