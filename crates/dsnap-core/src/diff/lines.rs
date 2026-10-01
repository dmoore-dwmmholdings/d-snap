//! Line diffs and hunks via `similar`. Owner: Chain G (DSNA-10).
//!
//! Everything here is a pure function over in-memory text or bytes. The diff is for display
//! only: original bytes are never changed. One hunk model serves both the inline and the
//! side-by-side view; [`side_by_side_rows`] pairs deleted and inserted lines for the latter.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffOp};

use crate::types::{DiffLine, DiffOptions, Hunk, LineTag};

/// Time budget for one line diff. Past it, `similar` returns a coarser (still correct) diff
/// instead of searching for the minimal one, so pathological inputs cannot hang the UI.
pub const DIFF_DEADLINE: Duration = Duration::from_millis(500);

/// Line diff of two texts grouped into hunks with `opts.context` lines of context.
///
/// Lines are split on `\n`; a line's text is shown without its line ending. Without
/// `opts.ignore_whitespace`, a changed line ending (CRLF vs LF, or a missing final newline)
/// counts as a change. With it, trailing whitespace, the amount of whitespace in a run and
/// line endings are ignored; the hunks still show the text as stored.
pub fn diff_text(old: &str, new: &str, opts: &DiffOptions) -> Vec<Hunk> {
    let prepared = Prepared::new(old, new, opts.ignore_whitespace);
    let ops = prepared.ops();
    let context = usize::try_from(opts.context).unwrap_or(usize::MAX);
    similar::group_diff_ops(ops, context)
        .iter()
        .map(|group| build_hunk(group, &prepared))
        .filter(|h| h.lines.iter().any(|l| l.tag != LineTag::Equal))
        .collect()
}

/// Lines added and removed between two texts: `(added, removed)`.
///
/// Uses the same comparison as [`diff_text`] but builds no hunks.
pub fn line_counts(old: &str, new: &str, opts: &DiffOptions) -> (u32, u32) {
    let prepared = Prepared::new(old, new, opts.ignore_whitespace);
    let (mut added, mut removed) = (0usize, 0usize);
    for op in prepared.ops() {
        if !matches!(op, DiffOp::Equal { .. }) {
            added += op.new_range().len();
            removed += op.old_range().len();
        }
    }
    (saturating_u32(added), saturating_u32(removed))
}

/// [`diff_text`] over raw file bytes, decoded with [`decode_text`].
pub fn diff_bytes(old: &[u8], new: &[u8], opts: &DiffOptions) -> Vec<Hunk> {
    diff_text(&decode_text(old), &decode_text(new), opts)
}

/// [`line_counts`] over raw file bytes, decoded with [`decode_text`].
pub fn line_counts_bytes(old: &[u8], new: &[u8], opts: &DiffOptions) -> (u32, u32) {
    line_counts(&decode_text(old), &decode_text(new), opts)
}

/// Decode file bytes for display.
///
/// A UTF-16 LE or BE byte-order mark selects UTF-16 (the mark itself is dropped; an odd
/// trailing byte or unpaired surrogate becomes U+FFFD). Anything else is read as UTF-8, with
/// invalid sequences replaced by U+FFFD. A UTF-8 BOM is kept as U+FEFF so adding or removing
/// it still shows as a change.
pub fn decode_text(bytes: &[u8]) -> Cow<'_, str> {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => Cow::Owned(decode_utf16(rest, u16::from_le_bytes)),
        [0xFE, 0xFF, rest @ ..] => Cow::Owned(decode_utf16(rest, u16::from_be_bytes)),
        _ => String::from_utf8_lossy(bytes),
    }
}

/// True when `bytes` start with a UTF-16 LE or BE byte-order mark.
pub fn has_utf16_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF])
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let chunks = bytes.chunks_exact(2);
    let odd = !chunks.remainder().is_empty();
    let units = chunks.map(|c| unit([c[0], c[1]]));
    let mut s: String = char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    if odd {
        s.push(char::REPLACEMENT_CHARACTER);
    }
    s
}

/// One side of a side-by-side row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideCell {
    /// 1-based line number on this side.
    pub no: u32,
    /// Line text without its line ending.
    pub text: String,
    /// True for a deleted (old side) or inserted (new side) line; false for context.
    pub changed: bool,
}

