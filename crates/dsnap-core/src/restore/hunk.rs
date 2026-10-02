//! Undoing one hunk of a text diff on the file's original bytes (DSNA-59).
//!
//! The diff numbers lines split on `\n` (see [`crate::diff::lines`]). For UTF-8 and other
//! byte-oriented text, the raw bytes are split the same way, so every byte outside the
//! reverted lines (line endings, invalid UTF-8, a BOM) is kept exactly. A UTF-16 file (with
//! a byte-order mark) is decoded, edited by line and encoded again in its own byte order.
//!
//! Only the changed lines of the hunk are replaced: with `ignore_whitespace`, its context
//! lines may differ in whitespace between the sides, and they keep the working file's text.

use crate::diff::lines::{decode_text, has_utf16_bom};
use crate::error::{Error, Result};
use crate::types::{Hunk, LineTag};

/// One run of changed lines: replace `new[new.0..new.1]` with `old[old.0..old.1]`
/// (0-based line indexes).
#[derive(Debug, PartialEq, Eq)]
struct Run {
    old: (usize, usize),
    new: (usize, usize),
}

/// The changed runs of `hunk`, checked against its line numbers.
fn runs(hunk: &Hunk) -> Result<Vec<Run>> {
    // An empty range's start is the line after which it sits, so it is already the 0-based
    // index of the next line.
    let start = |s: u32, len: u32| s.saturating_sub(u32::from(len > 0)) as usize;
    let (mut o, mut n) = (
        start(hunk.old_start, hunk.old_len),
        start(hunk.new_start, hunk.new_len),
    );
    let bad = || Error::InvalidInput("hunk line numbers do not match its lines".into());
    let mut out: Vec<Run> = Vec::new();
    let mut open: Option<Run> = None;
    for l in &hunk.lines {
        if let Some(no) = l.old_no.filter(|_| l.tag != LineTag::Insert) {
            if no as usize != o + 1 {
                return Err(bad());
            }
        }
        if let Some(no) = l.new_no.filter(|_| l.tag != LineTag::Delete) {
            if no as usize != n + 1 {
                return Err(bad());
            }
        }
        match l.tag {
            LineTag::Equal => {
                out.extend(open.take());
                o += 1;
                n += 1;
            }
            LineTag::Delete => {
                let r = open.get_or_insert(Run {
                    old: (o, o),
                    new: (n, n),
                });
                o += 1;
                r.old.1 = o;
            }
            LineTag::Insert => {
                let r = open.get_or_insert(Run {
                    old: (o, o),
                    new: (n, n),
                });
                n += 1;
                r.new.1 = n;
            }
        }
    }
    out.extend(open);
    Ok(out)
}

/// Split into lines, each keeping its `\n` (the last may have none).
fn lines<T: PartialEq + Copy>(s: &[T], nl: T) -> Vec<&[T]> {
    s.split_inclusive(|c| *c == nl).collect()
}

/// Replace each run's new lines with its old lines.
fn apply<T: Clone>(old: &[&[T]], new: &[&[T]], runs: &[Run]) -> Result<Vec<T>> {
    let mut out = Vec::new();
    let mut at = 0;
    for r in runs {
        if r.new.0 < at || r.new.1 > new.len() || r.old.1 > old.len() {
            return Err(Error::InvalidInput(
                "hunk does not fit the file; compute the diff again".into(),
            ));
        }
        new[at..r.new.0]
            .iter()
            .for_each(|l| out.extend_from_slice(l));
        old[r.old.0..r.old.1]
            .iter()
            .for_each(|l| out.extend_from_slice(l));
        at = r.new.1;
    }
    new[at..].iter().for_each(|l| out.extend_from_slice(l));
    Ok(out)
}

