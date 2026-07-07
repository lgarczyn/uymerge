//! Editor-faithful unwrap/rewrap of raw Unity YAML text.
//! SPEC section 2.
//! Packets P1 (terminators, plain scalars), P2 (quoted scalars, flow
//! cleanup), P3 (reserialize dispatch, byte parity).
//! Reference functions: split_lines, reemit_plain, join_plain_value,
//! gather_continuations, gather_quoted, reemit_quoted, decode_quoted,
//! reemit_double, decode_double, reserialize.
//!
//! Works on raw text lines, never a decoded YAML value: Unity's pre-fold
//! whitespace is significant, so the oracle is byte equality with the
//! editor's own serializer.
//! Columns count Unicode code points (`char`s), not UTF-16 units or bytes.

/// The "unwrap" width: wide enough that a scalar never folds.
/// Matches the reference `INF = 10 ** 9`.
pub const INF: usize = 1_000_000_000;

/// Reference `KEY = ^(\s*(?:- )*)([\w.\-/]+):\s(.+)$` as a matcher.
/// The leading group absorbs sequence dashes so `- k: v` and nested
/// `- - k: v` still fold.
/// The value keeps its trailing spaces.
/// Returns (indent, key, value) on a match.
pub fn key_match(line: &str) -> Option<(String, String, String)> {
    let ch: Vec<char> = line.chars().collect();
    let n = ch.len();

    // \s* leading whitespace.
    let mut p = 0;
    while p < n && ch[p].is_whitespace() {
        p += 1;
    }

    // (?:- )* greedy.
    // Record the end position after each "- " copy.
    let mut dash_ends = Vec::new();
    let mut q = p;
    while q + 1 < n && ch[q] == '-' && ch[q + 1] == ' ' {
        q += 2;
        dash_ends.push(q);
    }

    // The regex tries the most "- " copies first, then backtracks toward
    // zero.
    // \s* can never give ground: a space fits neither the dash run nor a
    // key char, so only the "- " count varies.
    let candidates = dash_ends.iter().rev().copied().chain(std::iter::once(p));
    for g1_end in candidates {
        // [\w.\-/]+ greedy.
        // The char after the run must be ':', so a shorter key never helps:
        // nothing shorter ends on a ':'.
        let mut k = g1_end;
        while k < n && is_key_char(ch[k]) {
            k += 1;
        }
        if k == g1_end || k >= n || ch[k] != ':' {
            continue;
        }
        let colon = k;
        // :\s one whitespace char, then (.+)$ at least one more char.
        if colon + 2 >= n || !ch[colon + 1].is_whitespace() {
            continue;
        }
        let indent: String = ch[..g1_end].iter().collect();
        let key: String = ch[g1_end..colon].iter().collect();
        let value: String = ch[colon + 2..].iter().collect();
        return Some((indent, key, value));
    }
    None
}

// A KEY key char: `\w` (Unicode word) plus `.`, `-`, `/`.
// Unity keys are ASCII identifiers, so is_alphanumeric stands in for `\w`.
fn is_key_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.' || c == '-' || c == '/'
}

/// Reference `split_lines`: split on `\n`, recording a trailing `\r` per
/// line so mixed LF/CRLF assets round-trip.
/// Returns (content, had_cr).
pub fn split_lines(text: &str) -> Vec<(String, bool)> {
    text.split('\n')
        .map(|p| match p.strip_suffix('\r') {
            Some(rest) => (rest.to_string(), true),
            None => (p.to_string(), false),
        })
        .collect()
}

/// Reference `reemit_plain`: fold a plain scalar at the last space of a run
/// once the column passes `width`.
/// A fold never splits a space run; earlier spaces become trailing
/// whitespace.
/// The first line carries `prefix`, later lines `cont_indent`.
pub fn reemit_plain(value: &str, prefix: &str, cont_indent: &str, width: usize) -> Vec<String> {
    let v: Vec<char> = value.chars().collect();
    let n = v.len();
    let mut out = Vec::new();
    let mut cur = String::from(prefix);
    let mut col = prefix.chars().count();
    for i in 0..n {
        let c = v[i];
        let next_is_space = i + 1 < n && v[i + 1] == ' ';
        if c == ' ' && col > width && !next_is_space {
            out.push(std::mem::replace(&mut cur, cont_indent.to_string()));
            col = cont_indent.chars().count();
        } else {
            cur.push(c);
            col += 1;
        }
    }
    out.push(cur);
    out
}

