//! The 3-way constructor.
//! SPEC section 4.
//!
//! Documents keyed by &anchor, table entries by m_Id and SerializeReference
//! records by rid all merge through one presence resolver, resolve_three_way.
//! A key present on both sides merges per block: the id-list region follows
//! the set rule, the rest goes through the P5 diff3 engine, whose all-guid
//! conflict regions are salvaged as ordered sets.
//! Record sections re-enter as placeholders anchored at the section header,
//! so an emptied section keeps its slot.
//!
//! Lines are split on '\n' carrying any trailing CR, and output reuses input
//! bytes wherever a side wins verbatim, so byte fidelity survives end to end.
//! The P8 verifier backstops every rule at the CLI layer.

use std::collections::{BTreeMap, BTreeSet};

use crate::diff3;
use crate::model::{self, Documents, Span};

/// A merged record section: block lines in output order plus whether any
/// conflict marker was emitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionMerge {
    pub lines: Vec<String>,
    pub conflict: bool,
}

// One document side, parsed once so every later query reads this view
// instead of re-parsing the text.
pub struct DocView<'a> {
    text: &'a str,
    table: model::TableData,
    refids: model::RefData,
}

impl<'a> DocView<'a> {
    pub fn new(text: &'a str) -> Self {
        DocView {
            text,
            table: model::table_entries(text),
            refids: model::refid_records(text),
        }
    }

    fn has_entries(&self) -> bool {
        !self.table.order.is_empty()
    }

    fn has_records(&self) -> bool {
        !self.refids.order.is_empty()
    }
}

/// Merge the m_TableData record set across three parsed sides.
pub fn merge_table(base: &DocView, ours: &DocView, theirs: &DocView) -> SectionMerge {
    merge_sections(
        &table_section(base),
        &table_section(ours),
        &table_section(theirs),
    )
}

/// Merge the references/RefIds record set across three parsed sides.
pub fn merge_refids(base: &DocView, ours: &DocView, theirs: &DocView) -> SectionMerge {
    merge_sections(
        &refids_section(base),
        &refids_section(ours),
        &refids_section(theirs),
    )
}

// A keyed section reduced to what the merge needs: key order, duplicated
// keys, and each key's occurrence blocks as raw lines.
// A table entry keyed by m_Id and a RefIds record keyed by rid are the same
// unit and merge through the same rules.
struct Section {
    order: Vec<String>,
    dups: BTreeSet<String>,
    blocks: BTreeMap<String, Vec<Vec<String>>>,
}

impl Section {
    fn has(&self, k: &str) -> bool {
        self.blocks.contains_key(k)
    }

    // First occurrence block; callers gate this on has, so the key is present.
    fn first(&self, k: &str) -> &[String] {
        &self.blocks[k][0]
    }
}

fn table_section(v: &DocView) -> Section {
    let td = &v.table;
    let spans = td
        .entries
        .iter()
        .map(|(k, e)| (k.clone(), e.spans.clone()))
        .collect();
    build_section(v.text, td.order.clone(), td.dups.clone(), spans)
}

fn refids_section(v: &DocView) -> Section {
    let rd = &v.refids;
    let spans = rd
        .records
        .iter()
        .map(|(k, r)| (k.clone(), r.spans.clone()))
        .collect();
    build_section(v.text, rd.order.clone(), rd.dups.clone(), spans)
}

fn build_section(
    text: &str,
    order: Vec<String>,
    dups: BTreeSet<String>,
    spans: BTreeMap<String, Vec<Span>>,
) -> Section {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut blocks: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    for (k, occ) in spans {
        let occs = occ
            .iter()
            .map(|s| {
                lines[s.start..s.end]
                    .iter()
                    .map(|l| (*l).to_string())
                    .collect()
            })
            .collect();
        blocks.insert(k, occs);
    }
    Section {
        order,
        dups,
        blocks,
    }
}

// Per-key merge outcome, shared by records and documents: survival, conflict
// flag, and the lines to emit, several when a duplicate is carried through.
struct Resolution {
    present: bool,
    conflict: bool,
    blocks: Vec<Vec<String>>,
}

// One key's view of the three sides: first-occurrence content, plus every
// occurrence on the surviving side for the inherited-duplicate carry.
struct KeyView {
    skip: bool,
    base: Option<Vec<String>>,
    ours: Option<Vec<String>>,
    theirs: Option<Vec<String>>,
    ours_all: Vec<Vec<String>>,
    theirs_all: Vec<Vec<String>>,
}

// SPEC 4.1 presence semantics, shared by record and document resolution.
// `merge_both` merges content when the key survives on both sides.
fn resolve_three_way(
    v: KeyView,
    merge_both: impl FnOnce(&[String], &[String], &[String]) -> (Vec<String>, bool),
) -> Resolution {
    if v.skip {
        // Inherited corruption: presence only, carried through unchanged.
        let blocks = if v.ours.is_some() {
            v.ours_all
        } else if v.theirs.is_some() {
            v.theirs_all
        } else {
            Vec::new()
        };
        return Resolution {
            present: !blocks.is_empty(),
            conflict: false,
            blocks,
        };
    }
    let keeper_is_ours = v.theirs.is_none();
    match (v.ours, v.theirs) {
        (Some(o), Some(t)) => {
            let empty: Vec<String> = Vec::new();
            let (blk, conflict) = merge_both(v.base.as_deref().unwrap_or(&empty), &o, &t);
            Resolution {
                present: true,
                conflict,
                blocks: vec![blk],
            }
        }
        (Some(kept), None) | (None, Some(kept)) => {
            match &v.base {
                // Added on one side, absent from base: keep it.
                None => Resolution {
                    present: true,
                    conflict: false,
                    blocks: vec![kept],
                },
                // Deleted on the other side, keeper unchanged: apply it.
                Some(b) if *b == kept => Resolution {
                    present: false,
                    conflict: false,
                    blocks: Vec::new(),
                },
                // Edited on one side, deleted on the other: a conflict.
                Some(_) => Resolution {
                    present: true,
                    conflict: true,
                    blocks: vec![edit_delete_block(&kept, keeper_is_ours)],
                },
            }
        }
        // Base only or nowhere: absent from the merge.
        (None, None) => Resolution {
            present: false,
            conflict: false,
            blocks: Vec::new(),
        },
    }
}

fn merge_sections(b: &Section, o: &Section, t: &Section) -> SectionMerge {
    // Keys duplicated in any input are inherited corruption per SPEC 4.4.
    let mut skip = b.dups.clone();
    skip.extend(o.dups.iter().cloned());
    skip.extend(t.dups.iter().cloned());

    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.extend(b.blocks.keys().cloned());
    keys.extend(o.blocks.keys().cloned());
    keys.extend(t.blocks.keys().cloned());

    let mut resolved: BTreeMap<String, Resolution> = BTreeMap::new();
    let mut present: BTreeMap<String, bool> = BTreeMap::new();
    for k in &keys {
        let r = resolve_key(k, &skip, b, o, t);
        present.insert(k.clone(), r.present);
        resolved.insert(k.clone(), r);
    }

    let order = reassemble(&o.order, &t.order, &present);
    let mut lines = Vec::new();
    let mut conflict = false;
    for k in &order {
        if let Some(r) = resolved.get(k) {
            for blk in &r.blocks {
                lines.extend(blk.iter().cloned());
            }
            conflict |= r.conflict;
        }
    }
    SectionMerge { lines, conflict }
}