/// The bytes of `new` with `hunk` (from the diff of `old` to `new`) undone.
pub(crate) fn revert(old: &[u8], new: &[u8], hunk: &Hunk) -> Result<Vec<u8>> {
    let runs = runs(hunk)?;
    if !has_utf16_bom(old) && !has_utf16_bom(new) {
        return apply(&lines(old, b'\n'), &lines(new, b'\n'), &runs);
    }
    // UTF-16: edit decoded code units, then encode in the working file's byte order (or the
    // old file's when the working file is not UTF-16).
    let units = |b: &[u8]| -> Vec<u16> { decode_text(b).encode_utf16().collect() };
    let (o, n) = (units(old), units(new));
    let nl = u16::from(b'\n');
    let text = apply(&lines(&o, nl), &lines(&n, nl), &runs)?;
    let be = if has_utf16_bom(new) {
        new.starts_with(&[0xFE, 0xFF])
    } else {
        old.starts_with(&[0xFE, 0xFF])
    };
    let mut out = if be {
        vec![0xFE, 0xFF]
    } else {
        vec![0xFF, 0xFE]
    };
    for u in text {
        out.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::lines::diff_bytes;
    use crate::types::DiffOptions;

    fn hunks(old: &[u8], new: &[u8], ws: bool) -> Vec<Hunk> {
        diff_bytes(
            old,
            new,
            &DiffOptions {
                ignore_whitespace: ws,
                context: 1,
            },
        )
    }

    const OLD: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
    const NEW: &str = "a\nB\nc\nd\ne\nf\nG\ng\nh\ni\nj\nk\n";

    #[test]
    fn reverting_the_middle_hunk_leaves_the_others() {
        let h = hunks(OLD.as_bytes(), NEW.as_bytes(), false);
        assert_eq!(h.len(), 3);
        let out = revert(OLD.as_bytes(), NEW.as_bytes(), &h[1]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\n"
        );
    }

    #[test]
    fn reverting_every_hunk_gives_the_old_file() {
        let (mut cur, old) = (NEW.as_bytes().to_vec(), OLD.as_bytes());
        while let Some(h) = hunks(old, &cur, false).first().cloned() {
            cur = revert(old, &cur, &h).unwrap();
        }
        assert_eq!(cur, old);
    }

    #[test]
    fn crlf_and_invalid_utf8_outside_the_hunk_are_kept() {
        let old = b"one\r\ntwo\r\nthree\r\n\xff\xfe tail\r\n".as_slice();
        let new = b"one\r\nTWO\r\nthree\r\n\xff\xfe tail\r\n".as_slice();
        let h = hunks(old, new, false);
        assert_eq!(h.len(), 1);
        assert_eq!(revert(old, new, &h[0]).unwrap(), old);
    }

    #[test]
    fn whitespace_only_context_keeps_the_working_text() {
        let old = b"x  =  1\nold\nz\n".as_slice();
        let new = b"x = 1\nnew\nz\n".as_slice();
        let h = hunks(old, new, true);
        assert_eq!(h.len(), 1);
        assert_eq!(revert(old, new, &h[0]).unwrap(), b"x = 1\nold\nz\n");
    }

    #[test]
    fn insertion_at_start_and_deletion_at_end() {
        let old = b"a\nb\n".as_slice();
        let new = b"new\na\n".as_slice();
        let h = hunks(old, new, false);
        let mut cur = new.to_vec();
        for h in h.iter().rev() {
            cur = revert(old, &cur, h).unwrap();
        }
        assert_eq!(cur, old);
    }

    #[test]
    fn utf16_le_is_edited_in_place() {
        let enc = |s: &str| {
            let mut v = vec![0xFF, 0xFE];
            s.encode_utf16()
                .for_each(|u| v.extend_from_slice(&u.to_le_bytes()));
            v
        };
        let (old, new) = (enc("a\r\nb\r\nc\r\n"), enc("a\r\nB\r\nc\r\n"));
        let h = hunks(&old, &new, false);
        assert_eq!(revert(&old, &new, &h[0]).unwrap(), old);
    }

    #[test]
    fn a_hunk_from_another_file_is_rejected() {
        let h = hunks(OLD.as_bytes(), NEW.as_bytes(), false);
        assert!(revert(b"x\n", b"y\n", &h[2]).is_err());
    }
}