/// One row of the side-by-side view.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideBySideRow {
    /// Index of the hunk this row belongs to; the UI draws a separator when it changes.
    pub hunk: u32,
    /// Old side, or `None` for a blank filler cell.
    pub old: Option<SideCell>,
    /// New side, or `None` for a blank filler cell.
    pub new: Option<SideCell>,
}

/// Lay hunks out for the side-by-side view.
///
/// Context lines appear on both sides. Within each run of changed lines, the n-th deleted line
/// is paired with the n-th inserted line; the longer side gets blank filler cells.
pub fn side_by_side_rows(hunks: &[Hunk]) -> Vec<SideBySideRow> {
    let mut rows = Vec::new();
    for (index, hunk) in hunks.iter().enumerate() {
        let hunk_no = saturating_u32(index);
        let mut deleted: Vec<SideCell> = Vec::new();
        let mut inserted: Vec<SideCell> = Vec::new();
        for line in &hunk.lines {
            match line.tag {
                LineTag::Delete => {
                    if !inserted.is_empty() {
                        flush_run(&mut rows, hunk_no, &mut deleted, &mut inserted);
                    }
                    deleted.push(cell(line.old_no, &line.text, true));
                }
                LineTag::Insert => inserted.push(cell(line.new_no, &line.text, true)),
                LineTag::Equal => {
                    flush_run(&mut rows, hunk_no, &mut deleted, &mut inserted);
                    rows.push(SideBySideRow {
                        hunk: hunk_no,
                        old: Some(cell(line.old_no, &line.text, false)),
                        new: Some(cell(line.new_no, &line.text, false)),
                    });
                }
            }
        }
        flush_run(&mut rows, hunk_no, &mut deleted, &mut inserted);
    }
    rows
}

fn cell(no: Option<u32>, text: &str, changed: bool) -> SideCell {
    SideCell {
        no: no.unwrap_or(0),
        text: text.to_owned(),
        changed,
    }
}

fn flush_run(
    rows: &mut Vec<SideBySideRow>,
    hunk: u32,
    deleted: &mut Vec<SideCell>,
    inserted: &mut Vec<SideCell>,
) {
    let mut old = deleted.drain(..);
    let mut new = inserted.drain(..);
    loop {
        let (o, n) = (old.next(), new.next());
        if o.is_none() && n.is_none() {
            break;
        }
        rows.push(SideBySideRow {
            hunk,
            old: o,
            new: n,
        });
    }
}

/// Both texts split into lines, plus one interned comparison key per line.
struct Prepared<'a> {
    old_lines: Vec<&'a str>,
    new_lines: Vec<&'a str>,
    old_keys: Vec<u32>,
    new_keys: Vec<u32>,
}

impl<'a> Prepared<'a> {
    fn new(old: &'a str, new: &'a str, ignore_whitespace: bool) -> Self {
        let old_raw: Vec<&'a str> = old.split_inclusive('\n').collect();
        let new_raw: Vec<&'a str> = new.split_inclusive('\n').collect();
        let mut interner: HashMap<Cow<'a, str>, u32> =
            HashMap::with_capacity(old_raw.len() + new_raw.len());
        let mut keys = |raw: &[&'a str]| -> Vec<u32> {
            raw.iter()
                .enumerate()
                .map(|(i, &line)| {
                    let k = if ignore_whitespace {
                        normalize_whitespace(line, i == 0)
                    } else {
                        Cow::Borrowed(line)
                    };
                    let next = saturating_u32(interner.len());
                    *interner.entry(k).or_insert(next)
                })
                .collect()
        };
        let old_keys = keys(&old_raw);
        let new_keys = keys(&new_raw);
        Self {
            old_lines: old_raw.into_iter().map(strip_eol).collect(),
            new_lines: new_raw.into_iter().map(strip_eol).collect(),
            old_keys,
            new_keys,
        }
    }

    fn ops(&self) -> Vec<DiffOp> {
        let deadline = Instant::now().checked_add(DIFF_DEADLINE);
        diff_keys(&self.old_keys, &self.new_keys, deadline)
    }
}