fn resolve_key(
    k: &str,
    skip: &BTreeSet<String>,
    b: &Section,
    o: &Section,
    t: &Section,
) -> Resolution {
    let first = |s: &Section| s.has(k).then(|| s.first(k).to_vec());
    let all = |s: &Section| s.blocks.get(k).cloned().unwrap_or_default();
    resolve_three_way(
        KeyView {
            skip: skip.contains(k),
            base: first(b),
            ours: first(o),
            theirs: first(t),
            ours_all: all(o),
            theirs_all: all(t),
        },
        merge_keyed_block,
    )
}

// Two-way marker block for an edit on one side against a delete on the other.
fn edit_delete_block(kept: &[String], keeper_is_ours: bool) -> Vec<String> {
    let mut v = vec!["<<<<<<< ours".to_string()];
    if keeper_is_ours {
        v.extend(kept.iter().cloned());
    }
    v.push("=======".to_string());
    if !keeper_is_ours {
        v.extend(kept.iter().cloned());
    }
    v.push(">>>>>>> theirs".to_string());
    v
}

// Records in ours keep ours order.
// Theirs-only records attach after the nearest common preceding record, per
// SPEC 4.5.
fn reassemble(
    ours_order: &[String],
    theirs_order: &[String],
    present: &BTreeMap<String, bool>,
) -> Vec<String> {
    let ours_set: BTreeSet<&String> = ours_order.iter().collect();
    let is_present = |k: &String| present.get(k).copied().unwrap_or(false);

    let mut start_attach: Vec<String> = Vec::new();
    let mut after: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut anchor: Option<String> = None;
    for k in theirs_order {
        if ours_set.contains(k) {
            anchor = Some(k.clone());
        } else if is_present(k) {
            match &anchor {
                Some(a) => after.entry(a.clone()).or_default().push(k.clone()),
                None => start_attach.push(k.clone()),
            }
        }
    }

    let mut out = Vec::new();
    out.extend(start_attach);
    for k in ours_order {
        if is_present(k) {
            out.push(k.clone());
        }
        if let Some(list) = after.get(k) {
            out.extend(list.iter().cloned());
        }
    }
    out
}

// Merge line by line via the P5 engine.
// A '\n' is reattached per line so each keeps identity, then split back.
// Used for record blocks, document bodies and preambles alike.
fn diff3_lines(base: &[String], ours: &[String], theirs: &[String]) -> (Vec<String>, bool) {
    let bt = term(base);
    let ot = term(ours);
    let tt = term(theirs);
    let br: Vec<&str> = bt.iter().map(String::as_str).collect();
    let orr: Vec<&str> = ot.iter().map(String::as_str).collect();
    let trr: Vec<&str> = tt.iter().map(String::as_str).collect();
    let regions = diff3::diff3(&br, &orr, &trr);
    let (text, conflict) =
        diff3::render_merge_with(&regions, &diff3::Labels::default(), salvage_ref_region);
    (unterm(&text), conflict)
}

// SPEC 4.7: a conflict region of only same-indent guid-reference items, the
// registry and collection-list shape, merges as an ordered set instead of
// conflicting: ours' items first, theirs' additions appended, base removals
// honored.
// Registries are guid-unique by construction, so a within-side duplicate or
// any non-item line falls back to markers.
fn salvage_ref_region(ours: &[&str], base: &[&str], theirs: &[&str]) -> Option<Vec<String>> {
    ref_items_indent(ours.iter().chain(base).chain(theirs))?;
    let pairs = |ls: &[&str]| -> Vec<(String, String)> {
        ls.iter()
            .map(|l| {
                (
                    l.trim_end_matches(['\n', '\r']).to_string(),
                    (*l).to_string(),
                )
            })
            .collect()
    };
    let (op, tp) = (pairs(ours), pairs(theirs));
    let uniq = |v: &[(String, String)]| {
        v.iter()
            .map(|(k, _)| k.clone())
            .collect::<BTreeSet<_>>()
            .len()
            == v.len()
    };
    let bs: BTreeSet<String> = base
        .iter()
        .map(|l| l.trim_end_matches(['\n', '\r']).to_string())
        .collect();
    if !uniq(&op) || !uniq(&tp) || bs.len() != base.len() {
        return None; // a within-side duplicate: not a set, let it conflict
    }
    Some(ordered_set_merge(&bs, &op, &tp))
}

// The common indent of a region whose every line is a `- {fileID: ..,
// guid: .., type: ..}` flow reference item, else None.
fn ref_items_indent<'a>(lines: impl Iterator<Item = &'a &'a str>) -> Option<usize> {
    let mut indent: Option<usize> = None;
    for l in lines {
        let l = l.trim_end_matches(['\n', '\r']);
        let trimmed = l.trim_start_matches(' ');
        let ind = l.len() - trimmed.len();
        if ind == 0 || !is_flow_ref_item(trimmed) {
            return None;
        }
        match indent {
            None => indent = Some(ind),
            Some(i) if i != ind => return None,
            _ => {}
        }
    }
    indent
}

fn is_flow_ref_item(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("- {fileID: ") else {
        return false;
    };
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return false;
    }
    let rest = &rest[digits..];
    let Some(rest) = rest.strip_prefix(", guid: ") else {
        return false;
    };
    let hex = rest.len()
        - rest
            .trim_start_matches(|c: char| c.is_ascii_hexdigit())
            .len();
    if hex != 32 {
        return false;
    }
    let rest = &rest[hex..];
    let Some(rest) = rest.strip_prefix(", type: ") else {
        return false;
    };
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && rest[digits..].trim_end_matches(['\n', '\r']) == "}"
}

