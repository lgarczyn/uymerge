//! Post-merge self-check: the reference validate_merge as a checker over the
//! final output.
//! A failure here is a bug in merge, not a conflict.
//! SPEC sections 4 and 5.1.
//! Packet P8.
//!
//! Ported function by function from the reference validate_merge and its
//! helpers _value_rule, _set_rule and _presence_rule.
//! The reference builds keyed views with table_entries, refid_records and
//! DOC_ANCHOR; here they come from the P4 model parsers, projected to the
//! same tuples: entries to (text, rids, loc_count), records to
//! (payload, ids), documents to the anchor set.
//! Line spans tracked for reassembly are dropped, since the checker only
//! compares content.
//!
//! Inputs are CRLF-normalized before parsing, mirroring the reference's
//! leading `x.replace("\r\n", "\n")`.
//! Inherited duplicate keys are checked for presence only, never content.

use std::collections::{BTreeMap, BTreeSet};

use crate::model;

// The reference tuple for an m_TableData entry: joined m_Localized text, the
// m_Metadata rid set, and the m_Localized field count.
// Equality matches the reference `side[key] == b[key]` tuple compare.
#[derive(Clone, PartialEq, Eq)]
struct Entry {
    text: String,
    rids: BTreeSet<String>,
    n: usize,
}

// The reference tuple for a references/RefIds record: payload minus id items
// and the `- id:` value set.
#[derive(Clone, PartialEq, Eq)]
struct Record {
    payload: String,
    ids: BTreeSet<String>,
}

// --- projected model views -----------------------------------------------

fn entries_of(text: &str) -> (BTreeMap<String, Entry>, BTreeSet<String>) {
    let td = model::table_entries(text);
    let map = td
        .entries
        .into_iter()
        .map(|(k, e)| {
            (
                k,
                Entry {
                    text: e.localized,
                    rids: e.rids,
                    n: e.loc_count,
                },
            )
        })
        .collect();
    (map, td.dups)
}

fn records_of(text: &str) -> (BTreeMap<String, Record>, BTreeSet<String>) {
    let rd = model::refid_records(text);
    let map = rd
        .records
        .into_iter()
        .map(|(k, r)| {
            (
                k,
                Record {
                    payload: r.payload,
                    ids: r.ids,
                },
            )
        })
        .collect();
    (map, rd.dups)
}

fn anchors_of(text: &str) -> BTreeSet<String> {
    model::documents(text).docs.into_keys().collect()
}

// --- rules, ported from _value_rule / _set_rule / _presence_rule ----------

// Scalar 3-way: a silent side-pick or invented value is as much a loss as
// a drop.
// `b` is None when the key is absent from base.
fn value_rule(what: &str, b: Option<&str>, o: &str, t: &str, m: &str, viols: &mut Vec<String>) {
    if o == t {
        if m != o {
            viols.push(format!("{what} matches neither side"));
        }
    } else if b == Some(o) {
        if m != t {
            viols.push(format!("{what} lost theirs' change"));
        }
    } else if b == Some(t) {
        if m != o {
            viols.push(format!("{what} lost ours' change"));
        }
    } else {
        viols.push(format!("{what} changed differently on both sides"));
    }
}

// Id sets merge as sets: both sides' additions and removals must all be
// honored.
// `b` None means the empty set.
fn set_rule(
    what: &str,
    b: Option<&BTreeSet<String>>,
    o: &BTreeSet<String>,
    t: &BTreeSet<String>,
    m: &BTreeSet<String>,
    viols: &mut Vec<String>,
) {
    let empty = BTreeSet::new();
    let b = b.unwrap_or(&empty);
    // An add/remove contradiction cannot exist with a shared base, so the
    // formula is total: (b & o & t) | (o - b) | (t - b)
    let mut expected: BTreeSet<String> = b
        .iter()
        .filter(|x| o.contains(*x) && t.contains(*x))
        .cloned()
        .collect();
    expected.extend(o.difference(b).cloned());
    expected.extend(t.difference(b).cloned());
    if *m != expected {
        viols.push(format!("{what} does not match the 3-way id set"));
    }
}