/// Reference `join_plain_value`: inverse of `reemit_plain`.
/// Each continuation restores one fold space plus its content past
/// `cont_indent`.
pub fn join_plain_value(val: &str, conts: &[String], cont_indent: &str) -> String {
    let ci_len = cont_indent.chars().count();
    let mut s = String::from(val);
    for c in conts {
        s.push(' ');
        s.extend(c.chars().skip(ci_len));
    }
    s
}

/// Reference `gather_continuations`: from line `i`, take strictly
/// more-indented non-blank lines that are not KEY mappings.
/// Returns the continuation lines and the index past them.
pub fn gather_continuations(lines: &[String], i: usize, key_indent: usize) -> (Vec<String>, usize) {
    let mut conts = Vec::new();
    let mut j = i + 1;
    while j < lines.len() {
        let c = &lines[j];
        let ci = c.chars().take_while(|&ch| ch == ' ').count();
        if c.trim().is_empty() || ci <= key_indent || key_match(c).is_some() {
            break;
        }
        conts.push(c.clone());
        j += 1;
    }
    (conts, j)
}

/// Reference `gather_quoted`: span a quoted block from its opening quote at
/// code-point column `quote_col` to the matching close.
/// The delimiter is the quote, not indent: `''` and `\"`/`\\` escape, and
/// the value may hold blank lines and `key:`-looking prose.
/// A missing close spans to end of file.
/// Returns the block lines and the index past them.
pub fn gather_quoted(
    lines: &[String],
    i: usize,
    quote_col: usize,
    quote: char,
) -> (Vec<String>, usize) {
    // Scan the conceptual lines[i..].join("\n") for the close, walking the
    // slice directly instead of materializing the whole tail per scalar.
    // `li` is the current line, `col` the char index in it; col == line
    // length sits on the '\n' before line li+1.
    // nl newlines to reach the break line is exactly li - i, matching the
    // reference's count of '\n' before the close.
    let last = lines.len().saturating_sub(1);
    let mut li = i;
    let mut cur: Vec<char> = lines[i].chars().collect();
    let mut col = quote_col + 1;
    loop {
        let c = if col < cur.len() {
            cur[col]
        } else if li < last {
            '\n'
        } else {
            break;
        };
        if quote == '\'' {
            if c == '\'' {
                // A doubled quote escapes; both halves sit on one line,
                // since a '\n' separator would otherwise fall between them.
                if col + 1 < cur.len() && cur[col + 1] == '\'' {
                    col += 2;
                    continue;
                }
                break;
            }
        } else if c == '\\' {
            // Skip the backslash and the next char, which may be a
            // fold-escaped '\n'.
            advance(&mut li, &mut cur, &mut col, lines, last);
            advance(&mut li, &mut cur, &mut col, lines, last);
            continue;
        } else if c == '"' {
            break;
        }
        advance(&mut li, &mut cur, &mut col, lines, last);
    }
    let nl = li - i;
    let j = (i + nl + 1).min(lines.len());
    (lines[i..j].to_vec(), j)
}

// Advance the gather_quoted cursor one char through lines[..].join("\n").
// Stepping off a non-final line's last char lands on the '\n' separator;
// the next step crosses into the following line.
// Past the final line, col runs beyond the length so the caller stops.
fn advance(li: &mut usize, cur: &mut Vec<char>, col: &mut usize, lines: &[String], last: usize) {
    if *col < cur.len() {
        *col += 1;
    } else if *li < last {
        *li += 1;
        *cur = lines[*li].chars().collect();
        *col = 0;
    } else {
        *col += 1;
    }
}

// Slice a line up to its last `q`, dropping the quote and anything after.
// Bug-compatibility: a missing quote makes the reference's rfind return
// -1, so `line[:-1]` drops the last char.
// Preserve that exactly.
fn cut_at_last_quote(line: &str, q: char) -> String {
    let ch: Vec<char> = line.chars().collect();
    match ch.iter().rposition(|&c| c == q) {
        Some(idx) => ch[..idx].iter().collect(),
        None if ch.is_empty() => String::new(),
        None => ch[..ch.len() - 1].iter().collect(),
    }
}