fn term(lines: &[String]) -> Vec<String> {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

fn unterm(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text.split('\n').map(str::to_string).collect();
    if v.last().is_some_and(String::is_empty) {
        v.pop();
    }
    v
}

// --- P6b: set-rule constructor inside a both-changed record block ---------

// Placeholder for a record's id-list run while the rest of the block merges
// by diff3.
// Uses the P7 NUL convention so it never collides with a real Unity YAML line.
const IDLIST_MARK: &str = "\u{0}uymerge-idlist\u{0}";

// A record's id-list region: the `m_Items:` or `m_SharedEntries:` header
// through its item run, plus the header indent, name, and each item's value
// and original line.
// The empty flow form `m_Items: []` is a present list with no items.
// `start` indexes the header, `end` is one past the region.
// `cr` records whether the header ends in CR, so synthesized lines keep the
// record's terminator style and a CRLF or mixed file stays byte identical,
// SPEC 2.5.
struct IdList {
    start: usize,
    end: usize,
    indent: String,
    name: String,
    cr: bool,
    items: Vec<(String, String)>,
}

// Merge a record changed on both sides.
// The constructor owns the whole id-list region including its header, so the
// SPEC 4.3 set rule alone decides members: two branches appending different
// ids reach the union, and a side that empties the list to `m_Items: []`
// cannot silently drop the other side's addition.
// The rest of the block merges by diff3 with the region masked to one
// placeholder line.
// A block with no id-list is a plain whole-block diff3, the P6 behavior.
fn merge_keyed_block(base: &[String], ours: &[String], theirs: &[String]) -> (Vec<String>, bool) {
    // SPEC 4.2: agreeing sides win verbatim.
    // This also carries inherited oddities like a value duplicated in one
    // list through byte identical, instead of silently repairing them.
    if ours == theirs {
        return (ours.to_vec(), false);
    }
    let bl = find_idlist(base);
    let ol = find_idlist(ours);
    let tl = find_idlist(theirs);
    if bl.is_none() && ol.is_none() && tl.is_none() {
        return diff3_lines(base, ours, theirs);
    }

    // Region-level scalar rule first: SPEC 4.2 on the whole list region as a
    // byte unit, so an unchanged side yields the other's region verbatim with
    // its exact bytes and terminator style.
    // Only a list changed on both sides is constructed.
    let b_reg = region_lines(base, bl.as_ref());
    let o_reg = region_lines(ours, ol.as_ref());
    let t_reg = region_lines(theirs, tl.as_ref());
    let region = if o_reg == t_reg {
        o_reg
    } else if o_reg == b_reg {
        t_reg
    } else if t_reg == b_reg {
        o_reg
    } else {
        let ids = merge_id_set(bl.as_ref(), ol.as_ref(), tl.as_ref());
        // Header indent, name and terminator come from ours, whose order
        // also wins emission.
        // The verifier compares normalized forms, so this arbitrary choice on
        // a genuine both-changed list is safe.
        match ol.as_ref().or(tl.as_ref()).or(bl.as_ref()) {
            Some(l) => emit_region(&l.indent, &l.name, l.cr, &ids),
            None => ids,
        }
    };
    let (masked, dconf) = diff3_lines(
        &mask_idlist(base, bl.as_ref()),
        &mask_idlist(ours, ol.as_ref()),
        &mask_idlist(theirs, tl.as_ref()),
    );

    let mut out = Vec::new();
    for line in masked {
        if line == IDLIST_MARK {
            out.extend(region.iter().cloned());
        } else {
            out.push(line);
        }
    }
    (out, dconf)
}

// A block's id-list region as raw lines, header through items; empty when
// the block has no list in either form.
fn region_lines(block: &[String], il: Option<&IdList>) -> Vec<String> {
    match il {
        Some(l) => block[l.start..l.end].to_vec(),
        None => Vec::new(),
    }
}

// The merged id-list region as lines: the canonical empty flow form when the
// set is empty, else the bare header followed by item lines.
// Indent and name are the block's own, so bytes stay editor faithful.
fn emit_region(indent: &str, name: &str, cr: bool, ids: &[String]) -> Vec<String> {
    let tail = if cr { "\r" } else { "" };
    if ids.is_empty() {
        vec![format!("{indent}{name}: []{tail}")]
    } else {
        let mut v = Vec::with_capacity(ids.len() + 1);
        v.push(format!("{indent}{name}:{tail}"));
        v.extend(ids.iter().cloned());
        v
    }
}

// SPEC 4.3 emission over keyed pairs, shared by the id-list constructor and
// the guid-reference salvage.
// The merged set is (b & o & t) | (o - b) | (t - b), emitted in ours' order
// with theirs' new members appended in theirs' order, each once.
// Original values are reused so bytes stay editor faithful.
// The formula is total: an add/remove contradiction cannot exist with a
// shared base, so a set never conflicts on its own.
fn ordered_set_merge<L: Clone>(
    base: &BTreeSet<String>,
    ours: &[(String, L)],
    theirs: &[(String, L)],
) -> Vec<L> {
    let os: BTreeSet<&String> = ours.iter().map(|(k, _)| k).collect();
    let ts: BTreeSet<&String> = theirs.iter().map(|(k, _)| k).collect();
    let keep =
        |k: &String| (base.contains(k) && os.contains(k) && ts.contains(k)) || !base.contains(k);
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for (k, v) in ours.iter().chain(theirs) {
        if keep(k) && emitted.insert(k.clone()) {
            out.push(v.clone());
        }
    }
    out
}

fn merge_id_set(b: Option<&IdList>, o: Option<&IdList>, t: Option<&IdList>) -> Vec<String> {
    let pairs = |il: Option<&IdList>| il.map(|l| l.items.clone()).unwrap_or_default();
    ordered_set_merge(&id_set(b), &pairs(o), &pairs(t))
}

fn id_set(il: Option<&IdList>) -> BTreeSet<String> {
    il.map(|l| l.items.iter().map(|(v, _)| v.clone()).collect())
        .unwrap_or_default()
}

// Replace a block's whole id-list region, header included, with one
// placeholder line; everything else passes through unchanged.
fn mask_idlist(lines: &[String], il: Option<&IdList>) -> Vec<String> {
    match il {
        None => lines.to_vec(),
        Some(il) => {
            let mut out = Vec::with_capacity(lines.len());
            out.extend_from_slice(&lines[..il.start]);
            out.push(IDLIST_MARK.to_string());
            out.extend_from_slice(&lines[il.end..]);
            out
        }
    }
}

// The first id-list region: an `m_Items:` or `m_SharedEntries:` header, bare
// with a run of `- rid:` or `- id:` items, or the empty flow `m_Items: []`.
// The empty form is a present list, so a side that empties the list is still
// masked and owned by the set rule.
fn find_idlist(lines: &[String]) -> Option<IdList> {
    for (h, line) in lines.iter().enumerate() {
        let Some((indent, name, empty_form, cr)) = idlist_header(line) else {
            continue;
        };
        let mut end = h + 1;
        let mut items = Vec::new();
        if !empty_form {
            while end < lines.len() {
                match id_item_value(&lines[end]) {
                    Some(v) => {
                        items.push((v, lines[end].clone()));
                        end += 1;
                    }
                    None => break,
                }
            }
        }
        return Some(IdList {
            start: h,
            end,
            indent,
            name,
            cr,
            items,
        });
    }
    None
}

// Parse an id-list header into indent, field name, empty-flow flag and CR
// flag.
// Only `m_Items` and `m_SharedEntries` in bare or `[]` form qualify; an inline
// populated flow like `[1, 2]` is left to diff3.
// One trailing CR is tolerated and recorded, so the region is owned in CRLF
// files too and synthesis keeps the terminator.
fn idlist_header(line: &str) -> Option<(String, String, bool, bool)> {
    let cr = line.ends_with('\r');
    let line = model::strip_cr(line);
    let trimmed = line.trim_start_matches(char::is_whitespace);
    let indent_len = line.len() - trimmed.len();
    if indent_len == 0 {
        return None;
    }
    for name in ["m_Items", "m_SharedEntries"] {
        if let Some(rest) = trimmed.strip_prefix(name).and_then(|r| r.strip_prefix(':')) {
            return match rest {
                "" => Some((line[..indent_len].to_string(), name.to_string(), false, cr)),
                " []" => Some((line[..indent_len].to_string(), name.to_string(), true, cr)),
                _ => None,
            };
        }
    }
    None
}

// Value of an `- rid: N` or `- id: N` sequence item, else None.
// Matches the model item parsers: leading whitespace required, tail a signed
// int, one trailing CR tolerated and excluded so ids compare equal across
// line-ending styles while emission reuses the original bytes.
fn id_item_value(line: &str) -> Option<String> {
    let line = model::strip_cr(line);
    let trimmed = line.trim_start_matches(char::is_whitespace);
    if trimmed.len() == line.len() {
        return None;
    }
    let rest = trimmed
        .strip_prefix("- rid: ")
        .or_else(|| trimmed.strip_prefix("- id: "))?;
    let bytes = rest.as_bytes();
    let mut i = usize::from(bytes.first() == Some(&b'-'));
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start || i != bytes.len() {
        return None;
    }
    Some(rest.to_string())
}

// --- P7: document-level composition --------------------------------------

/// A merged whole file: output lines in a terminator-free line space, and
/// whether any conflict marker was emitted.
/// Join with '\n' to render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMerge {
    pub lines: Vec<String>,
    pub conflict: bool,
}

