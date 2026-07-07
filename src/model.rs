//! Parsers for documents, table entries, and RefIds records, with line spans
//! for reassembly.
//! SPEC section 3.
//! Packet P4.
//! Reference functions: table_entries, refid_records, DOC_ANCHOR.
//!
//! These mirror the Python reference one to one.
//! Content extraction matches table_entries and refid_records exactly; the
//! added value is line spans and explicit duplicate-key tracking.
//! Parsers work on unwrapped text split on '\n'; a trailing '\r' stays in the
//! line content, as in the reference.
//! CRLF normalization is the caller's job.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

/// A half-open line range into `text.split('\n')`.
/// `start` inclusive, `end` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// One `m_TableData` entry, keyed by `m_Id`.
/// `localized` is the joined `m_Localized` text, `rids` the `m_Metadata` rid
/// set, `loc_count` the count of `m_Localized` fields.
/// `spans` holds one range per physical occurrence.
/// Duplicates accumulate content but keep every occurrence span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableEntry {
    pub localized: String,
    pub rids: BTreeSet<String>,
    pub loc_count: usize,
    pub spans: Vec<Span>,
}

/// Result of [`table_entries`]: entries by id, ids in first-seen order, and
/// the ids that occur more than once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableData {
    pub entries: BTreeMap<String, TableEntry>,
    pub order: Vec<String>,
    pub dups: BTreeSet<String>,
}

/// One `references`/`RefIds` record, keyed by rid.
/// `payload` is the joined record lines minus `- id:` items, `ids` their
/// value set.
/// `spans` holds one range per physical occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefRecord {
    pub payload: String,
    pub ids: BTreeSet<String>,
    pub spans: Vec<Span>,
}

/// Result of [`refid_records`]: records by rid, rids in first-seen order, and
/// the rids that occur more than once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefData {
    pub records: BTreeMap<String, RefRecord>,
    pub order: Vec<String>,
    pub dups: BTreeSet<String>,
}

/// Result of [`documents`]: anchors in file order, each anchor's first-seen
/// span, repeated anchors, and the preamble span before the first marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Documents {
    pub order: Vec<String>,
    pub docs: BTreeMap<String, Span>,
    pub dups: BTreeSet<String>,
    pub preamble: Option<Span>,
}

// --- private matchers, ported char for char from the reference regexes ---

// One or more ASCII digits from the front of `s`.
// Reference `\d+`.
fn take_uint(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        None
    } else {
        Some((&s[..i], &s[i..]))
    }
}

// Optional '-' then one or more ASCII digits.
// Reference `-?\d+`.
fn take_signed_int(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        None
    } else {
        Some((&s[..i], &s[i..]))
    }
}