/// Reference `decode_quoted`: inverse of `reemit_quoted`.
/// A soft fold restores one space, a blank line one newline, `''` to `'`.
/// `cont_indent` is a code-point count, `prefix` the opening run through the
/// quote.
pub fn decode_quoted(block: &[String], prefix: &str, cont_indent: usize) -> String {
    if block.is_empty() {
        return String::new();
    }
    let last = &block[block.len() - 1];
    let body: Vec<String> = if last.trim() == "'" {
        block[..block.len() - 1].to_vec()
    } else {
        let mut b = block[..block.len() - 1].to_vec();
        b.push(cut_at_last_quote(last, '\''));
        b
    };
    if body.is_empty() {
        return String::new();
    }
    let plen = prefix.chars().count();
    let mut phys: Vec<String> = Vec::with_capacity(body.len());
    phys.push(body[0].chars().skip(plen).collect());
    for l in &body[1..] {
        if l.trim().is_empty() {
            phys.push(String::new());
        } else {
            phys.push(l.chars().skip(cont_indent).collect());
        }
    }
    let mut content = phys[0].clone();
    for k in 1..phys.len() {
        if phys[k].is_empty() {
            content.push('\n');
        } else if phys[k - 1].is_empty() {
            content.push_str(&phys[k]);
        } else {
            content.push(' ');
            content.push_str(&phys[k]);
        }
    }
    content.replace("''", "'")
}

/// Reference `reemit_quoted`: single-quoted re-emit.
/// `'` becomes `''`, a content newline becomes a blank line, and the column
/// accumulates across newlines.
/// A fold never splits a space run.
pub fn reemit_quoted(content: &str, prefix: &str, cont_indent: usize, width: usize) -> Vec<String> {
    if content.is_empty() {
        return vec![format!("{}'", prefix)];
    }
    let escaped = content.replace('\'', "''");
    let segments: Vec<&str> = escaped.split('\n').collect();
    let ind = " ".repeat(cont_indent);
    let mut out: Vec<String> = Vec::new();
    let mut vcol = prefix.chars().count();
    let mut prev_empty = false;
    for (s, seg) in segments.iter().enumerate() {
        if s > 0 {
            out.push(String::new());
            vcol += if prev_empty { 1 } else { cont_indent + 2 };
        }
        if seg.is_empty() {
            if s == 0 {
                out.push(prefix.to_string());
            }
            prev_empty = s != 0;
            continue;
        }
        prev_empty = false;
        let mut cur = if s == 0 {
            prefix.to_string()
        } else {
            ind.clone()
        };
        let mut firstw = true;
        for word in seg.split(' ') {
            let wlen = word.chars().count();
            if firstw {
                cur.push_str(word);
                vcol += wlen;
                firstw = false;
            } else if !word.is_empty() && vcol + 1 > width {
                out.push(std::mem::replace(&mut cur, format!("{}{}", ind, word)));
                vcol = cont_indent + wlen;
            } else {
                cur.push(' ');
                cur.push_str(word);
                vcol += 1 + wlen;
            }
        }
        out.push(cur);
    }
    if segments[segments.len() - 1].is_empty() {
        out.push("'".to_string());
    } else {
        let last = out.len() - 1;
        out[last].push('\'');
    }
    out
}