// Placeholders for a keyed record run while its document body merges by diff3.
// NUL cannot occur in Unity YAML, so a placeholder never collides with a real
// line and stays one stable line through the merge.
const TABLE_MARK: &str = "\u{0}uymerge-table\u{0}";
const REFIDS_MARK: &str = "\u{0}uymerge-refids\u{0}";

/// Merge three unwrapped files by document set and per-document dispatch.
/// Base, ours and theirs are unwrapped texts; the result is the composed file
/// plus a conflict flag.
/// No rewrap and no CRLF restore: that is the CLI's job.
pub fn merge_file(base: &str, ours: &str, theirs: &str) -> FileMerge {
    let bl: Vec<&str> = base.split('\n').collect();
    let ol: Vec<&str> = ours.split('\n').collect();
    let tl: Vec<&str> = theirs.split('\n').collect();
    let bd = model::documents(base);
    let od = model::documents(ours);
    let td = model::documents(theirs);

    let mut out: Vec<String> = Vec::new();
    let mut conflict = false;

    // The preamble is a synthetic document; it merges as plain body content.
    let (pre, pconf) = diff3_lines(
        &span_lines(&bl, bd.preamble),
        &span_lines(&ol, od.preamble),
        &span_lines(&tl, td.preamble),
    );
    out.extend(pre);
    conflict |= pconf;

    // Anchors duplicated in any input are inherited corruption: presence only.
    let dups: BTreeSet<String> = bd
        .dups
        .iter()
        .chain(od.dups.iter())
        .chain(td.dups.iter())
        .cloned()
        .collect();

    let mut anchors: BTreeSet<String> = BTreeSet::new();
    anchors.extend(bd.docs.keys().cloned());
    anchors.extend(od.docs.keys().cloned());
    anchors.extend(td.docs.keys().cloned());

    let mut present: BTreeMap<String, bool> = BTreeMap::new();
    let mut content: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for a in &anchors {
        let r = resolve_document(a, &dups, &bl, &bd, &ol, &od, &tl, &td);
        present.insert(a.clone(), r.present);
        conflict |= r.conflict;
        if r.present {
            content.insert(a.clone(), r.blocks.concat());
        }
    }

    // Documents in ours keep ours' order; theirs-only ones follow neighbor
    // order, the same reassembly used for records, SPEC 4.5.
    let order = reassemble(&dedup(&od.order), &dedup(&td.order), &present);
    for a in &order {
        if let Some(lines) = content.get(a) {
            out.extend(lines.iter().cloned());
        }
    }
    FileMerge {
        lines: out,
        conflict,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_document(
    a: &str,
    dups: &BTreeSet<String>,
    bl: &[&str],
    bd: &Documents,
    ol: &[&str],
    od: &Documents,
    tl: &[&str],
    td: &Documents,
) -> Resolution {
    let doc =
        |lines: &[&str], d: &Documents| d.docs.contains_key(a).then(|| doc_lines(lines, d, a));
    let ours = doc(ol, od);
    let theirs = doc(tl, td);
    // The model keeps one span per duplicated anchor, so an inherited
    // duplicate collapses to its first occurrence, which the set-based
    // verifier accepts.
    resolve_three_way(
        KeyView {
            skip: dups.contains(a),
            base: doc(bl, bd),
            ours: ours.clone(),
            theirs: theirs.clone(),
            ours_all: ours.into_iter().collect(),
            theirs_all: theirs.into_iter().collect(),
        },
        |b, o, t| merge_document(&b.join("\n"), &o.join("\n"), &t.join("\n")),
    )
}

// Merge one document present on both sides.
// A document with keyed record runs merges those runs structurally and its
// body by diff3 with the runs masked as atomic placeholders.
// A document with no keyed run merges wholly by diff3.
fn merge_document(base: &str, ours: &str, theirs: &str) -> (Vec<String>, bool) {
    let (b, o, t) = (DocView::new(base), DocView::new(ours), DocView::new(theirs));
    let has_table = b.has_entries() || o.has_entries() || t.has_entries();
    let has_refs = b.has_records() || o.has_records() || t.has_records();
    if !has_table && !has_refs {
        return diff3_lines(&text_lines(base), &text_lines(ours), &text_lines(theirs));
    }
    let table = has_table.then(|| merge_table(&b, &o, &t));
    let refids = has_refs.then(|| merge_refids(&b, &o, &t));
    let (masked, dconf) = diff3_lines(&mask(&b), &mask(&o), &mask(&t));

    // A section conflict counts even when a surrounding delete drops its
    // placeholder, so a keyed conflict never slips out as a clean exit.
    // The P8 verifier rechecks the assembled output as a backstop.
    let mut conflict = dconf;
    if let Some(s) = &table {
        conflict |= s.conflict;
    }
    if let Some(s) = &refids {
        conflict |= s.conflict;
    }

    let mut out = Vec::new();
    for line in masked {
        if line == TABLE_MARK {
            if let Some(s) = &table {
                out.extend(s.lines.iter().cloned());
            }
        } else if line == REFIDS_MARK {
            if let Some(s) = &refids {
                out.extend(s.lines.iter().cloned());
            }
        } else {
            out.push(line);
        }
    }
    (out, conflict)
}

// Replace the table entry run and the RefIds record run with one placeholder
// line each; every other line passes through.
// The runs are contiguous, so a placeholder holds the section's exact position.
fn mask(v: &DocView) -> Vec<String> {
    let text = v.text;
    let lines: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    };
    // One placeholder per section, emitted exactly once.
    // An EMPTY run, a header with every record deleted, has start == end: the
    // mark is inserted before line `start` and the scan must still advance, or
    // it loops forever appending marks until memory dies.
    let mut runs = [
        (table_run(v), TABLE_MARK, false),
        (refids_run(v), REFIDS_MARK, false),
    ];
    let mut out = Vec::new();
    let mut i = 0;
    'scan: while i < lines.len() {
        for (run, mark, done) in &mut runs {
            if let Some((s, e)) = run {
                if i == *s && !*done {
                    *done = true;
                    out.push((*mark).to_string());
                    if *e > i {
                        i = *e;
                        continue 'scan;
                    }
                }
            }
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    out
}

// The record run of a section, anchored at the header so an EMPTY section
// still yields a placeholder.
// Without the anchor, a side that deleted every record has no placeholder,
// the masked diff3 treats the section as removed, and the merged section with
// the other side's additions is dropped.
fn section_run(
    text: &str,
    header_pred: impl Fn(&str) -> bool,
    spans: impl Iterator<Item = Span>,
) -> Option<(usize, usize)> {
    let header = text.split('\n').position(header_pred)?;
    let end = span_bounds(spans).map_or(header + 1, |(_, e)| e);
    Some((header + 1, end))
}

fn table_run(v: &DocView) -> Option<(usize, usize)> {
    section_run(
        v.text,
        model::is_table_header,
        v.table
            .entries
            .values()
            .flat_map(|e| e.spans.iter().copied()),
    )
}

fn refids_run(v: &DocView) -> Option<(usize, usize)> {
    section_run(
        v.text,
        model::is_refids_header,
        v.refids
            .records
            .values()
            .flat_map(|r| r.spans.iter().copied()),
    )
}

// Smallest start and largest end over a set of spans, or None when empty.
fn span_bounds(spans: impl Iterator<Item = Span>) -> Option<(usize, usize)> {
    let mut it = spans;
    let first = it.next()?;
    let mut lo = first.start;
    let mut hi = first.end;
    for s in it {
        lo = lo.min(s.start);
        hi = hi.max(s.end);
    }
    Some((lo, hi))
}

fn span_lines(lines: &[&str], span: Option<Span>) -> Vec<String> {
    match span {
        Some(s) => lines[s.start..s.end]
            .iter()
            .map(|l| (*l).to_string())
            .collect(),
        None => Vec::new(),
    }
}

fn doc_lines(lines: &[&str], docs: &Documents, a: &str) -> Vec<String> {
    span_lines(lines, docs.docs.get(a).copied())
}

// Split a body into terminator-free lines, treating empty text as no lines
// rather than one blank line, so an absent document contributes nothing.
fn text_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').map(str::to_string).collect()
    }
}