fn all_ws(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

// Strip one trailing CR, leaving the content of a CRLF line.
//
// The verifier's regexes run on text already normalized to LF, so they never
// meet a trailing CR.
// The merge instead runs these matchers on the raw codec-unwrap lines, which
// keep any CR so a mixed-terminator file survives a no-op byte identical,
// SPEC 2.5.
// The end-anchored matchers below accept exactly one trailing CR and exclude
// it from the value, so an id or rid compares equal across line-ending styles
// while the merge reuses the original bytes.
pub(crate) fn strip_cr(s: &str) -> &str {
    s.strip_suffix('\r').unwrap_or(s)
}

// `^  - m_Id: (\d+)\s*$`
fn entry_id(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("  - m_Id: ")?;
    let (num, tail) = take_uint(rest)?;
    all_ws(tail).then_some(num)
}

// `^    - rid: (-?\d+)\s*$`
fn ref_rid(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("    - rid: ")?;
    let (num, tail) = take_signed_int(rest)?;
    all_ws(tail).then_some(num)
}

// `^\s+- rid: (-?\d+)$`, plus one tolerated trailing CR.
// See strip_cr.
fn item_rid(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(char::is_whitespace);
    if trimmed.len() == line.len() {
        return None; // \s+ needs at least one whitespace char
    }
    let rest = trimmed.strip_prefix("- rid: ")?;
    let (num, tail) = take_signed_int(rest)?;
    (tail.is_empty() || tail == "\r").then_some(num)
}

// `^\s+m_\w+:( \[\])?\s*$`: a nested list header in bare or empty [] form.
fn is_list_header(line: &str) -> bool {
    let line = strip_cr(line);
    let trimmed = line.trim_start_matches(char::is_whitespace);
    if trimmed.len() == line.len() {
        return false;
    }
    let Some(rest) = trimmed.strip_prefix("m_") else {
        return false;
    };
    let word = rest.len()
        - rest
            .trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
            .len();
    if word == 0 {
        return false;
    }
    let rest = rest[word..].trim_end_matches(char::is_whitespace);
    rest == ":" || rest == ": []"
}

// `^\s+- id: (-?\d+)$`, plus one tolerated trailing CR.
// See strip_cr.
fn shared_id(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(char::is_whitespace);
    if trimmed.len() == line.len() {
        return None;
    }
    let rest = trimmed.strip_prefix("- id: ")?;
    let (num, tail) = take_signed_int(rest)?;
    (tail.is_empty() || tail == "\r").then_some(num)
}

// `^--- !u!\d+ &(-?\d+)`, not anchored at end.
fn doc_anchor(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("--- !u!")?;
    let (_typ, rest) = take_uint(rest)?;
    let rest = rest.strip_prefix(" &")?;
    let (anchor, _tail) = take_signed_int(rest)?;
    Some(anchor)
}

// Section boundary rule: two leading spaces, not `  -`, and a third
// non-whitespace char.
// Reference:
// `line.startswith("  ") and not line.startswith("  -") and line[2:3].strip()`
/// The header opening a table section.
/// m_TableData holds locale tables, m_Entries the shared key table; both
/// carry the same m_Id-keyed record shape.
/// Single owner of the section name list: the parser and the merge run
/// finder must agree on it, or a side's placeholder and its parsed entries
/// drift apart.
pub(crate) fn is_table_header(line: &str) -> bool {
    is_section_key(line) && matches!(strip_cr(line).trim(), "m_TableData:" | "m_Entries:")
}

/// The header opening the RefIds run under `references:`.
pub(crate) fn is_refids_header(line: &str) -> bool {
    strip_cr(line) == "    RefIds:"
}

pub(crate) fn is_section_key(line: &str) -> bool {
    if !line.starts_with("  ") || line.starts_with("  -") {
        return false;
    }
    matches!(line.chars().nth(2), Some(c) if !c.is_whitespace())
}

// --- parsers -------------------------------------------------------------

/// Split whole documents by `&anchor` per [`Documents`].
/// Ported from the reference `DOC_ANCHOR` regex, plus spans and duplicate
/// tracking.
pub fn documents(text: &str) -> Documents {
    let lines: Vec<&str> = text.split('\n').collect();
    let marks: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| doc_anchor(line).map(|a| (i, a)))
        .collect();
    let mut order = Vec::new();
    let mut docs: BTreeMap<String, Span> = BTreeMap::new();
    let mut dups = BTreeSet::new();
    for (idx, &(start, anchor)) in marks.iter().enumerate() {
        let end = marks.get(idx + 1).map_or(lines.len(), |&(next, _)| next);
        order.push(anchor.to_string());
        match docs.entry(anchor.to_string()) {
            Entry::Occupied(_) => {
                dups.insert(anchor.to_string());
            }
            Entry::Vacant(e) => {
                e.insert(Span { start, end });
            }
        }
    }
    let preamble = match marks.first() {
        Some(&(first, _)) if first > 0 => Some(Span {
            start: 0,
            end: first,
        }),
        Some(_) => None,
        None => Some(Span {
            start: 0,
            end: lines.len(),
        }),
    };
    Documents {
        order,
        docs,
        dups,
        preamble,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    None,
    Loc,
    Meta,
}

#[derive(Default)]
struct EntBuilder {
    loc: Vec<String>,
    rids: BTreeSet<String>,
    n: usize,
    spans: Vec<Span>,
}

/// Parse `m_TableData` entries per [`TableData`].
/// Mirrors the reference `table_entries`: duplicate ids accumulate into the
/// first record, and content rules are the caller's concern.
pub fn table_entries(text: &str) -> TableData {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut builders: BTreeMap<String, EntBuilder> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut dups: BTreeSet<String> = BTreeSet::new();
    let mut in_table = false;
    let mut cur: Option<String> = None;
    let mut field = Field::None;
    let mut open: Option<(String, usize)> = None;

    for (i, &line) in lines.iter().enumerate() {
        if is_section_key(line) {
            close_span(&mut builders, &mut open, i);
            in_table = is_table_header(line);
            cur = None;
            field = Field::None;
            continue;
        }
        if !in_table {
            continue;
        }
        if let Some(id) = entry_id(line) {
            close_span(&mut builders, &mut open, i);
            let id = id.to_string();
            match builders.entry(id.clone()) {
                Entry::Occupied(_) => {
                    dups.insert(id.clone());
                }
                Entry::Vacant(e) => {
                    e.insert(EntBuilder::default());
                    order.push(id.clone());
                }
            }
            open = Some((id.clone(), i));
            cur = Some(id);
            field = Field::None;
            continue;
        }
        let Some(id) = cur.clone() else { continue };
        let Some(b) = builders.get_mut(&id) else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("    m_Localized:") {
            b.loc.push(rest.to_string());
            b.n += 1;
            field = Field::Loc;
        } else if line.starts_with("    m_") {
            field = if line.starts_with("    m_Metadata:") {
                Field::Meta
            } else {
                Field::None
            };
        } else if field == Field::Meta {
            if let Some(rid) = item_rid(line) {
                b.rids.insert(rid.to_string());
            }
        } else if field == Field::Loc {
            b.loc.push(line.to_string());
        }
    }
    close_span(&mut builders, &mut open, lines.len());

    let entries = builders
        .into_iter()
        .map(|(k, b)| {
            (
                k,
                TableEntry {
                    localized: b.loc.join("\n"),
                    rids: b.rids,
                    loc_count: b.n,
                    spans: b.spans,
                },
            )
        })
        .collect();
    TableData {
        entries,
        order,
        dups,
    }
}

// Close the open occurrence at `end`, appending its span to that builder.
fn close_span<B: HasSpans>(
    builders: &mut BTreeMap<String, B>,
    open: &mut Option<(String, usize)>,
    end: usize,
) {
    if let Some((id, start)) = open.take() {
        if let Some(b) = builders.get_mut(&id) {
            b.spans_mut().push(Span { start, end });
        }
    }
}

// Lets close_span push spans into either builder type.
trait HasSpans {
    fn spans_mut(&mut self) -> &mut Vec<Span>;
}
impl HasSpans for EntBuilder {
    fn spans_mut(&mut self) -> &mut Vec<Span> {
        &mut self.spans
    }
}
impl HasSpans for RefBuilder {
    fn spans_mut(&mut self) -> &mut Vec<Span> {
        &mut self.spans
    }
}

#[derive(Default)]
struct RefBuilder {
    raw: Vec<String>,
    ids: BTreeSet<String>,
    spans: Vec<Span>,
}

/// Parse `references`/`RefIds` records per [`RefData`].
/// Mirrors the reference `refid_records`: `- id:` items feed the id set,
/// everything else the payload, and duplicate rids accumulate into the first
/// record.
pub fn refid_records(text: &str) -> RefData {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut builders: BTreeMap<String, RefBuilder> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut dups: BTreeSet<String> = BTreeSet::new();
    let mut in_refs = false;
    let mut in_list = false;
    let mut cur: Option<String> = None;
    let mut open: Option<(String, usize)> = None;

    for (i, &line) in lines.iter().enumerate() {
        if is_section_key(line) {
            close_span(&mut builders, &mut open, i);
            in_refs = line.trim() == "references:";
            in_list = false;
            cur = None;
            continue;
        }
        if !in_refs {
            continue;
        }
        if is_refids_header(line) {
            in_list = true;
            continue;
        }
        if !in_list {
            continue;
        }
        if let Some(rid) = ref_rid(line) {
            close_span(&mut builders, &mut open, i);
            let rid = rid.to_string();
            match builders.entry(rid.clone()) {
                Entry::Occupied(_) => {
                    dups.insert(rid.clone());
                }
                Entry::Vacant(e) => {
                    e.insert(RefBuilder::default());
                    order.push(rid.clone());
                }
            }
            open = Some((rid.clone(), i));
            cur = Some(rid);
            continue;
        }
        if let Some(id) = cur.clone() {
            if line.starts_with("      ") || strip_cr(line).is_empty() {
                if let Some(b) = builders.get_mut(&id) {
                    if let Some(sid) = shared_id(line) {
                        b.ids.insert(sid.to_string());
                    } else if is_list_header(line) {
                        // A list header's bare vs [] form is derived from the
                        // id set, not payload, so it must not trip the payload
                        // rule.
                    } else {
                        b.raw.push(line.to_string());
                    }
                }
            } else {
                // Content scanning stops here, matching the reference, but the
                // emission span stays open to the next record or section key.
                // A quoted payload can close at column zero; that line is this
                // record's bytes, and a span gap would let the mask swallow it
                // and un-terminate the scalar on re-emission.
                cur = None;
            }
        }
    }
    close_span(&mut builders, &mut open, lines.len());

    let records = builders
        .into_iter()
        .map(|(k, b)| {
            (
                k,
                RefRecord {
                    payload: b.raw.join("\n"),
                    ids: b.ids,
                    spans: b.spans,
                },
            )
        })
        .collect();
    RefData {
        records,
        order,
        dups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = include_str!("../tests/fixtures/inputs/table-with-refs.asset");
    const PREFAB: &str = include_str!("../tests/fixtures/inputs/prefab-multidoc.prefab");
    const CRLF: &str = include_str!("../tests/fixtures/inputs/crlf-table.asset");

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn documents_multidoc_fixture() {
        let d = documents(PREFAB);
        assert_eq!(d.order, vec!["100", "200", "-300", "400"]);
        assert!(d.dups.is_empty());
        assert_eq!(d.preamble, Some(Span { start: 0, end: 2 }));
        assert_eq!(d.docs["100"], Span { start: 2, end: 5 });
        assert_eq!(d.docs["200"], Span { start: 5, end: 9 });
        assert_eq!(d.docs["-300"], Span { start: 9, end: 12 });
        // last document runs to EOF, past the trailing blank line
        assert_eq!(d.docs["400"], Span { start: 12, end: 16 });
    }

    #[test]
    fn documents_single_doc_has_no_preamble_split_only_when_marker_first() {
        let d = documents(TABLE);
        assert_eq!(d.order, vec!["11400000"]);
        // header lines precede the first marker
        assert_eq!(d.preamble, Some(Span { start: 0, end: 2 }));
        assert_eq!(d.docs["11400000"].start, 2);
    }

    #[test]
    fn documents_tracks_duplicate_anchors() {
        let t =
            "--- !u!1 &100\nGameObject:\n  m_Name: A\n--- !u!1 &100\nGameObject:\n  m_Name: B\n";
        let d = documents(t);
        assert_eq!(d.order, vec!["100", "100"]);
        assert_eq!(d.dups, set(&["100"]));
        // first occurrence span is kept
        assert_eq!(d.docs["100"], Span { start: 0, end: 3 });
    }

    #[test]
    fn documents_no_marker_is_all_preamble() {
        let d = documents("%YAML 1.1\n%TAG !u!\n");
        assert!(d.order.is_empty());
        assert_eq!(d.preamble, Some(Span { start: 0, end: 3 }));
    }

    #[test]
    fn table_entries_fixture() {
        let td = table_entries(TABLE);
        assert_eq!(td.order, vec!["100", "200", "300"]);
        assert!(td.dups.is_empty());

        let e100 = &td.entries["100"];
        assert_eq!(e100.localized, " plain string");
        assert!(e100.rids.is_empty());
        assert_eq!(e100.loc_count, 1);
        assert_eq!(e100.spans, vec![Span { start: 6, end: 10 }]);

        let e200 = &td.entries["200"];
        assert_eq!(e200.localized, " '{smart} string'");
        assert_eq!(e200.rids, set(&["842043826615615503"]));
        assert_eq!(e200.loc_count, 1);
        assert_eq!(e200.spans, vec![Span { start: 10, end: 15 }]);

        let e300 = &td.entries["300"];
        assert_eq!(e300.localized, " third entry");
        assert_eq!(e300.spans, vec![Span { start: 15, end: 19 }]);
    }

    #[test]
    fn table_entries_duplicate_id_accumulates() {
        let t = "--- !u!114 &1\nMonoBehaviour:\n  m_TableData:\n  - m_Id: 100\n    m_Localized: first\n    m_Metadata:\n      m_Items:\n      - rid: 5\n  - m_Id: 100\n    m_Localized: second\n    m_Metadata:\n      m_Items:\n      - rid: 7\n  m_OtherKey: x\n";
        let td = table_entries(t);
        assert_eq!(td.dups, set(&["100"]));
        let e = &td.entries["100"];
        // first occurrence content kept, later occurrence accumulated
        assert_eq!(e.localized, " first\n second");
        assert_eq!(e.rids, set(&["5", "7"]));
        assert_eq!(e.loc_count, 2);
        assert_eq!(
            e.spans,
            vec![Span { start: 3, end: 8 }, Span { start: 8, end: 13 }]
        );
    }

    #[test]
    fn table_entries_unwrapped_continuation_joins() {
        let t = "--- !u!114 &1\nMonoBehaviour:\n  m_TableData:\n  - m_Id: 100\n    m_Localized: line one\n      cont two\n    m_Metadata:\n      m_Items: []\n";
        let td = table_entries(t);
        let e = &td.entries["100"];
        assert_eq!(e.localized, " line one\n      cont two");
        assert_eq!(e.loc_count, 1);
    }

    #[test]
    fn table_entries_crlf_content_preserved() {
        let td = table_entries(CRLF);
        assert_eq!(td.order, vec!["100", "200"]);
        // raw parser keeps the trailing CR in content
        assert_eq!(td.entries["100"].localized, " a\r");
        assert_eq!(td.entries["200"].localized, " b\r");
    }

    #[test]
    fn refid_records_crlf_lines_parse_keys_and_ids() {
        // A CRLF record: header, rid, id items and blank line all carry a
        // trailing CR, yet keys and ids must parse without it while the raw
        // lines stay intact for the span.
        let t = "--- !u!114 &1\r\nMonoBehaviour:\r\n  references:\r\n    RefIds:\r\n    - rid: 10\r\n      a: one\r\n\r\n      - id: 1\r\n      - id: 2\r\n  m_End: 1\r\n";
        let rd = refid_records(t);
        assert_eq!(rd.order, vec!["10"]);
        assert!(rd.dups.is_empty());
        let r = &rd.records["10"];
        assert_eq!(r.ids, set(&["1", "2"]));
        // the blank CRLF line is content, kept in the payload with its CR
        assert_eq!(r.payload, "      a: one\r\n\r");
        assert_eq!(r.spans, vec![Span { start: 4, end: 9 }]);
    }

    #[test]
    fn table_entries_crlf_metadata_rids_parse() {
        let t = "--- !u!114 &1\r\nMonoBehaviour:\r\n  m_TableData:\r\n  - m_Id: 100\r\n    m_Localized: a\r\n    m_Metadata:\r\n      m_Items:\r\n      - rid: 5\r\n  references:\r\n    version: 2\r\n";
        let td = table_entries(t);
        assert_eq!(td.order, vec!["100"]);
        assert_eq!(td.entries["100"].rids, set(&["5"]));
    }

    #[test]
    fn refid_records_fixture() {
        let rd = refid_records(TABLE);
        assert_eq!(rd.order, vec!["842043826615615503"]);
        assert!(rd.dups.is_empty());
        let r = &rd.records["842043826615615503"];
        // list headers m_Entries and m_SharedEntries are excluded: their
        // bare or [] form is derived from the id set, not payload
        assert_eq!(
            r.payload,
            "      type: {class: SmartFormatTag, ns: UnityEngine.Localization.Metadata, asm: Unity.Localization}\n      data:\n"
        );
        assert_eq!(r.ids, set(&["200", "300"]));
        // record runs from its rid line to EOF, over the trailing blank line
        assert_eq!(r.spans, vec![Span { start: 22, end: 30 }]);
    }

    #[test]
    fn refid_records_duplicate_rid_accumulates() {
        let t = "--- !u!114 &1\nMonoBehaviour:\n  references:\n    RefIds:\n    - rid: 10\n      a: one\n      - id: 1\n    - rid: 10\n      b: two\n      - id: 2\n    - rid: 20\n      c: three\n  m_End: 1\n";
        let rd = refid_records(t);
        assert_eq!(rd.order, vec!["10", "20"]);
        assert_eq!(rd.dups, set(&["10"]));
        let r10 = &rd.records["10"];
        // id items are stripped from payload, kept in the id set
        assert_eq!(r10.payload, "      a: one\n      b: two");
        assert_eq!(r10.ids, set(&["1", "2"]));
        assert_eq!(
            r10.spans,
            vec![Span { start: 4, end: 7 }, Span { start: 7, end: 10 }]
        );
        let r20 = &rd.records["20"];
        assert_eq!(r20.payload, "      c: three");
        assert!(r20.ids.is_empty());
        assert_eq!(r20.spans, vec![Span { start: 10, end: 12 }]);
    }

    #[test]
    fn refid_records_only_under_references() {
        // a RefIds-looking block under any other section key is ignored
        let t =
            "--- !u!114 &1\nMonoBehaviour:\n  m_Other:\n    RefIds:\n    - rid: 99\n      x: y\n";
        let rd = refid_records(t);
        assert!(rd.records.is_empty());
        assert!(rd.order.is_empty());
    }

    #[test]
    fn section_key_boundary() {
        assert!(is_section_key("  m_TableData:"));
        assert!(is_section_key("  references:"));
        assert!(!is_section_key("  - m_Id: 1")); // sequence item
        assert!(!is_section_key("    RefIds:")); // deeper indent
        assert!(!is_section_key("MonoBehaviour:")); // no indent
        assert!(!is_section_key("  ")); // no third char
    }
}