/// Reference `decode_double`: inverse of `reemit_double`.
/// Soft folds rejoin with one space, `\ ` decodes to a literal space, every
/// other escape passes through verbatim.
pub fn decode_double(block: &[String], prefix: &str, cont_indent: usize) -> String {
    if block.is_empty() {
        return String::new();
    }
    let last = &block[block.len() - 1];
    let mut body: Vec<String> = block[..block.len() - 1].to_vec();
    body.push(cut_at_last_quote(last, '"'));
    let plen = prefix.chars().count();
    let mut parts: Vec<String> = Vec::with_capacity(body.len());
    parts.push(body[0].chars().skip(plen).collect());
    for l in &body[1..] {
        parts.push(l.chars().skip(cont_indent).collect());
    }
    let s: Vec<char> = parts.join(" ").chars().collect();
    let n = s.len();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        if s[i] == '\\' && i + 1 < n {
            if s[i + 1] == ' ' {
                out.push(' ');
            } else {
                out.push(s[i]);
                out.push(s[i + 1]);
            }
            i += 2;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// Reference `reemit_double`: double-quoted content is one continuous
/// escaped flow, so it re-wraps at `width` and closes with `"`.
pub fn reemit_double(escaped: &str, prefix: &str, cont_indent: usize, width: usize) -> Vec<String> {
    let ind = " ".repeat(cont_indent);
    let mut out = Vec::new();
    let mut cur = prefix.to_string();
    let mut vcol = prefix.chars().count();
    let mut first = true;
    for word in escaped.split(' ') {
        let wlen = word.chars().count();
        if first {
            cur.push_str(word);
            vcol += wlen;
            first = false;
        } else if !word.is_empty() && vcol + 1 > width {
            out.push(std::mem::replace(&mut cur, format!("{}{}", ind, word)));
            vcol = cont_indent + wlen;
        } else {
            cur.push(' ');
            cur.push_str(word);
            vcol += 1 + wlen;
        }
    }
    cur.push('"');
    out.push(cur);
    out
}

/// Reference `EMPTY_FLOW.sub`: the merge injects `: ''` where the editor
/// leaves a bare `: `.
/// Strip `''` from a `: ''` immediately followed by `,` or `}`, keeping the
/// trailing space.
pub fn empty_flow(text: &str) -> String {
    let ch: Vec<char> = text.chars().collect();
    let n = ch.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < n {
        if i + 4 < n
            && ch[i] == ':'
            && ch[i + 1] == ' '
            && ch[i + 2] == '\''
            && ch[i + 3] == '\''
            && (ch[i + 4] == ',' || ch[i + 4] == '}')
        {
            out.push(':');
            out.push(' ');
            i += 4;
        } else {
            out.push(ch[i]);
            i += 1;
        }
    }
    out
}

/// The editor's plain-scalar fold width.
pub const PLAIN_WIDTH: usize = 79;
/// The editor's quoted-scalar fold width.
pub const QUOTED_WIDTH: usize = 80;

// Value heads that are not plain scalars: block/flow indicators and YAML
// sigils.
// A value opening with one of these passes through untouched.
fn is_exclude_first(c: char) -> bool {
    matches!(c, '|' | '>' | '{' | '[' | '&' | '*' | '!' | '#' | '%')
}

// Reference `SEQ = ^(\s*)- (\S.*)$`: a bare plain scalar sequence item
// `- value`, with no `key:` form.
// Returns (indent, value); value is the non-space char and the rest.
fn seq_match(line: &str) -> Option<(String, String)> {
    let ch: Vec<char> = line.chars().collect();
    let n = ch.len();
    let mut p = 0;
    while p < n && ch[p].is_whitespace() {
        p += 1;
    }
    if p + 1 >= n || ch[p] != '-' || ch[p + 1] != ' ' {
        return None;
    }
    let v = p + 2;
    if v >= n || ch[v].is_whitespace() {
        return None;
    }
    Some((ch[..p].iter().collect(), ch[v..].iter().collect()))
}

// Reference `MAPPINGISH = ^[\w.\-/]+:(\s|$)`: a `key:` mapping, not a
// scalar.
// Keeps a `key:`-looking sequence value from folding.
fn mappingish(value: &str) -> bool {
    let ch: Vec<char> = value.chars().collect();
    let n = ch.len();
    let mut k = 0;
    while k < n && is_key_char(ch[k]) {
        k += 1;
    }
    if k == 0 || k >= n || ch[k] != ':' {
        return false;
    }
    k + 1 == n || ch[k + 1].is_whitespace()
}

// Leading-space count of a continuation line, the folded-value indent.
fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|&c| c == ' ').count()
}