// Presence per SPEC 4.1.
// `verify_both` runs when the key is on both sides and survived.
// `verify_copy` runs when it was added on one side and survived, with that
// side's value.
// Both receive `viols` to append to.
#[allow(clippy::too_many_arguments)]
fn presence_rule<V: PartialEq>(
    what: &str,
    key: &str,
    b: &BTreeMap<String, V>,
    o: &BTreeMap<String, V>,
    t: &BTreeMap<String, V>,
    m: &BTreeMap<String, V>,
    viols: &mut Vec<String>,
    mut verify_both: impl FnMut(&str, &mut Vec<String>),
    mut verify_copy: impl FnMut(&str, &V, &mut Vec<String>),
) {
    let in_o = o.contains_key(key);
    let in_t = t.contains_key(key);
    let in_m = m.contains_key(key);
    if in_o && in_t {
        if !in_m {
            viols.push(format!("{what} was dropped (present on both sides)"));
        } else {
            verify_both(key, viols);
        }
    } else if in_o || in_t {
        let (side, other) = if in_o { (o, "theirs") } else { (t, "ours") };
        match (side.get(key), b.get(key)) {
            (Some(sv), None) => {
                if !in_m {
                    viols.push(format!("{what} added on one side was dropped"));
                } else {
                    verify_copy(key, sv, viols);
                }
            }
            (Some(sv), Some(bv)) => {
                if sv == bv {
                    if in_m {
                        viols.push(format!("{what} deleted on {other} was resurrected"));
                    }
                } else {
                    viols.push(format!("{what} edited on one side but deleted on {other}"));
                }
            }
            _ => {}
        }
    } else if in_m && b.contains_key(key) {
        viols.push(format!("{what} deleted on both sides was resurrected"));
    }
}