/// Patience-anchored diff of two key sequences.
///
/// Lines that occur exactly once on each side are matched up by a longest increasing
/// subsequence (O(n log n)); the gaps between those anchors are diffed with Myers. Plain
/// Myers is O(N·D), which is too slow when many lines change across a large file (DSNA-40);
/// `similar`'s own Patience runs Myers over all unique lines and degrades the same way.
fn diff_keys(old: &[u32], new: &[u32], deadline: Option<Instant>) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let (mut o, mut n) = (0usize, 0usize);
    for (ao, an) in unique_anchors(old, new) {
        ops.extend(similar::capture_diff_deadline(
            Algorithm::Myers,
            old,
            o..ao,
            new,
            n..an,
            deadline,
        ));
        ops.push(DiffOp::Equal {
            old_index: ao,
            new_index: an,
            len: 1,
        });
        (o, n) = (ao + 1, an + 1);
    }
    ops.extend(similar::capture_diff_deadline(
        Algorithm::Myers,
        old,
        o..old.len(),
        new,
        n..new.len(),
        deadline,
    ));
    normalize_ops(ops)
}

/// `(old_index, new_index)` pairs of lines unique on both sides, forming the longest chain
/// increasing on both sides.
fn unique_anchors(old: &[u32], new: &[u32]) -> Vec<(usize, usize)> {
    const NONE: usize = usize::MAX;
    let n_keys = old.iter().chain(new).max().map_or(0, |&m| m as usize + 1);
    // Per key: count and position on each side (position valid when the count is 1).
    let mut old_count = vec![0u8; n_keys];
    let mut new_count = vec![0u8; n_keys];
    let mut new_pos = vec![NONE; n_keys];
    for &k in old {
        let c = &mut old_count[k as usize];
        *c = c.saturating_add(1);
    }
    for (i, &k) in new.iter().enumerate() {
        let c = &mut new_count[k as usize];
        *c = c.saturating_add(1);
        new_pos[k as usize] = i;
    }
    let pairs: Vec<(usize, usize)> = old
        .iter()
        .enumerate()
        .filter(|&(_, &k)| old_count[k as usize] == 1 && new_count[k as usize] == 1)
        .map(|(i, &k)| (i, new_pos[k as usize]))
        .collect();

    // Patience sorting: tails[l] = index into `pairs` of the smallest new_index ending an
    // increasing run of length l + 1; prev links rebuild the run.
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![NONE; pairs.len()];
    for (i, &(_, ni)) in pairs.iter().enumerate() {
        let l = tails.partition_point(|&t| pairs[t].1 < ni);
        if l > 0 {
            prev[i] = tails[l - 1];
        }
        if l == tails.len() {
            tails.push(i);
        } else {
            tails[l] = i;
        }
    }
    let mut chain = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied().unwrap_or(NONE);
    while cur != NONE {
        chain.push(pairs[cur]);
        cur = prev[cur];
    }
    chain.reverse();
    chain
}

/// Rewrite the op indices from a running cursor, drop empty ops and join adjacent `Equal`
/// ops so `group_diff_ops` sees each unchanged run as one op.
///
/// The cursor rewrite is needed because `similar` 2.7's Myers can report a `Delete`'s
/// `new_index` one past the true position (the lengths and order are right), which would
/// skew hunk ranges.
fn normalize_ops(ops: Vec<DiffOp>) -> Vec<DiffOp> {
    let mut out: Vec<DiffOp> = Vec::with_capacity(ops.len());
    let (mut o, mut n) = (0usize, 0usize);
    for op in ops {
        let (old_len, new_len) = (op.old_range().len(), op.new_range().len());
        let op = match op {
            DiffOp::Equal { len, .. } => {
                if let Some(DiffOp::Equal { len: prev, .. }) = out.last_mut() {
                    *prev += len;
                    (o, n) = (o + len, n + len);
                    continue;
                }
                DiffOp::Equal {
                    old_index: o,
                    new_index: n,
                    len,
                }
            }
            DiffOp::Delete { .. } => DiffOp::Delete {
                old_index: o,
                old_len,
                new_index: n,
            },
            DiffOp::Insert { .. } => DiffOp::Insert {
                old_index: o,
                new_index: n,
                new_len,
            },
            DiffOp::Replace { .. } => DiffOp::Replace {
                old_index: o,
                old_len,
                new_index: n,
                new_len,
            },
        };
        (o, n) = (o + old_len, n + new_len);
        if old_len + new_len > 0 {
            out.push(op);
        }
    }
    out
}