/// Reference `reserialize`: re-wrap `text` the way the Unity editor would.
/// Plain scalars fold at `width`, quoted at `quoted_width`; pass `INF` to
/// unwrap.
/// Mixed-LF/CRLF quoted blocks pass through verbatim, since a terminator is
/// never invented.
/// With `fix_empty`, the merge's injected `''` flow values are stripped.
/// Idempotent on editor-form input.
pub fn reserialize(text: &str, width: usize, quoted_width: usize, fix_empty: bool) -> String {
    let pairs = split_lines(text);
    let lines: Vec<String> = pairs.iter().map(|(c, _)| c.clone()).collect();
    let crs: Vec<bool> = pairs.iter().map(|(_, cr)| *cr).collect();
    let n = lines.len();
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    while i < n {
        let line = &lines[i];
        if let Some((indent, _key, val)) = key_match(line) {
            let first = match val.chars().next() {
                Some(c) => c,
                None => {
                    out.push((line.clone(), crs[i]));
                    i += 1;
                    continue;
                }
            };
            if is_exclude_first(first) {
                out.push((line.clone(), crs[i]));
                i += 1;
                continue;
            }
            if first == '\'' || first == '"' {
                let quote_col = line.chars().count() - val.chars().count();
                let (block, j) = gather_quoted(&lines, i, quote_col, first);
                if crs[i..j].iter().all(|&c| c == crs[i]) {
                    let qp: String = line.chars().take(quote_col + 1).collect();
                    // block[1..-1]: lines strictly between first and last.
                    // A range like 1..0 is invalid for a 1-line block, so a
                    // checked slice yields empty there instead.
                    let inner = block.get(1..block.len().saturating_sub(1)).unwrap_or(&[]);
                    let ci = inner
                        .iter()
                        .find(|l| !l.trim().is_empty())
                        .map(|l| leading_spaces(l))
                        .unwrap_or(indent.chars().count() + 2);
                    let decoded = if first == '\'' {
                        decode_quoted(&block, &qp, ci)
                    } else {
                        decode_double(&block, &qp, ci)
                    };
                    let emitted = if first == '\'' {
                        reemit_quoted(&decoded, &qp, ci, quoted_width)
                    } else {
                        reemit_double(&decoded, &qp, ci, quoted_width)
                    };
                    out.extend(emitted.into_iter().map(|e| (e, crs[i])));
                } else {
                    out.extend((i..j).map(|k| (lines[k].clone(), crs[k])));
                }
                i = j;
                continue;
            }
            let indent_len = indent.chars().count();
            let (conts, j) = gather_continuations(&lines, i, indent_len);
            let prefix: String = line
                .chars()
                .take(line.chars().count() - val.chars().count())
                .collect();
            let cont_indent = match conts.first() {
                Some(c0) => " ".repeat(leading_spaces(c0)),
                None => " ".repeat(indent_len + 2),
            };
            let value = join_plain_value(&val, &conts, &cont_indent);
            out.extend(
                reemit_plain(&value, &prefix, &cont_indent, width)
                    .into_iter()
                    .map(|e| (e, crs[i])),
            );
            i = j;
            continue;
        }
        if let Some((sindent, svalue)) = seq_match(line) {
            let sfirst = svalue.chars().next();
            let plain = sfirst.is_some_and(|c| {
                !is_exclude_first(c)
                    && c != '\''
                    && c != '"'
                    && !svalue.starts_with("- ")
                    && !mappingish(&svalue)
            });
            if plain {
                let sindent_len = sindent.chars().count();
                let (conts, j) = gather_continuations(&lines, i, sindent_len);
                let prefix: String = line
                    .chars()
                    .take(line.chars().count() - svalue.chars().count())
                    .collect();
                let cont_indent = match conts.first() {
                    Some(c0) => " ".repeat(leading_spaces(c0)),
                    None => " ".repeat(prefix.chars().count()),
                };
                let value = join_plain_value(&svalue, &conts, &cont_indent);
                out.extend(
                    reemit_plain(&value, &prefix, &cont_indent, width)
                        .into_iter()
                        .map(|e| (e, crs[i])),
                );
                i = j;
                continue;
            }
        }
        out.push((line.clone(), crs[i]));
        i += 1;
    }
    let result: String = out
        .iter()
        .map(|(c, cr)| format!("{}{}", c, if *cr { "\r" } else { "" }))
        .collect::<Vec<_>>()
        .join("\n");
    if fix_empty {
        empty_flow(&result)
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn garbage_nested_quote_matches_reference_not_idempotent() {
        // Fuzzing found rewrap is not idempotent on non-editor-form input:
        // an unterminated quote makes gather_quoted span into a later
        // scalar, and each pass reshapes the text.
        // The Python reference does the same, so we pin parity, not
        // idempotence.
        // Idempotence holds on editor-form input, proven over the corpus.
        let input = concat!(
            "e_en\n  m_Localized: \"spae_en\n  m_Localized: \"spaced\\ ",
            "f f ff f f f f f f f f f f f v f f f f  end\"\n"
        );
        let once = reserialize(input, PLAIN_WIDTH, QUOTED_WIDTH, true);
        let twice = reserialize(&once, PLAIN_WIDTH, QUOTED_WIDTH, true);
        assert_eq!(
            once,
            concat!(
                "e_en\n  m_Localized: \"spae_en Localized: \"spaced f f ff ",
                "f f f f f f f f f f f v f f f f \n    end\"\n"
            )
        );
        assert_eq!(
            twice,
            "e_en\n  m_Localized: \"spae_en Localized: \"\n    end\"\n"
        );
    }

    // Editor-form document generator for the SPEC 2.7 properties.
    // The alphabet carries structural punctuation, quotes, newlines and CR
    // so documents hit every dispatch branch.
    // Idempotence and losslessness hold only on editor-emittable text:
    // balanced quoting, CR only as a line terminator.
    // Raw char soup reaches unterminated-quote states where the reference
    // is not idempotent; those are left to no_panic_on_arbitrary, the
    // fuzzers and the pinned garbage parity test.
    fn arb_asset_line() -> impl Strategy<Value = String> {
        let key = "[a-z][a-zA-Z0-9_]{0,6}";
        // No trailing space: an unfolded value with a trailing run past the
        // width is not editor-emittable, and the fold there drops a
        // whitespace-only continuation in the reference too.
        let plain = "([a-zA-Z0-9 .,]{0,69}[a-zA-Z0-9.,])?";
        prop_oneof![
            (key, plain).prop_map(|(k, v)| format!("  {k}: {v}")),
            (key, plain).prop_map(|(k, v)| format!("  {k}: '{}'", v.replace(' ', "  "))),
            (key, plain).prop_map(|(k, v)| format!("  {k}: \"{v}\\r\"")),
            (key, plain).prop_map(|(k, v)| format!("  - {k}: {v}")),
            plain.prop_map(|v| format!("  - {v}")),
            (key, key).prop_map(|(a, b)| format!("  {a}: {{class: {b}, ns: , asm: }}")),
            key.prop_map(|k| format!("  {k}:")),
            Just("--- !u!114 &11400000".to_string()),
            Just("  m_TableData:".to_string()),
        ]
    }

    fn arb_asset_text() -> impl Strategy<Value = String> {
        (proptest::collection::vec(
            (arb_asset_line(), proptest::bool::ANY),
            0..12,
        ),)
            .prop_map(|(lines,)| {
                lines
                    .into_iter()
                    .map(|(l, cr)| if cr { format!("{l}\r") } else { l })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
    }

    fn rewrap(t: &str) -> String {
        reserialize(t, PLAIN_WIDTH, QUOTED_WIDTH, true)
    }

    fn unwrap_codec(t: &str) -> String {
        reserialize(t, INF, INF, false)
    }

    #[test]
    fn split_lines_records_cr_per_line() {
        let got = split_lines("a\r\nb\nc\r");
        assert_eq!(
            got,
            vec![
                ("a".to_string(), true),
                ("b".to_string(), false),
                ("c".to_string(), true),
            ]
        );
    }

    #[test]
    fn split_lines_trailing_newline_yields_empty_tail() {
        assert_eq!(
            split_lines("x\n"),
            vec![("x".to_string(), false), (String::new(), false)]
        );
    }

    #[test]
    fn key_match_plain() {
        let (i, k, v) = key_match("  m_Name: Table_en").unwrap();
        assert_eq!(
            (i.as_str(), k.as_str(), v.as_str()),
            ("  ", "m_Name", "Table_en")
        );
    }

    #[test]
    fn key_match_absorbs_sequence_dashes() {
        let (i, k, v) = key_match("  - - m_Key: value here").unwrap();
        assert_eq!(
            (i.as_str(), k.as_str(), v.as_str()),
            ("  - - ", "m_Key", "value here")
        );
    }

    #[test]
    fn key_match_keeps_trailing_spaces_in_value() {
        let (_, _, v) = key_match("  k:  x  ").unwrap();
        // one \s eats the first space; the rest is group 3 verbatim.
        assert_eq!(v, " x  ");
    }

    #[test]
    fn key_match_rejects_bare_sequence_and_empty_value() {
        assert!(key_match("  - Some.Assembly.Type, Version=1").is_none());
        // \s eats the space, nothing left
        assert!(key_match("  key: ").is_none());
        assert!(key_match("  key:").is_none());
    }

    #[test]
    fn reemit_plain_infinite_width_never_folds() {
        let out = reemit_plain("a b c d e", "  k: ", "    ", INF);
        assert_eq!(out, vec!["  k: a b c d e".to_string()]);
    }

    #[test]
    fn reemit_plain_folds_at_last_space_of_a_run() {
        // width 3: fold triggers once col > 3 at a run's last space; the
        // earlier spaces stay as trailing whitespace.
        let out = reemit_plain("aa    bb", "", "", 3);
        assert_eq!(out, vec!["aa   ".to_string(), "bb".to_string()]);
    }

    #[test]
    fn gather_continuations_stops_at_key_blank_and_dedent() {
        let lines: Vec<String> = [
            "  k: v",         // 0, the key line
            "    more text",  // 1, continuation
            "    k2: nested", // 2, a KEY line, stops
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (conts, j) = gather_continuations(&lines, 0, 2);
        assert_eq!(conts, vec!["    more text".to_string()]);
        assert_eq!(j, 2);
    }

    #[test]
    fn gather_continuations_stops_on_dedented_sibling() {
        let lines: Vec<String> = ["  k: v", "  sibling: x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (conts, j) = gather_continuations(&lines, 0, 2);
        assert!(conts.is_empty());
        assert_eq!(j, 1);
    }

    fn lines(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reemit_quoted_folds_and_closes() {
        let got = reemit_quoted("word word word word word word end", "  k: ", 4, 20);
        assert_eq!(
            got,
            lines(&["  k: word word word word", "    word word end'"])
        );
    }

    #[test]
    fn reemit_quoted_escapes_apostrophe() {
        assert_eq!(
            reemit_quoted("it's here", "  k: ", 4, 80),
            lines(&["  k: it''s here'"])
        );
    }

    #[test]
    fn reemit_quoted_blank_line_becomes_blank_physical_lines() {
        assert_eq!(
            reemit_quoted("a\n\nb", "  k: ", 4, 80),
            lines(&["  k: a", "", "", "    b'"])
        );
    }

    #[test]
    fn decode_quoted_roundtrips_apostrophe() {
        let block = reemit_quoted("it's x", "  k: ", 4, 80);
        assert_eq!(decode_quoted(&block, "  k: ", 4), "it's x");
    }

    #[test]
    fn decode_quoted_drops_last_char_on_missing_close() {
        // bug-compatibility: no closing quote makes rfind return -1, so the
        // reference drops the last char of the block.
        assert_eq!(
            decode_quoted(&lines(&["  k: 'aa", "    bbcc"]), "  k: '", 4),
            "aa bbc"
        );
    }

    #[test]
    fn reemit_double_folds_and_closes() {
        assert_eq!(
            reemit_double("alpha beta gamma delta epsilon", "  k: ", 4, 16),
            lines(&["  k: alpha beta gamma", "    delta epsilon\""])
        );
    }

    #[test]
    fn decode_double_unescapes_escaped_space() {
        assert_eq!(
            decode_double(&lines(&["  k: \"al\\ pha end\""]), "  k: \"", 6),
            "al pha end"
        );
    }

    #[test]
    fn decode_double_drops_last_char_on_missing_close() {
        assert_eq!(
            decode_double(&lines(&["  k: \"aa", "    bbcc"]), "  k: \"", 4),
            "aa bbc"
        );
    }

    #[test]
    fn gather_quoted_spans_multiline_double_block() {
        let ls = lines(&["  k: \"aa", "    bb\"", "  next: 1"]);
        let (block, j) = gather_quoted(&ls, 0, 5, '"');
        assert_eq!(block, lines(&["  k: \"aa", "    bb\""]));
        assert_eq!(j, 2);
    }

    #[test]
    fn gather_quoted_treats_double_apostrophe_as_escape() {
        let ls = lines(&["  k: 'it''s here'", "  x: 1"]);
        let (block, j) = gather_quoted(&ls, 0, 5, '\'');
        assert_eq!(block, lines(&["  k: 'it''s here'"]));
        assert_eq!(j, 1);
    }

    #[test]
    fn gather_quoted_double_backslash_escapes_the_fold_newline() {
        // A trailing backslash escapes the fold '\n', so the scan must cross
        // into the next line to find the close.
        let ls = lines(&["  k: \"aa\\", "    bb\"", "  x: 1"]);
        let (block, j) = gather_quoted(&ls, 0, 5, '"');
        assert_eq!(block, lines(&["  k: \"aa\\", "    bb\""]));
        assert_eq!(j, 2);
    }

    #[test]
    fn gather_quoted_missing_close_spans_to_eof() {
        // No closing quote: the block runs to the end of the file.
        let ls = lines(&["  k: 'aa", "    bb"]);
        let (block, j) = gather_quoted(&ls, 0, 5, '\'');
        assert_eq!(block, lines(&["  k: 'aa", "    bb"]));
        assert_eq!(j, 2);
    }

    #[test]
    fn empty_flow_strips_only_inside_flow() {
        assert_eq!(empty_flow("{class: '', ns: ''}"), "{class: , ns: }");
        // a real empty value, no , or }
        assert_eq!(empty_flow("plain: ''"), "plain: ''");
    }

    proptest! {
        // reemit_plain then join_plain_value round-trips any plain value:
        // folds drop exactly one space each, join restores exactly one.
        #[test]
        fn reemit_join_roundtrip(
            value in "[a-zA-Z ]{0,80}",
            prefix_len in 0usize..6,
            cont_len in 0usize..6,
            width in 1usize..40,
        ) {
            let prefix = " ".repeat(prefix_len);
            let cont_indent = " ".repeat(cont_len);
            let lines = reemit_plain(&value, &prefix, &cont_indent, width);
            let first = lines[0].strip_prefix(&prefix).unwrap();
            let joined = join_plain_value(first, &lines[1..], &cont_indent);
            prop_assert_eq!(joined, value);
        }

        // At INF width a value is a single line, byte-for-byte prefix+value.
        #[test]
        fn reemit_infinite_is_single_line(
            value in "[^\n]{0,120}",
            prefix_len in 0usize..6,
        ) {
            let prefix = " ".repeat(prefix_len);
            let lines = reemit_plain(&value, &prefix, "  ", INF);
            prop_assert_eq!(lines.len(), 1);
            prop_assert_eq!(&lines[0], &format!("{}{}", prefix, value));
        }

        // split_lines is exactly reversible by re-joining content+terminator.
        #[test]
        fn split_lines_reversible(text in "[a-zA-Z\r\n]{0,60}") {
            let rebuilt: String = split_lines(&text)
                .iter()
                .map(|(c, cr)| format!("{}{}", c, if *cr { "\r" } else { "" }))
                .collect::<Vec<_>>()
                .join("\n");
            prop_assert_eq!(rebuilt, text);
        }

        // empty_flow is idempotent: stripping never creates a new match.
        #[test]
        fn empty_flow_idempotent(text in "[a-zA-Z:', {}]{0,60}") {
            let once = empty_flow(&text);
            prop_assert_eq!(empty_flow(&once), once.clone());
            // and it never lengthens the text.
            prop_assert!(once.chars().count() <= text.chars().count());
        }

        // reemit_double always closes with `"` and yields at least one line.
        #[test]
        fn reemit_double_always_closes(
            escaped in "[a-zA-Z \\\\]{0,60}", ci in 0usize..6, width in 1usize..30,
        ) {
            let out = reemit_double(&escaped, "  k: \"", ci, width);
            prop_assert!(!out.is_empty());
            prop_assert!(out[out.len() - 1].ends_with('"'));
        }

        // reemit_quoted always closes non-empty content with `'`.
        #[test]
        fn reemit_quoted_always_closes(
            content in "[a-zA-Z '\n]{1,60}", ci in 0usize..6, width in 1usize..30,
        ) {
            let out = reemit_quoted(&content, "  k: ", ci, width);
            prop_assert!(!out.is_empty());
            prop_assert!(out[out.len() - 1].ends_with('\''));
        }

        // gather_quoted returns the lines[i..j] prefix with i < j <= len.
        #[test]
        fn gather_quoted_returns_prefix_block(
            n in 1usize..8, quote_col in 0usize..4,
        ) {
            let ls: Vec<String> = (0..n).map(|k| format!("  k{}: 'v", k)).collect();
            let (block, j) = gather_quoted(&ls, 0, quote_col, '\'');
            prop_assert!(j >= 1 && j <= ls.len());
            prop_assert_eq!(&block[..], &ls[0..j]);
        }

        // SPEC 2.7 required properties.
        // The alphabet carries the structural chars so every dispatch branch
        // is exercised.
        #[test]
        fn reserialize_idempotent(text in arb_asset_text()) {
            let once = rewrap(&text);
            prop_assert_eq!(rewrap(&once), once.clone());
        }

        #[test]
        fn unwrap_lossless(text in arb_asset_text()) {
            let canon = rewrap(&text);
            prop_assert_eq!(rewrap(&unwrap_codec(&canon)), canon.clone());
        }

        #[test]
        fn unwrap_canonicalizes(text in arb_asset_text()) {
            prop_assert_eq!(unwrap_codec(&text), unwrap_codec(&rewrap(&text)));
        }

        // No panic on arbitrary input; invalid UTF-8 is a CLI-layer concern.
        #[test]
        fn no_panic_on_arbitrary(text in ".{0,300}") {
            let _ = rewrap(&text);
            let _ = unwrap_codec(&text);
        }
    }
}