// First occurrence of each key, order preserved.
// Otherwise a duplicate anchor would emit a document twice from one span.
fn dedup(order: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for k in order {
        if seen.insert(k.clone()) {
            out.push(k.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, loc: &str) -> String {
        format!("  - m_Id: {id}\n    m_Localized: {loc}\n    m_Metadata:\n      m_Items: []")
    }

    fn entry_rids(id: &str, loc: &str, rids: &[&str]) -> String {
        let mut s =
            format!("  - m_Id: {id}\n    m_Localized: {loc}\n    m_Metadata:\n      m_Items:");
        for r in rids {
            s.push_str(&format!("\n      - rid: {r}"));
        }
        s
    }

    fn tbl(entries: &[String]) -> String {
        let mut s = String::from("--- !u!114 &1\nMonoBehaviour:\n  m_TableData:\n");
        for e in entries {
            s.push_str(e);
            s.push('\n');
        }
        s.push_str("  references:\n    version: 2\n");
        s
    }

    fn refrec(rid: &str, payload: &str, ids: &[&str]) -> String {
        let mut s =
            format!("    - rid: {rid}\n      {payload}\n      data:\n        m_SharedEntries:");
        for i in ids {
            s.push_str(&format!("\n        - id: {i}"));
        }
        s
    }

    fn refs(records: &[String]) -> String {
        let mut s = String::from(
            "--- !u!114 &1\nMonoBehaviour:\n  references:\n    version: 2\n    RefIds:\n",
        );
        for r in records {
            s.push_str(r);
            s.push('\n');
        }
        s
    }

    fn joined(m: &SectionMerge) -> String {
        m.lines.join("\n")
    }

    #[test]
    fn table_noop_keeps_all_entries() {
        let doc = tbl(&[entry("100", "a"), entry("200", "b")]);
        let m = mt(&doc, &doc, &doc);
        assert!(!m.conflict);
        assert_eq!(
            joined(&m),
            [entry("100", "a"), entry("200", "b")].join("\n")
        );
    }

    #[test]
    fn table_ours_edit_is_kept() {
        let base = tbl(&[entry("100", "a"), entry("200", "b")]);
        let ours = tbl(&[entry("100", "EDITED"), entry("200", "b")]);
        let theirs = base.clone();
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        assert_eq!(
            joined(&m),
            [entry("100", "EDITED"), entry("200", "b")].join("\n")
        );
    }

    #[test]
    fn table_theirs_add_appends_after_neighbor() {
        let base = tbl(&[entry("100", "a"), entry("200", "b")]);
        let ours = base.clone();
        let theirs = tbl(&[entry("100", "a"), entry("150", "new"), entry("200", "b")]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        assert_eq!(
            joined(&m),
            [entry("100", "a"), entry("150", "new"), entry("200", "b")].join("\n")
        );
    }

    #[test]
    fn table_clean_delete_drops_entry() {
        let base = tbl(&[entry("100", "a"), entry("200", "b")]);
        let ours = tbl(&[entry("100", "a")]);
        let theirs = base.clone();
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        assert_eq!(joined(&m), entry("100", "a"));
    }

    #[test]
    fn table_edit_delete_conflicts() {
        let base = tbl(&[entry("100", "a"), entry("200", "b")]);
        // ours deletes 200, theirs edits 200
        let ours = tbl(&[entry("100", "a")]);
        let theirs = tbl(&[entry("100", "a"), entry("200", "CHANGED")]);
        let m = mt(&base, &ours, &theirs);
        assert!(m.conflict);
        let text = joined(&m);
        assert!(text.contains("<<<<<<< ours"));
        assert!(text.contains(">>>>>>> theirs"));
        assert!(text.contains("CHANGED"));
    }

    #[test]
    fn table_both_edit_same_field_conflicts() {
        let base = tbl(&[entry("100", "a")]);
        let ours = tbl(&[entry("100", "OURS")]);
        let theirs = tbl(&[entry("100", "THEIRS")]);
        let m = mt(&base, &ours, &theirs);
        assert!(m.conflict);
        let text = joined(&m);
        assert!(text.contains("OURS"));
        assert!(text.contains("THEIRS"));
    }

    #[test]
    fn table_both_add_same_entry_is_clean() {
        let base = tbl(&[entry("100", "a")]);
        let add = entry("200", "b");
        let ours = tbl(&[entry("100", "a"), add.clone()]);
        let theirs = tbl(&[entry("100", "a"), add.clone()]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        assert_eq!(
            joined(&m),
            [entry("100", "a"), entry("200", "b")].join("\n")
        );
    }

    #[test]
    fn table_disjoint_edit_and_meta_add_merge_clean() {
        // Ours edits the localized text, theirs adds a metadata rid on a
        // different line, so the block diff3 merges both without conflict.
        let base = tbl(&[entry_rids("100", "a", &[])]);
        let ours = tbl(&[entry_rids("100", "EDIT", &[])]);
        let theirs = tbl(&[entry_rids("100", "a", &["5"])]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(text.contains("m_Localized: EDIT"));
        assert!(text.contains("- rid: 5"));
    }

    #[test]
    fn refids_theirs_add_id_is_clean() {
        let base = refs(&[refrec("10", "type: A", &["1"])]);
        let ours = base.clone();
        let theirs = refs(&[refrec("10", "type: A", &["1", "2"])]);
        let m = mr(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(text.contains("- id: 1"));
        assert!(text.contains("- id: 2"));
    }

    #[test]
    fn refids_new_record_appended() {
        let base = refs(&[refrec("10", "type: A", &["1"])]);
        let ours = base.clone();
        let theirs = refs(&[
            refrec("10", "type: A", &["1"]),
            refrec("20", "type: B", &["2"]),
        ]);
        let m = mr(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(text.contains("- rid: 20"));
        assert!(text.contains("type: B"));
    }

    #[test]
    fn duplicate_key_is_carried_through_without_conflict() {
        // 100 duplicated in ours is inherited corruption: presence only, both
        // occurrences carried through, no content merge, no conflict.
        let base = tbl(&[entry("100", "a")]);
        let ours = tbl(&[entry("100", "a"), entry("100", "a")]);
        let theirs = base.clone();
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        assert_eq!(joined(&m).matches("m_Id: 100").count(), 2);
    }

    #[test]
    fn empty_inputs_yield_empty_merge() {
        let doc = "--- !u!1 &1\nGameObject:\n  m_Name: x\n";
        let m = mt(doc, doc, doc);
        assert!(!m.conflict);
        assert!(m.lines.is_empty());
    }

    // --- P6b: set-rule constructor inside a both-changed record ----------

    fn mr(b: &str, o: &str, th: &str) -> SectionMerge {
        merge_refids(&DocView::new(b), &DocView::new(o), &DocView::new(th))
    }

    fn mt(b: &str, o: &str, th: &str) -> SectionMerge {
        merge_table(&DocView::new(b), &DocView::new(o), &DocView::new(th))
    }

    fn reg(guids: &[&str]) -> String {
        let mut s = String::from("--- !u!114 &1\nMonoBehaviour:\n  m_Name: Registry\n  items:\n");
        for g in guids {
            s.push_str(&format!("  - {{fileID: 11400000, guid: {g}, type: 2}}\n"));
        }
        s.push_str("  count: 1\n");
        s
    }

    const GA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GB: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const GC: &str = "cccccccccccccccccccccccccccccccc";
    const GD: &str = "dddddddddddddddddddddddddddddddd";

    #[test]
    fn registry_concurrent_appends_union() {
        // SPEC 4.7, the classic registry conflict: both sides append a
        // different reference at the same position, and an all-guid region
        // merges as a set instead of conflicting.
        let base = reg(&[GA]);
        let ours = reg(&[GA, GB]);
        let theirs = reg(&[GA, GC]);
        let m = merge_file(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = rendered(&m);
        assert!(text.contains(GB) && text.contains(GC));
        assert!(text.find(GB).unwrap() < text.find(GC).unwrap());
    }

    #[test]
    fn registry_removal_beats_membership() {
        // ours removes GA while theirs appends GD: removal honored, append kept
        let base = reg(&[GA, GB]);
        let ours = reg(&[GB]);
        let theirs = reg(&[GA, GB, GD]);
        let m = merge_file(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = rendered(&m);
        assert!(!text.contains(GA));
        assert!(text.contains(GD));
    }

    #[test]
    fn registry_duplicate_within_side_still_conflicts() {
        // a within-side duplicate means the run is not a set; fall back loud
        let base = reg(&[GA]);
        let ours = reg(&[GA, GB, GB]);
        let theirs = reg(&[GA, GC]);
        let m = merge_file(&base, &ours, &theirs);
        assert!(m.conflict);
    }

    #[test]
    fn mixed_content_regions_still_conflict() {
        // non-item lines in the region keep the plain conflict behavior
        let base = "--- !u!114 &1\nMonoBehaviour:\n  v: old\n".to_string();
        let ours = base.replace("old", "mine");
        let theirs = base.replace("old", "yours");
        let m = merge_file(&base, &ours, &theirs);
        assert!(m.conflict);
    }

    #[test]
    fn both_sides_empty_the_table_terminates() {
        // The 22 GB memory bomb: base has one entry, both sides deleted it,
        // leaving a bare section header.
        // The empty run has start == end, so the mask scan must still advance
        // instead of appending placeholder marks forever.
        // Found by the model checker as a runaway process.
        let base = tbl(&[entry("100", "sigma")]);
        let empty = tbl(&[]);
        let m = merge_file(&base, &empty, &empty);
        assert!(!m.conflict);
        assert!(!rendered(&m).contains("m_Id: 100"));
    }

    #[test]
    fn shared_table_entries_merge_structurally() {
        // SharedTableData keeps its key map under m_Entries, not m_TableData,
        // but the same record shape.
        // Concurrent key additions on both sides must union structurally
        // instead of falling to line diff3, a quarter of the flip friction.
        let mk = |extra: &str| {
            format!(
                "--- !u!114 &1\nMonoBehaviour:\n  m_Name: Shared\n  m_Entries:\n  \
                 - m_Id: 100\n    m_Key: menu.pause\n    m_Metadata:\n      m_Items: []\n{extra}"
            )
        };
        let base = mk("");
        let ours = mk("  - m_Id: 200\n    m_Key: ours.key\n    m_Metadata:\n      m_Items: []\n");
        let theirs =
            mk("  - m_Id: 300\n    m_Key: theirs.key\n    m_Metadata:\n      m_Items: []\n");
        let m = merge_file(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = rendered(&m);
        assert!(text.contains("ours.key"));
        assert!(text.contains("theirs.key"));
        assert!(text.contains("menu.pause"));
    }

    #[test]
    fn terminator_flip_takes_theirs_region_bytes() {
        // Exotic battery E4a: theirs rewrites the file to CRLF, ours is
        // untouched.
        // The id-list region must come from theirs verbatim, not be
        // resynthesized with ours' terminator, so take-theirs stays byte exact.
        let base = tbl(&[entry("100", "a"), entry("200", "b")]);
        let theirs = base.replace('\n', "\r\n");
        let m = mt(&base, &theirs, &base);
        assert!(!m.conflict);
        // merge_table emits the record section only; the byte-source truth
        // for take-theirs is theirs' own section, via a no-op on theirs.
        let want = mt(&theirs, &theirs, &theirs);
        assert_eq!(joined(&m), joined(&want));
        assert!(joined(&m).contains("m_Items: []\r"));
    }

    #[test]
    fn crlf_concurrent_add_keeps_terminators_and_unions() {
        // CRLF variant of the concurrent-add case.
        // The region is owned under CRLF too: ids compare equal across line
        // endings, original item bytes are reused, and the header keeps the CR.
        let base = tbl(&[entry_rids("100", "a", &["1"])]).replace('\n', "\r\n");
        let ours = tbl(&[entry_rids("100", "a", &["1", "5"])]).replace('\n', "\r\n");
        let theirs = tbl(&[entry_rids("100", "a", &["1", "7"])]).replace('\n', "\r\n");
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(text.contains("m_Items:\r"));
        assert!(text.contains("- rid: 1\r"));
        assert!(text.contains("- rid: 5\r"));
        assert!(text.contains("- rid: 7\r"));
    }

    #[test]
    fn table_concurrent_add_rids_unions() {
        // Both sides append a different rid to the same entry's m_Items list.
        // Plain diff3 conflicts on the shared insertion point; the set rule
        // merges to the union in ours-then-theirs order.
        let base = tbl(&[entry_rids("100", "a", &["1"])]);
        let ours = tbl(&[entry_rids("100", "a", &["1", "5"])]);
        let theirs = tbl(&[entry_rids("100", "a", &["1", "7"])]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(text.contains("- rid: 1"));
        assert!(text.contains("- rid: 5"));
        assert!(text.contains("- rid: 7"));
    }

    #[test]
    fn table_concurrent_make_smart_from_empty_unions() {
        // The review scenario: two designers make the same string smart at
        // once, each adding a rid where base had `m_Items: []`.
        // Both changes land as the union.
        let base = tbl(&[entry("100", "a")]);
        let ours = tbl(&[entry_rids("100", "a", &["5"])]);
        let theirs = tbl(&[entry_rids("100", "a", &["7"])]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(text.contains("- rid: 5"));
        assert!(text.contains("- rid: 7"));
    }

    #[test]
    fn table_add_and_remove_rid_merge_clean() {
        // Ours removes a rid, theirs adds a different one: both apply, no
        // conflict, per the set rule union and removal semantics.
        let base = tbl(&[entry_rids("100", "a", &["5", "7"])]);
        let ours = tbl(&[entry_rids("100", "a", &["7"])]);
        let theirs = tbl(&[entry_rids("100", "a", &["5", "7", "9"])]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(!text.contains("- rid: 5"));
        assert!(text.contains("- rid: 7"));
        assert!(text.contains("- rid: 9"));
    }

    #[test]
    fn table_set_merge_keeps_text_conflict() {
        // The id lists union cleanly, but the localized text is edited
        // differently on both sides, so the block still conflicts by diff3.
        let base = tbl(&[entry_rids("100", "a", &["1"])]);
        let ours = tbl(&[entry_rids("100", "OURS", &["1", "5"])]);
        let theirs = tbl(&[entry_rids("100", "THEIRS", &["1", "7"])]);
        let m = mt(&base, &ours, &theirs);
        assert!(m.conflict);
        let text = joined(&m);
        assert!(text.contains("OURS"));
        assert!(text.contains("THEIRS"));
    }

    #[test]
    fn table_ours_empties_while_theirs_adds_keeps_addition() {
        // Ours removes every rid, so its block carries `m_Items: []`; theirs
        // keeps rid 1 and adds rid 7.
        // The set rule owns the whole region, so theirs' addition is not
        // dropped by a masked-header diff3.
        let base = tbl(&[entry_rids("100", "a", &["1"])]);
        let ours = tbl(&[entry("100", "a")]);
        let theirs = tbl(&[entry_rids("100", "a", &["1", "7"])]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(!text.contains("- rid: 1"));
        assert!(text.contains("- rid: 7"));
        assert!(!text.contains("m_Items: []"));
    }

    #[test]
    fn table_theirs_empties_while_ours_adds_keeps_addition() {
        let base = tbl(&[entry_rids("100", "a", &["1"])]);
        let ours = tbl(&[entry_rids("100", "a", &["1", "5"])]);
        let theirs = tbl(&[entry("100", "a")]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(!text.contains("- rid: 1"));
        assert!(text.contains("- rid: 5"));
        assert!(!text.contains("m_Items: []"));
    }

    #[test]
    fn table_both_empty_the_list_yields_empty_form() {
        // Both sides remove every rid; the merged region is the canonical
        // empty flow form, not a bare header.
        let base = tbl(&[entry_rids("100", "a", &["1"])]);
        let ours = tbl(&[entry("100", "a")]);
        let theirs = tbl(&[entry("100", "a")]);
        let m = mt(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(!text.contains("- rid: 1"));
        assert!(text.contains("m_Items: []"));
    }

    #[test]
    fn refids_ours_empties_while_theirs_adds_keeps_addition() {
        let base = refs(&[refrec("10", "type: A", &["1"])]);
        let ours = refs(&[refrec("10", "type: A", &[])]);
        let theirs = refs(&[refrec("10", "type: A", &["1", "2"])]);
        let m = mr(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(!text.contains("- id: 1"));
        assert!(text.contains("- id: 2"));
        assert!(!text.contains("m_SharedEntries: []"));
    }

    #[test]
    fn refids_theirs_empties_while_ours_adds_keeps_addition() {
        let base = refs(&[refrec("10", "type: A", &["1"])]);
        let ours = refs(&[refrec("10", "type: A", &["1", "3"])]);
        let theirs = refs(&[refrec("10", "type: A", &[])]);
        let m = mr(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(!text.contains("- id: 1"));
        assert!(text.contains("- id: 3"));
        assert!(!text.contains("m_SharedEntries: []"));
    }

    #[test]
    fn refids_concurrent_add_ids_unions() {
        // Both sides append a different id to the same record's
        // m_SharedEntries list, and the set rule merges to the union.
        let base = refs(&[refrec("10", "type: A", &["1"])]);
        let ours = refs(&[refrec("10", "type: A", &["1", "2"])]);
        let theirs = refs(&[refrec("10", "type: A", &["1", "3"])]);
        let m = mr(&base, &ours, &theirs);
        assert!(!m.conflict);
        let text = joined(&m);
        assert!(text.contains("- id: 1"));
        assert!(text.contains("- id: 2"));
        assert!(text.contains("- id: 3"));
    }

    // --- P7: document composition ----------------------------------------

    const PREFAB: &str = include_str!("../tests/fixtures/inputs/prefab-multidoc.prefab");
    const TABLE: &str = include_str!("../tests/fixtures/inputs/table-with-refs.asset");

    fn rendered(m: &FileMerge) -> String {
        m.lines.join("\n")
    }

    #[test]
    fn file_noop_multidoc_is_byte_identical() {
        let m = merge_file(PREFAB, PREFAB, PREFAB);
        assert!(!m.conflict);
        assert_eq!(rendered(&m), PREFAB);
    }

    #[test]
    fn record_payload_closing_at_column_zero_survives_noop() {
        // P10 regression: a quoted payload can close at column zero.
        // The content scan stops there but the emission span must not, or the
        // mask swallows the close quote and re-emission un-terminates the
        // scalar, mangling every later record.
        let doc = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n\
                   --- !u!114 &11400000\nMonoBehaviour:\n  m_Name: Shared\n\
                   \x20 references:\n    version: 2\n    RefIds:\n\
                   \x20   - rid: 100\n      data:\n        m_CommentText: '\n\n'\n\
                   \x20   - rid: 200\n      data:\n        m_CommentText: after\n";
        let m = merge_file(doc, doc, doc);
        assert!(!m.conflict);
        assert_eq!(rendered(&m), doc);
    }

    #[test]
    fn within_list_duplicate_rid_survives_noop() {
        // P10 regression: an entry carrying the same rid twice is inherited
        // corruption; agreeing sides pass through verbatim rather than being
        // silently deduplicated on a no-op.
        let base = tbl(&[entry_rids("100", "a", &["7", "7"])]);
        let m = mt(&base, &base, &base);
        assert!(!m.conflict);
        assert_eq!(joined(&m).matches("- rid: 7").count(), 2);
    }

    #[test]
    fn file_noop_keyed_doc_is_byte_identical() {
        // A document with both m_TableData and references/RefIds round-trips
        // through masking, keyed merge and reassembly with no byte change.
        let m = merge_file(TABLE, TABLE, TABLE);
        assert!(!m.conflict);
        assert_eq!(rendered(&m), TABLE);
    }

    #[test]
    fn document_added_on_theirs_appends_by_neighbor() {
        let base = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n";
        let ours = base;
        let theirs = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n--- !u!1 &200\nGameObject:\n  m_Name: B\n";
        let m = merge_file(base, ours, theirs);
        assert!(!m.conflict);
        assert_eq!(rendered(&m), theirs);
    }

    #[test]
    fn document_deleted_on_ours_is_dropped() {
        let base = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n--- !u!1 &200\nGameObject:\n  m_Name: B\n";
        let ours = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n";
        let theirs = base;
        let m = merge_file(base, ours, theirs);
        assert!(!m.conflict);
        assert_eq!(rendered(&m), ours);
    }

    #[test]
    fn document_added_both_sides_identically_is_clean() {
        let base = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n";
        let add = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n--- !u!1 &200\nGameObject:\n  m_Name: B\n";
        let m = merge_file(base, add, add);
        assert!(!m.conflict);
        assert_eq!(rendered(&m), add);
    }

    #[test]
    fn document_edit_delete_conflicts() {
        let base = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n--- !u!1 &200\nGameObject:\n  m_Name: B\n";
        // ours deletes 200, theirs edits it
        let ours = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n";
        let theirs = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n--- !u!1 &200\nGameObject:\n  m_Name: CHANGED\n";
        let m = merge_file(base, ours, theirs);
        assert!(m.conflict);
        let text = rendered(&m);
        assert!(text.contains("<<<<<<< ours"));
        assert!(text.contains(">>>>>>> theirs"));
        assert!(text.contains("CHANGED"));
    }

    #[test]
    fn plain_document_body_conflicts_by_diff3() {
        let base = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 1\n";
        let ours = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 2\n";
        let theirs = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Value: 3\n";
        let m = merge_file(base, ours, theirs);
        assert!(m.conflict);
        let text = rendered(&m);
        assert!(text.contains("  m_Value: 2"));
        assert!(text.contains("  m_Value: 3"));
    }

    #[test]
    fn plain_document_disjoint_edits_merge_clean() {
        // The two edits sit on either side of an unchanged line, so diff3 keeps
        // them apart and merges both, matching git merge-file.
        let base = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  a: 1\n  m: 0\n  b: 1\n";
        let ours = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  a: 9\n  m: 0\n  b: 1\n";
        let theirs = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  a: 1\n  m: 0\n  b: 9\n";
        let m = merge_file(base, ours, theirs);
        assert!(!m.conflict);
        assert_eq!(
            rendered(&m),
            "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  a: 9\n  m: 0\n  b: 9\n"
        );
    }

    // A document body edit and a keyed entry edit on opposite sides must both
    // land: the body merges by diff3, the entries by the keyed rules, and the
    // record run stays out of the body diff3 as an atomic placeholder.
    #[test]
    fn keyed_and_body_edits_on_opposite_sides_merge() {
        let base = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Name: T\n  m_TableData:\n  - m_Id: 100\n    m_Localized: a\n  references:\n    version: 2\n";
        // ours edits the entry text, theirs edits the body name
        let ours = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Name: T\n  m_TableData:\n  - m_Id: 100\n    m_Localized: EDIT\n  references:\n    version: 2\n";
        let theirs = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_Name: RENAMED\n  m_TableData:\n  - m_Id: 100\n    m_Localized: a\n  references:\n    version: 2\n";
        let m = merge_file(base, ours, theirs);
        assert!(!m.conflict);
        let text = rendered(&m);
        assert!(text.contains("m_Name: RENAMED"));
        assert!(text.contains("m_Localized: EDIT"));
    }

    // Adding an entry on one side and a whole record on the other, in the same
    // keyed document, must merge both without conflict.
    #[test]
    fn keyed_entry_and_record_adds_merge() {
        let base = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_TableData:\n  - m_Id: 100\n    m_Localized: a\n  references:\n    version: 2\n    RefIds:\n    - rid: 10\n      type: A\n";
        let ours = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_TableData:\n  - m_Id: 100\n    m_Localized: a\n  - m_Id: 200\n    m_Localized: b\n  references:\n    version: 2\n    RefIds:\n    - rid: 10\n      type: A\n";
        let theirs = "%YAML 1.1\n--- !u!114 &1\nMonoBehaviour:\n  m_TableData:\n  - m_Id: 100\n    m_Localized: a\n  references:\n    version: 2\n    RefIds:\n    - rid: 10\n      type: A\n    - rid: 20\n      type: B\n";
        let m = merge_file(base, ours, theirs);
        assert!(!m.conflict);
        let text = rendered(&m);
        assert!(text.contains("m_Id: 200"));
        assert!(text.contains("- rid: 20"));
    }

    #[test]
    fn preamble_edit_on_one_side_is_kept() {
        let base = "%YAML 1.1\n%TAG !u! x\n--- !u!1 &1\nGameObject:\n  m_Name: A\n";
        let ours = "%YAML 1.1\n%TAG !u! y\n--- !u!1 &1\nGameObject:\n  m_Name: A\n";
        let theirs = base;
        let m = merge_file(base, ours, theirs);
        assert!(!m.conflict);
        assert_eq!(rendered(&m), ours);
    }

    #[test]
    fn duplicate_anchor_collapses_without_conflict() {
        // A duplicated anchor is corrupt input.
        // Presence rules apply with no content merge; it collapses to the
        // first occurrence and never conflicts.
        let doc = "%YAML 1.1\n--- !u!1 &100\nGameObject:\n  m_Name: A\n--- !u!1 &100\nGameObject:\n  m_Name: B\n";
        let m = merge_file(doc, doc, doc);
        assert!(!m.conflict);
        let text = rendered(&m);
        assert_eq!(text.matches("&100").count(), 1);
        assert!(text.contains("m_Name: A"));
    }
}