/// Every violation of faithful 3-way merge semantics in `merged` against
/// `base`, `ours` and `theirs`.
/// An empty result means verified.
/// Argument order matches the reference validate_merge.
pub fn validate_merge(base: &str, ours: &str, theirs: &str, merged: &str) -> Vec<String> {
    let base = base.replace("\r\n", "\n");
    let ours = ours.replace("\r\n", "\n");
    let theirs = theirs.replace("\r\n", "\n");
    let merged = merged.replace("\r\n", "\n");
    let mut viols: Vec<String> = Vec::new();

    // Documents, keyed by &anchor.
    // Only drops are checked here; a document body is verified structurally
    // below through its records, not line by line.
    let ab = anchors_of(&base);
    let ao = anchors_of(&ours);
    let at = anchors_of(&theirs);
    let am = anchors_of(&merged);
    for a in ao.intersection(&at) {
        if !am.contains(a) {
            viols.push(format!("document &{a} was dropped (present on both sides)"));
        }
    }
    // (ao | at) - (ao & at) - ab - am: on exactly one side, new, and gone.
    for a in ao.symmetric_difference(&at) {
        if !ab.contains(a) && !am.contains(a) {
            viols.push(format!("document &{a} added on one side was dropped"));
        }
    }

    // m_TableData entries, keyed by m_Id.
    let (eb, edb) = entries_of(&base);
    let (eo, edo) = entries_of(&ours);
    let (et, edt) = entries_of(&theirs);
    let (em, edups) = entries_of(&merged);
    // Keys already duplicated in an input carry inherited corruption.
    // Their parsed content is first-occurrence-only and not comparable, so
    // only presence is checked for them.
    let eskip: BTreeSet<String> = edb
        .iter()
        .chain(edo.iter())
        .chain(edt.iter())
        .cloned()
        .collect();
    for d in &edups {
        if !edo.contains(d) && !edt.contains(d) {
            viols.push(format!("entry {d} is duplicated in the merge"));
        }
    }

    let mut entry_both = |k: &str, viols: &mut Vec<String>| {
        if eskip.contains(k) {
            return;
        }
        let Some(m) = em.get(k) else { return };
        // shared-table entries carry no m_Localized; only a surplus, the
        // stacked field a line merge can produce, is corruption
        if m.n > 1 {
            viols.push(format!("entry {k} has {} m_Localized fields", m.n));
        }
        let (Some(o), Some(t)) = (eo.get(k), et.get(k)) else {
            return;
        };
        value_rule(
            &format!("entry {k} text"),
            eb.get(k).map(|e| e.text.as_str()),
            &o.text,
            &t.text,
            &m.text,
            viols,
        );
        set_rule(
            &format!("entry {k} metadata"),
            eb.get(k).map(|e| &e.rids),
            &o.rids,
            &t.rids,
            &m.rids,
            viols,
        );
    };
    let mut entry_copy = |k: &str, side: &Entry, viols: &mut Vec<String>| {
        if eskip.contains(k) {
            return;
        }
        let Some(m) = em.get(k) else { return };
        if m.text != side.text || m.rids != side.rids {
            viols.push(format!("entry {k} was altered while being added"));
        }
    };

    let mut ekeys: BTreeSet<&String> = BTreeSet::new();
    ekeys.extend(eb.keys());
    ekeys.extend(eo.keys());
    ekeys.extend(et.keys());
    for k in ekeys {
        presence_rule(
            &format!("entry {k}"),
            k,
            &eb,
            &eo,
            &et,
            &em,
            &mut viols,
            &mut entry_both,
            &mut entry_copy,
        );
    }

    // references/RefIds records, keyed by rid.
    let (rb, rdb) = records_of(&base);
    let (ro, rdo) = records_of(&ours);
    let (rt, rdt) = records_of(&theirs);
    let (rm, rdups) = records_of(&merged);
    let rskip: BTreeSet<String> = rdb
        .iter()
        .chain(rdo.iter())
        .chain(rdt.iter())
        .cloned()
        .collect();
    for d in &rdups {
        if !rdo.contains(d) && !rdt.contains(d) {
            viols.push(format!(
                "reference record {d} is duplicated with differing content"
            ));
        }
    }

    let mut rec_both = |k: &str, viols: &mut Vec<String>| {
        if rskip.contains(k) {
            return;
        }
        let (Some(o), Some(t), Some(m)) = (ro.get(k), rt.get(k), rm.get(k)) else {
            return;
        };
        value_rule(
            &format!("reference {k} payload"),
            rb.get(k).map(|r| r.payload.as_str()),
            &o.payload,
            &t.payload,
            &m.payload,
            viols,
        );
        set_rule(
            &format!("reference {k} entry ids"),
            rb.get(k).map(|r| &r.ids),
            &o.ids,
            &t.ids,
            &m.ids,
            viols,
        );
    };
    let mut rec_copy = |k: &str, side: &Record, viols: &mut Vec<String>| {
        if rskip.contains(k) {
            return;
        }
        let Some(m) = rm.get(k) else { return };
        if m != side {
            viols.push(format!(
                "reference record {k} was altered while being added"
            ));
        }
    };

    let mut rkeys: BTreeSet<&String> = BTreeSet::new();
    rkeys.extend(rb.keys());
    rkeys.extend(ro.keys());
    rkeys.extend(rt.keys());
    for k in rkeys {
        presence_rule(
            &format!("reference record {k}"),
            k,
            &rb,
            &ro,
            &rt,
            &rm,
            &mut viols,
            &mut rec_both,
            &mut rec_copy,
        );
    }

    // Every rid an entry claims in its metadata must resolve to a record.
    // Negative rids are the null sentinel and are exempt.
    for (k, e) in &em {
        for r in &e.rids {
            if !r.starts_with('-') && !rm.contains_key(r) {
                viols.push(format!("entry {k} references rid {r} which has no record"));
            }
        }
    }

    viols
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape builders mirror tests/conftest.py so ported scenarios read like
    // the reference suite.
    const HDR: &str = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n\
                       --- !u!114 &11400000\nMonoBehaviour:\n  m_Name: Table_en\n";
    const RID: &str = "842043826615615503";

    fn entry(eid: &str, text: &str, rids: &[&str]) -> String {
        let md = if rids.is_empty() {
            "      m_Items: []".to_string()
        } else {
            let mut s = "      m_Items:".to_string();
            for r in rids {
                s.push_str(&format!("\n      - rid: {r}"));
            }
            s
        };
        format!("  - m_Id: {eid}\n    m_Localized: {text}\n    m_Metadata:\n{md}")
    }

    fn refrec(rid: &str, ids: &[&str]) -> String {
        let mut s = format!(
            "    - rid: {rid}\n      type: {{class: SmartFormatTag, \
             ns: UnityEngine.Localization.Metadata, asm: Unity.Localization}}\n\
             \x20     data:\n        m_Entries: \n        m_SharedEntries:\n"
        );
        let id_lines: Vec<String> = ids.iter().map(|i| format!("        - id: {i}")).collect();
        s.push_str(&id_lines.join("\n"));
        s
    }

    fn asset(entries: &[String], refs: &[String]) -> String {
        let mut text = format!("{HDR}  m_TableData:");
        for e in entries {
            text.push('\n');
            text.push_str(e);
        }
        text.push_str("\n  references:\n    version: 2");
        if !refs.is_empty() {
            text.push_str("\n    RefIds:");
            for r in refs {
                text.push('\n');
                text.push_str(r);
            }
        }
        text.push('\n');
        text
    }

    fn table(pairs: &[(&str, &str)]) -> String {
        let entries: Vec<String> = pairs.iter().map(|(e, t)| entry(e, t, &[])).collect();
        asset(&entries, &[])
    }

    fn viols(base: &str, ours: &str, theirs: &str, merged: &str) -> Vec<String> {
        validate_merge(base, ours, theirs, merged)
    }

    fn any(v: &[String], needle: &str) -> bool {
        v.iter().any(|s| s.contains(needle))
    }

    // --- ported from TestParsers.test_crlf_normalized_by_validator ---------

    #[test]
    fn crlf_normalized_by_validator() {
        let doc = table(&[("100", "a")]);
        assert_eq!(
            viols(&doc, &doc, &doc.replace('\n', "\r\n"), &doc),
            Vec::<String>::new()
        );
    }

    // --- ported from TestDedup --------------------------------------------

    #[test]
    fn differing_duplicate_introduced_by_merge_is_flagged() {
        let clean = asset(&[entry("100", "a", &[RID])], &[refrec(RID, &["100"])]);
        let merged = asset(
            &[entry("100", "a", &[RID])],
            &[refrec(RID, &["100"]), refrec(RID, &["200"])],
        );
        assert!(any(
            &viols(&clean, &clean, &clean, &merged),
            "duplicated with differing content"
        ));
    }

    #[test]
    fn inherited_duplicate_is_tolerated() {
        // corruption already in history must not conflict every future merge;
        // only presence is checked for keys the inputs themselves duplicate
        let doc = asset(
            &[entry("100", "a", &[RID])],
            &[refrec(RID, &["100"]), refrec(RID, &["200"])],
        );
        assert_eq!(viols(&doc, &doc, &doc, &doc), Vec::<String>::new());
    }

    // --- ported from TestEntryRules ---------------------------------------

    fn entry_base() -> String {
        asset(
            &[entry("100", "victim", &[RID]), entry("200", "old", &[])],
            &[refrec(RID, &["100"])],
        )
    }

    #[test]
    fn faithful_take_theirs_verifies() {
        let base = entry_base();
        let theirs = base.replace("m_Localized: old", "m_Localized: new");
        assert_eq!(viols(&base, &base, &theirs, &theirs), Vec::<String>::new());
    }

    #[test]
    fn entry_dropped_from_both_sides() {
        let base = entry_base();
        let merged = asset(&[entry("200", "old", &[])], &[refrec(RID, &["100"])]);
        assert!(any(
            &viols(&base, &base, &base, &merged),
            "entry 100 was dropped"
        ));
    }

    #[test]
    fn one_sided_delete_is_faithful() {
        let base = entry_base();
        let theirs = asset(&[entry("200", "old", &[])], &[refrec(RID, &["100"])]);
        assert_eq!(viols(&base, &base, &theirs, &theirs), Vec::<String>::new());
    }

    #[test]
    fn deletion_not_applied_is_resurrection() {
        let base = entry_base();
        let theirs = asset(&[entry("200", "old", &[])], &[refrec(RID, &["100"])]);
        assert!(any(
            &viols(&base, &base, &theirs, &base),
            "deleted on theirs was resurrected"
        ));
    }

    #[test]
    fn edit_vs_delete_conflicts() {
        let base = entry_base();
        let ours = base.replace("m_Localized: victim", "m_Localized: edited");
        let theirs = asset(&[entry("200", "old", &[])], &[refrec(RID, &["100"])]);
        assert!(any(
            &viols(&base, &ours, &theirs, &theirs),
            "edited on one side but deleted"
        ));
    }

    #[test]
    fn silent_side_pick_on_both_changed() {
        let base = entry_base();
        let ours = base.replace("m_Localized: old", "m_Localized: OURS");
        let theirs = base.replace("m_Localized: old", "m_Localized: THEIRS");
        assert!(any(
            &viols(&base, &ours, &theirs, &ours),
            "changed differently on both sides"
        ));
    }

    #[test]
    fn revert_to_base_on_both_changed() {
        let base = entry_base();
        let ours = base.replace("m_Localized: old", "m_Localized: OURS");
        let theirs = base.replace("m_Localized: old", "m_Localized: THEIRS");
        assert_ne!(viols(&base, &ours, &theirs, &base), Vec::<String>::new());
    }

    #[test]
    fn value_swap_matches_neither_side() {
        let base = entry_base();
        let merged = base.replace("m_Localized: victim", "m_Localized: old");
        assert!(any(
            &viols(&base, &base, &base, &merged),
            "entry 100 text matches neither side"
        ));
    }

    #[test]
    fn smart_flag_stripped_from_metadata() {
        // the July symptom: entry text survives but its smart tag is gone
        let base = entry_base();
        let merged = asset(
            &[entry("100", "victim", &[]), entry("200", "old", &[])],
            &[refrec(RID, &["100"])],
        );
        assert!(any(
            &viols(&base, &base, &base, &merged),
            "entry 100 metadata does not match"
        ));
    }

    #[test]
    fn stacked_localized_detected() {
        let base = entry_base();
        let merged = base.replace(
            "    m_Localized: old",
            "    m_Localized: old\n    m_Localized: dup",
        );
        assert!(any(
            &viols(&base, &base, &base, &merged),
            "m_Localized fields"
        ));
    }

    #[test]
    fn added_entry_must_survive_unaltered() {
        let base = entry_base();
        let ours = asset(
            &[
                entry("100", "victim", &[RID]),
                entry("150", "added", &[]),
                entry("200", "old", &[]),
            ],
            &[refrec(RID, &["100"])],
        );
        assert!(any(
            &viols(&base, &ours, &base, &base),
            "entry 150 added on one side was dropped"
        ));
        let altered = ours.replace("m_Localized: added", "m_Localized: mangled");
        assert!(any(
            &viols(&base, &ours, &base, &altered),
            "entry 150 was altered while being added"
        ));
    }

    #[test]
    fn metadata_set_merges_both_sides_additions() {
        // ours adds rid 111, theirs adds rid 222 to the same entry: the merge
        // must keep both
        let base = entry_base();
        let ours = base.replace(
            "      m_Items:\n      - rid: 842043826615615503",
            "      m_Items:\n      - rid: 842043826615615503\n      - rid: 111",
        );
        let theirs = base.replace(
            "      m_Items:\n      - rid: 842043826615615503",
            "      m_Items:\n      - rid: 842043826615615503\n      - rid: 222",
        );
        let good = base.replace(
            "      m_Items:\n      - rid: 842043826615615503",
            "      m_Items:\n      - rid: 842043826615615503\n      - rid: 111\n      - rid: 222",
        );
        assert!(!any(&viols(&base, &ours, &theirs, &good), "metadata"));
        assert!(any(
            &viols(&base, &ours, &theirs, &ours),
            "entry 100 metadata does not match"
        ));
    }

    // --- ported from TestReferenceRules -----------------------------------

    fn ref_base() -> String {
        asset(
            &[
                entry("100", "'{smart}'", &[RID]),
                entry("200", "plain", &[]),
            ],
            &[refrec(RID, &["100"])],
        )
    }

    #[test]
    fn record_dropped() {
        let base = ref_base();
        let merged = asset(
            &[
                entry("100", "'{smart}'", &[RID]),
                entry("200", "plain", &[]),
            ],
            &[],
        );
        let got = viols(&base, &base, &base, &merged);
        assert!(any(&got, &format!("reference record {RID} was dropped")));
        assert!(any(
            &got,
            &format!("references rid {RID} which has no record")
        ));
    }

    #[test]
    fn shared_id_dropped_from_record() {
        let base = asset(
            &[entry("100", "'{a}'", &[RID]), entry("200", "'{b}'", &[RID])],
            &[refrec(RID, &["100", "200"])],
        );
        let merged = asset(
            &[entry("100", "'{a}'", &[RID]), entry("200", "'{b}'", &[RID])],
            &[refrec(RID, &["200"])],
        );
        assert!(any(
            &viols(&base, &base, &base, &merged),
            &format!("reference {RID} entry ids does not match")
        ));
    }

    #[test]
    fn shared_id_additions_merge_as_set() {
        let base = ref_base();
        let ours = base.replace("        - id: 100", "        - id: 100\n        - id: 111");
        let theirs = base.replace("        - id: 100", "        - id: 100\n        - id: 222");
        let good = base.replace(
            "        - id: 100",
            "        - id: 100\n        - id: 111\n        - id: 222",
        );
        assert!(!any(&viols(&base, &ours, &theirs, &good), "entry ids"));
    }

    #[test]
    fn payload_change_verified() {
        let base = ref_base();
        let theirs = base.replace("        m_Entries: ", "        m_Entries: changed");
        assert_eq!(viols(&base, &base, &theirs, &theirs), Vec::<String>::new());
        assert!(any(
            &viols(&base, &base, &theirs, &base),
            &format!("reference {RID} payload lost theirs' change")
        ));
    }

    // --- ported from TestDocumentRules ------------------------------------

    const PREFAB: &str = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n\
                          --- !u!1 &100\nGameObject:\n  m_Name: Root\n\
                          --- !u!114 &200\nMonoBehaviour:\n  m_Enabled: 1\n";

    #[test]
    fn document_dropped() {
        let merged = PREFAB.replace("--- !u!114 &200\nMonoBehaviour:\n  m_Enabled: 1\n", "");
        assert!(any(
            &viols(PREFAB, PREFAB, PREFAB, &merged),
            "document &200 was dropped"
        ));
    }

    #[test]
    fn document_added_on_one_side_dropped() {
        let ours = format!("{PREFAB}--- !u!114 &300\nMonoBehaviour:\n  m_New: 1\n");
        assert!(any(
            &viols(PREFAB, &ours, PREFAB, PREFAB),
            "document &300 added on one side was dropped"
        ));
        assert_eq!(viols(PREFAB, &ours, PREFAB, &ours), Vec::<String>::new());
    }

    // --- red-team shapes from oracle/redteam.py, as verifier checks --------
    // Each scenario feeds a fabricated native-tool corruption to the verifier.
    // All must be caught: validate_merge returns a non-empty list.
    // Argument order is validate_merge(base, ours, theirs, merged); the driver
    // maps remote to theirs and local to ours.

    fn rt_base() -> String {
        asset(
            &[
                entry("100", "plain string", &[]),
                entry("200", "'{smart} string'", &[RID]),
                entry("300", "third", &[]),
            ],
            &[refrec(RID, &["200", "300"])],
        )
    }

    fn rt_theirs() -> String {
        rt_base().replace("m_Localized: third", "m_Localized: third rewritten")
    }

    const RT_PREFAB: &str = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n\
                             --- !u!1 &100\nGameObject:\n  m_Name: Root\n\
                             --- !u!114 &200\nMonoBehaviour:\n  m_Script: {fileID: 11500000}\n\
                             --- !u!114 &300\nMonoBehaviour:\n  m_Enabled: 1\n";

    fn caught(v: Vec<String>) -> bool {
        !v.is_empty()
    }

    #[test]
    fn redteam_s1_drop_whole_entry() {
        let base = rt_base();
        let theirs = rt_theirs();
        // merged is theirs with the m_Id 100 entry removed
        let merged = theirs.replace(&format!("\n{}", entry("100", "plain string", &[])), "");
        assert!(caught(viols(&base, &base, &theirs, &merged)));
    }

    #[test]
    fn redteam_s2_drop_refids_record() {
        let base = rt_base();
        let theirs = rt_theirs();
        // merged is theirs with the RefIds record gone
        let merged = asset(
            &[
                entry("100", "plain string", &[]),
                entry("200", "'{smart} string'", &[RID]),
                entry("300", "third rewritten", &[]),
            ],
            &[],
        );
        assert!(caught(viols(&base, &base, &theirs, &merged)));
    }

    #[test]
    fn redteam_s3_strip_smart_rid_from_metadata() {
        let base = rt_base();
        let theirs = rt_theirs();
        let merged = theirs.replace(
            "    m_Metadata:\n      m_Items:\n      - rid: 842043826615615503",
            "    m_Metadata:\n      m_Items: []",
        );
        assert!(caught(viols(&base, &base, &theirs, &merged)));
    }

    #[test]
    fn redteam_s4_drop_id_from_shared_entries() {
        let base = rt_base();
        let theirs = rt_theirs();
        let merged = theirs.replace("        - id: 200\n", "");
        assert!(caught(viols(&base, &base, &theirs, &merged)));
    }

    #[test]
    fn redteam_s5_duplicate_the_refids_record() {
        let base = rt_base();
        let theirs = rt_theirs();
        let rec = format!("\n{}", refrec(RID, &["200", "300"]));
        let merged = theirs.replace(&rec, &format!("{rec}{rec}"));
        assert!(caught(viols(&base, &base, &theirs, &merged)));
    }

    #[test]
    fn redteam_s6_cross_entry_value_swap() {
        let base = rt_base();
        let theirs = rt_theirs();
        let merged = theirs.replace("m_Localized: plain string", "m_Localized: third rewritten");
        assert!(caught(viols(&base, &base, &theirs, &merged)));
    }

    #[test]
    fn redteam_s7_silent_side_pick_on_both_changed() {
        let base = rt_base();
        let theirs = base.replace("m_Localized: third", "m_Localized: THEIRS-EDIT");
        let ours = base.replace("m_Localized: third", "m_Localized: OURS-EDIT");
        // the tool copies local over the result
        let merged = ours.clone();
        assert!(caught(viols(&base, &ours, &theirs, &merged)));
    }

    #[test]
    fn redteam_s8_drop_whole_document_from_prefab() {
        let base = RT_PREFAB.to_string();
        let theirs = RT_PREFAB.replace("m_Name: Root", "m_Name: RootRenamed");
        let merged = theirs.replace("--- !u!114 &300\nMonoBehaviour:\n  m_Enabled: 1\n", "");
        assert!(caught(viols(&base, &base, &theirs, &merged)));
    }

    #[test]
    fn redteam_s9_revert_both_changed_entry_to_base() {
        let base = rt_base();
        let theirs = base.replace("m_Localized: third", "m_Localized: THEIRS-EDIT");
        let ours = base.replace("m_Localized: third", "m_Localized: OURS-EDIT");
        // the tool reverts theirs' edit back to base
        let merged = theirs.replace("m_Localized: THEIRS-EDIT", "m_Localized: third");
        assert!(caught(viols(&base, &ours, &theirs, &merged)));
    }

    #[test]
    fn redteam_s10_both_sides_add_same_record_tool_stacks() {
        let both_add_base = table(&[("100", "plain string"), ("300", "third")]);
        let side = asset(
            &[
                entry("100", "plain string", &[]),
                entry("200", "'{smart}'", &[RID]),
                entry("300", "third", &[]),
            ],
            &[refrec(RID, &["200"])],
        );
        // the tool stacks the record added identically on both sides
        let rec = format!("\n{}", refrec(RID, &["200"]));
        let merged = side.replace(&rec, &format!("{rec}{rec}"));
        assert!(caught(viols(&both_add_base, &side, &side, &merged)));
    }
}