fn strip_eol(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Comparison key under "ignore whitespace": trailing whitespace and the line ending are
/// dropped, every other whitespace run becomes one space. A leading UTF-8 BOM on the first
/// line is dropped too.
fn normalize_whitespace(line: &str, first: bool) -> Cow<'_, str> {
    let line = if first {
        line.strip_prefix('\u{FEFF}').unwrap_or(line)
    } else {
        line
    };
    let trimmed = line.trim_end();
    let mut out = String::with_capacity(trimmed.len());
    let mut in_ws = false;
    for c in trimmed.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    if out == line {
        Cow::Borrowed(line)
    } else {
        Cow::Owned(out)
    }
}

fn build_hunk(group: &[DiffOp], p: &Prepared<'_>) -> Hunk {
    let mut lines = Vec::new();
    for op in group {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for i in 0..len {
                    lines.push(DiffLine {
                        tag: LineTag::Equal,
                        old_no: Some(line_no(old_index + i)),
                        new_no: Some(line_no(new_index + i)),
                        text: line_text(&p.new_lines, new_index + i),
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => push_deleted(&mut lines, p, old_index..old_index + old_len),
            DiffOp::Insert {
                new_index, new_len, ..
            } => push_inserted(&mut lines, p, new_index..new_index + new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                push_deleted(&mut lines, p, old_index..old_index + old_len);
                push_inserted(&mut lines, p, new_index..new_index + new_len);
            }
        }
    }
    let old = span(group.iter().map(DiffOp::old_range));
    let new = span(group.iter().map(DiffOp::new_range));
    Hunk {
        old_start: start_no(&old),
        old_len: saturating_u32(old.len()),
        new_start: start_no(&new),
        new_len: saturating_u32(new.len()),
        lines,
    }
}

fn line_text(lines: &[&str], i: usize) -> String {
    lines.get(i).copied().unwrap_or_default().to_owned()
}

fn push_deleted(lines: &mut Vec<DiffLine>, p: &Prepared<'_>, range: Range<usize>) {
    for i in range {
        lines.push(DiffLine {
            tag: LineTag::Delete,
            old_no: Some(line_no(i)),
            new_no: None,
            text: line_text(&p.old_lines, i),
        });
    }
}

fn push_inserted(lines: &mut Vec<DiffLine>, p: &Prepared<'_>, range: Range<usize>) {
    for i in range {
        lines.push(DiffLine {
            tag: LineTag::Insert,
            old_no: None,
            new_no: Some(line_no(i)),
            text: line_text(&p.new_lines, i),
        });
    }
}

/// Smallest range covering all `ranges` (ops in a group are contiguous).
fn span(mut ranges: impl Iterator<Item = Range<usize>>) -> Range<usize> {
    let Some(first) = ranges.next() else {
        return 0..0;
    };
    ranges.fold(first, |acc, r| acc.start.min(r.start)..acc.end.max(r.end))
}

/// 1-based first line of a range, or 0 when the range is empty.
fn start_no(r: &Range<usize>) -> u32 {
    if r.is_empty() { 0 } else { line_no(r.start) }
}

fn line_no(index: usize) -> u32 {
    saturating_u32(index.saturating_add(1))
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> DiffOptions {
        DiffOptions::default()
    }

    fn ws() -> DiffOptions {
        DiffOptions {
            ignore_whitespace: true,
            ..DiffOptions::default()
        }
    }

    /// Render hunks as unified-diff-like text for compact assertions.
    fn render(hunks: &[Hunk]) -> String {
        let mut s = String::new();
        for h in hunks {
            s.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                h.old_start, h.old_len, h.new_start, h.new_len
            ));
            for l in &h.lines {
                s.push(match l.tag {
                    LineTag::Equal => ' ',
                    LineTag::Insert => '+',
                    LineTag::Delete => '-',
                });
                s.push_str(&l.text);
                s.push('\n');
            }
        }
        s
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        let t = numbered(10);
        assert!(diff_text(&t, &t, &opts()).is_empty());
        assert_eq!(line_counts(&t, &t, &opts()), (0, 0));
        assert!(diff_text("", "", &opts()).is_empty());
    }

    #[test]
    fn insert_in_middle_has_context_and_line_numbers() {
        let old = numbered(10);
        let new = old.replace("line 5\n", "line 5\nnew\n");
        let hunks = diff_text(&old, &new, &opts());
        assert_eq!(
            render(&hunks),
            "@@ -3,6 +3,7 @@\n line 3\n line 4\n line 5\n+new\n line 6\n line 7\n line 8\n"
        );
        let ins = &hunks[0].lines[3];
        assert_eq!((ins.old_no, ins.new_no), (None, Some(6)));
        let after = &hunks[0].lines[4];
        assert_eq!((after.old_no, after.new_no), (Some(6), Some(7)));
        assert_eq!(line_counts(&old, &new, &opts()), (1, 0));
    }

    #[test]
    fn delete_line() {
        let old = numbered(3);
        let new = "line 1\nline 3\n";
        assert_eq!(
            render(&diff_text(&old, new, &opts())),
            "@@ -1,3 +1,2 @@\n line 1\n-line 2\n line 3\n"
        );
        assert_eq!(line_counts(&old, new, &opts()), (0, 1));
    }

    #[test]
    fn modify_line_is_delete_then_insert() {
        let old = numbered(3);
        let new = "line 1\nLINE 2\nline 3\n";
        assert_eq!(
            render(&diff_text(&old, new, &opts())),
            "@@ -1,3 +1,3 @@\n line 1\n-line 2\n+LINE 2\n line 3\n"
        );
        assert_eq!(line_counts(&old, new, &opts()), (1, 1));
    }

    #[test]
    fn distant_changes_make_separate_hunks_and_context_is_respected() {
        let old = numbered(30);
        let new = old.replace("line 2\n", "two\n").replace("line 28\n", "x\n");
        let hunks = diff_text(&old, &new, &opts());
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].old_start, hunks[0].old_len), (1, 5));
        assert_eq!((hunks[1].old_start, hunks[1].old_len), (25, 6));

        let zero = DiffOptions {
            context: 0,
            ..opts()
        };
        assert_eq!(
            render(&diff_text(&old, &new, &zero)),
            "@@ -2,1 +2,1 @@\n-line 2\n+two\n@@ -28,1 +28,1 @@\n-line 28\n+x\n"
        );
    }

    #[test]
    fn crlf_only_change_shows_without_and_hides_with_ignore_whitespace() {
        let old = "a\r\nb\r\nc\r\n";
        let new = "a\nb\nc\n";
        let hunks = diff_text(old, new, &opts());
        assert_eq!(line_counts(old, new, &opts()), (3, 3));
        // Text is shown without line endings on both sides.
        assert!(hunks[0].lines.iter().all(|l| !l.text.contains('\r')));
        assert!(diff_text(old, new, &ws()).is_empty());
        assert_eq!(line_counts(old, new, &ws()), (0, 0));
    }

    #[test]
    fn ignore_whitespace_ignores_trailing_and_amount_but_not_presence() {
        let old = "fn  main() {\n    x;   \n}\n";
        let new = "fn main() {\n\tx;\n}\n";
        assert!(!diff_text(old, new, &opts()).is_empty());
        assert!(diff_text(old, new, &ws()).is_empty());
        // Adding whitespace where there was none is still a change.
        assert_eq!(line_counts("ab\n", "a b\n", &ws()), (1, 1));
        // Real changes still show, with the stored text.
        let hunks = diff_text("a  b\nc\n", "a b\nd\n", &ws());
        assert_eq!(render(&hunks), "@@ -1,2 +1,2 @@\n a b\n-c\n+d\n");
    }

    #[test]
    fn missing_trailing_newline() {
        let old = "a\nb";
        let new = "a\nb\n";
        // The last line's ending changed: shown as a change by default.
        assert_eq!(
            render(&diff_text(old, new, &opts())),
            "@@ -1,2 +1,2 @@\n a\n-b\n+b\n"
        );
        assert!(diff_text(old, new, &ws()).is_empty());

        // Appending to a file with no final newline.
        let hunks = diff_text("a", "a\nb", &opts());
        assert_eq!(render(&hunks), "@@ -1,1 +1,2 @@\n-a\n+a\n+b\n");
    }

    #[test]
    fn empty_versus_non_empty() {
        let hunks = diff_text("", "x\ny\n", &opts());
        assert_eq!(render(&hunks), "@@ -0,0 +1,2 @@\n+x\n+y\n");
        assert_eq!(line_counts("", "x\ny\n", &opts()), (2, 0));

        let hunks = diff_text("x\ny\n", "", &opts());
        assert_eq!(render(&hunks), "@@ -1,2 +0,0 @@\n-x\n-y\n");
        assert_eq!(line_counts("x\ny\n", "", &opts()), (0, 2));
    }

    fn le(s: &str) -> Vec<u8> {
        let mut b = vec![0xFF, 0xFE];
        b.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
        b
    }

    fn be(s: &str) -> Vec<u8> {
        let mut b = vec![0xFE, 0xFF];
        b.extend(s.encode_utf16().flat_map(u16::to_be_bytes));
        b
    }

    #[test]
    fn utf16_files_decode_and_diff() {
        assert_eq!(decode_text(&le("héllo\r\n")), "héllo\r\n");
        assert_eq!(decode_text(&be("wörld 🌍\n")), "wörld 🌍\n");
        assert!(has_utf16_bom(&le("x")) && has_utf16_bom(&be("x")));

        let old = le("one\r\ntwo\r\nthree\r\n");
        let new = le("one\r\n2\r\nthree\r\n");
        assert_eq!(
            render(&diff_bytes(&old, &new, &opts())),
            "@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
        );
        assert_eq!(line_counts_bytes(&old, &new, &opts()), (1, 1));
        // Same text in LE and BE decodes identically.
        assert!(diff_bytes(&le("a\nb\n"), &be("a\nb\n"), &opts()).is_empty());
        // An odd trailing byte becomes a replacement character instead of failing.
        let mut odd = le("a");
        odd.push(0x41);
        assert_eq!(decode_text(&odd), "a\u{FFFD}");
    }

    #[test]
    fn invalid_utf8_is_decoded_lossily() {
        let old = b"ok\n\xFF\xFE\xFD bad\n";
        // Starts with "ok", so no UTF-16 BOM: lossy UTF-8.
        assert_eq!(decode_text(old), "ok\n\u{FFFD}\u{FFFD}\u{FFFD} bad\n");
        assert_eq!(line_counts_bytes(old, b"ok\nfixed\n", &opts()), (1, 1));
    }

    #[test]
    fn utf8_bom_is_a_change_unless_ignoring_whitespace() {
        let old = b"\xEF\xBB\xBFa\nb\n";
        let new = b"a\nb\n";
        assert_eq!(line_counts_bytes(old, new, &opts()), (1, 1));
        assert_eq!(line_counts_bytes(old, new, &ws()), (0, 0));
    }

    type CellView<'a> = Option<(u32, &'a str, bool)>;

    fn view(rows: &[SideBySideRow]) -> Vec<(CellView<'_>, CellView<'_>)> {
        rows.iter()
            .map(|r| {
                (
                    r.old.as_ref().map(|c| (c.no, c.text.as_str(), c.changed)),
                    r.new.as_ref().map(|c| (c.no, c.text.as_str(), c.changed)),
                )
            })
            .collect()
    }

    #[test]
    fn side_by_side_pairs_runs() {
        let hunks = diff_text("a\nb\nc\nd\n", "a\nB\nC\nX\nd\n", &opts());
        let rows = side_by_side_rows(&hunks);
        assert_eq!(
            view(&rows),
            vec![
                (Some((1, "a", false)), Some((1, "a", false))),
                (Some((2, "b", true)), Some((2, "B", true))),
                (Some((3, "c", true)), Some((3, "C", true))),
                (None, Some((4, "X", true))),
                (Some((4, "d", false)), Some((5, "d", false))),
            ]
        );
        assert!(rows.iter().all(|r| r.hunk == 0));
    }

    #[test]
    fn side_by_side_numbers_hunks_and_handles_pure_deletes() {
        let old = numbered(30);
        let new = old.replace("line 2\n", "").replace("line 28\n", "x\n");
        let rows = side_by_side_rows(&diff_text(&old, &new, &opts()));
        assert_eq!(rows.first().map(|r| r.hunk), Some(0));
        assert_eq!(rows.last().map(|r| r.hunk), Some(1));
        let deleted = rows
            .iter()
            .find(|r| r.old.as_ref().is_some_and(|c| c.changed))
            .unwrap();
        assert_eq!(deleted.old.as_ref().unwrap().text, "line 2");
        assert!(deleted.new.is_none());
    }

    #[test]
    fn side_by_side_row_serializes_camel_case() {
        let row = SideBySideRow {
            hunk: 0,
            old: None,
            new: Some(SideCell {
                no: 1,
                text: "x".into(),
                changed: true,
            }),
        };
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            r#"{"hunk":0,"old":null,"new":{"no":1,"text":"x","changed":true}}"#
        );
    }

    /// Apply ops to `old` and check they produce `new`, cover both sides exactly once, and
    /// never leave two adjacent `Equal` ops.
    fn check_ops(old: &[u32], new: &[u32]) {
        let ops = diff_keys(old, new, None);
        let mut rebuilt = Vec::new();
        let (mut o, mut n) = (0, 0);
        for (i, op) in ops.iter().enumerate() {
            assert_eq!(
                (op.old_range().start, op.new_range().start),
                (o, n),
                "{ops:?}"
            );
            if let DiffOp::Equal { .. } = op {
                assert_eq!(&old[op.old_range()], &new[op.new_range()]);
                if i > 0 {
                    assert!(!matches!(ops[i - 1], DiffOp::Equal { .. }), "{ops:?}");
                }
            }
            rebuilt.extend_from_slice(&new[op.new_range()]);
            (o, n) = (op.old_range().end, op.new_range().end);
        }
        assert_eq!((o, n), (old.len(), new.len()));
        assert_eq!(rebuilt, new);
    }

    #[test]
    fn anchored_diff_is_a_valid_edit_script() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut rnd = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for _ in 0..200 {
            // Small alphabets give many duplicate lines; larger ones give many anchors.
            let alphabet = 2 + rnd(40);
            let old: Vec<u32> = (0..rnd(60)).map(|_| rnd(alphabet) as u32).collect();
            let mut new = Vec::new();
            for &k in &old {
                match rnd(6) {
                    0 => {}
                    1 => new.push(rnd(alphabet) as u32),
                    2 => new.extend([k, rnd(alphabet) as u32]),
                    _ => new.push(k),
                }
            }
            check_ops(&old, &new);
            check_ops(&new, &old);
        }
        check_ops(&[], &[]);
        check_ops(&[1, 2, 3], &[]);
        check_ops(&[], &[1, 2, 3]);
    }

    #[test]
    fn anchors_skip_lines_repeated_on_either_side() {
        // 1 is unique on both sides; 2 repeats in old; 3 repeats in new; 4 moves.
        // (0, 1) and (4, 0) cross, so only one of them can be kept.
        let anchors = unique_anchors(&[1, 2, 2, 3, 4, 5], &[4, 1, 3, 3, 5]);
        assert_eq!(anchors.len(), 2, "{anchors:?}");
        assert_eq!(anchors.last(), Some(&(5, 4)));
        assert!(anchors.iter().all(|&(o, _)| o == 0 || o == 4 || o == 5));
    }

    #[test]
    fn totally_different_inputs_finish() {
        // Two large, fully different inputs: the worst case for Myers.
        let old: String = (0..20_000).map(|i| format!("a{i}\n")).collect();
        let new: String = (0..20_000).map(|i| format!("b{i}\n")).collect();
        let start = Instant::now();
        assert_eq!(line_counts(&old, &new, &opts()), (20_000, 20_000));
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
