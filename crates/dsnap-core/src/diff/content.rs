//! Per-file diff: text, binary, image or too large. Owner: Chain G (DSNA-10).
//!
//! [`classify`] and [`build_file_diff`] are pure functions over bytes already in memory. The
//! [`Dsnap::file_diff`] facade that loads those bytes belongs to Chain K (DSNA-51).

use crate::diff::lines;
use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{
    BlobHash, DiffBody, DiffOptions, Entry, EntryKind, FileDiff, ProjectId, RelPath, VersionId,
    VersionRef,
};

/// Text files above this size (on either side) are reported as [`DiffBody::TooLarge`]
/// instead of being line-diffed. Binary files and images have no limit: they are not diffed.
pub const MAX_TEXT_DIFF_BYTES: u64 = 5 * 1024 * 1024;

/// How many leading bytes [`classify`] inspects.
pub const SNIFF_BYTES: usize = 8 * 1024;

/// How a file's content is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentClass {
    /// Valid text: line diff.
    Text,
    /// Image with this MIME type: side-by-side preview.
    Image(String),
    /// Anything else: size and hash only.
    Binary,
}

/// Classify content by its path and bytes. Only the first [`SNIFF_BYTES`] bytes are read, so
/// a prefix of the file is enough.
///
/// Images are recognised by magic number (PNG, JPEG, GIF, WebP, BMP, ICO), and SVG by a
/// `.svg` extension plus an `<svg` tag near the start. Otherwise content with a UTF-16 byte
/// order mark that decodes plausibly (see [`plausible_utf16`]) is text, content with a NUL
/// byte is binary, and everything else is text.
pub fn classify(path: &RelPath, bytes: &[u8]) -> ContentClass {
    let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
    if let Some(mime) = image_mime(head) {
        return ContentClass::Image(mime.to_owned());
    }
    if plausible_utf16(head, bytes.len() > head.len()) {
        return ContentClass::Text;
    }
    if head.contains(&0) {
        return ContentClass::Binary;
    }
    if is_svg(path, head) {
        return ContentClass::Image("image/svg+xml".to_owned());
    }
    ContentClass::Text
}

fn image_mime(head: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if head.starts_with(PNG) {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else if head.len() >= 26 && head.starts_with(b"BM") && head[6..10] == [0, 0, 0, 0] {
        // "BM" alone is too weak (plain text can start with it); the reserved header fields
        // of a real bitmap are zero.
        Some("image/bmp")
    } else if is_ico(head) {
        Some("image/x-icon")
    } else {
        None
    }
}

/// ICONDIR (reserved 0, type 1, image count > 0) plus a sane first ICONDIRENTRY: reserved
/// byte 0, colour planes 0 or 1, and image data after the directory.
fn is_ico(head: &[u8]) -> bool {
    let [
        0,
        0,
        1,
        0,
        c0,
        c1,
        _,
        _,
        _,
        reserved,
        p0,
        p1,
        _,
        _,
        _,
        _,
        _,
        _,
        o0,
        o1,
        o2,
        o3,
        ..,
    ] = *head
    else {
        return false;
    };
    let count = u64::from(u16::from_le_bytes([c0, c1]));
    let planes = u16::from_le_bytes([p0, p1]);
    let offset = u64::from(u32::from_le_bytes([o0, o1, o2, o3]));
    count > 0 && reserved == 0 && planes <= 1 && offset >= 6 + 16 * count
}

/// True when `head` starts with a UTF-16 byte order mark and the code units after it look
/// like text: no NUL units, no C0 control characters other than tab, LF, VT, FF, CR and ESC,
/// and no unpaired surrogates (a high surrogate cut off by the end of the sniff window is
/// allowed when `truncated`). Binary data that happens to start with `FF FE` or `FE FF`
/// fails this and falls through to the NUL check.
pub fn plausible_utf16(head: &[u8], truncated: bool) -> bool {
    type Unit = fn([u8; 2]) -> u16;
    let (body, unit): (&[u8], Unit) = match head {
        [0xFF, 0xFE, rest @ ..] => (rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => (rest, u16::from_be_bytes),
        _ => return false,
    };
    let mut units = body.chunks_exact(2).map(|c| unit([c[0], c[1]]));
    while let Some(u) = units.next() {
        match u {
            0x0000..=0x0008 | 0x000E..=0x001A | 0x001C..=0x001F => return false,
            0xD800..=0xDBFF => match units.next() {
                Some(0xDC00..=0xDFFF) => {}
                None if truncated => {}
                _ => return false,
            },
            0xDC00..=0xDFFF => return false,
            _ => {}
        }
    }
    true
}

fn is_svg(path: &RelPath, head: &[u8]) -> bool {
    let has_ext = path
        .as_str()
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("svg"));
    if !has_ext {
        return false;
    }
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start_matches('\u{FEFF}').trim_start();
    trimmed.starts_with('<') && text.contains("<svg")
}

/// One side of a file comparison: its entry and its full content.
///
/// For a symlink entry the content is ignored and the link target is diffed as text; for a
/// directory entry it is ignored and treated as empty.
pub type DiffSide<'a> = (&'a Entry, &'a [u8]);

/// Build the diff of one file. `None` on a side means the file does not exist there (added or
/// deleted).
///
/// Rules: if every present side is an image, the result is [`DiffBody::Image`]. Otherwise, if
/// any side is binary or an image, it is [`DiffBody::Binary`]. Otherwise it is text: larger
/// than [`MAX_TEXT_DIFF_BYTES`] on either side gives [`DiffBody::TooLarge`], else a line diff
/// against empty text for a missing side.
pub fn build_file_diff(
    path: &RelPath,
    old: Option<DiffSide<'_>>,
    new: Option<DiffSide<'_>>,
    opts: &DiffOptions,
) -> FileDiff {
    let old = old.map(|(e, b)| Side::new(path, e, b));
    let new = new.map(|(e, b)| Side::new(path, e, b));
    let classes = [&old, &new];
    let present = || classes.iter().filter_map(|s| s.as_ref());

    let body = if let Some(mime) = all_images(present()) {
        DiffBody::Image {
            old: old.as_ref().map(Side::hash),
            new: new.as_ref().map(Side::hash),
            mime,
        }
    } else if present().any(|s| s.class != ContentClass::Text) {
        DiffBody::Binary {
            old_size: old.as_ref().map(Side::size),
            new_size: new.as_ref().map(Side::size),
            old_hash: old.as_ref().map(Side::hash),
            new_hash: new.as_ref().map(Side::hash),
        }
    } else {
        let size = present().map(Side::size).max().unwrap_or(0);
        if size > MAX_TEXT_DIFF_BYTES {
            DiffBody::TooLarge { size }
        } else {
            let old_bytes = old.as_ref().map_or(&[][..], |s| s.bytes);
            let new_bytes = new.as_ref().map_or(&[][..], |s| s.bytes);
            DiffBody::Text {
                hunks: lines::diff_bytes(old_bytes, new_bytes, opts),
            }
        }
    };
    FileDiff {
        path: path.clone(),
        body,
    }
}

/// MIME type when every present side is an image (new side's type wins), else `None`.
fn all_images<'s>(mut sides: impl Iterator<Item = &'s Side<'s>>) -> Option<String> {
    let mut mime = None;
    let all = sides.all(|s| match &s.class {
        ContentClass::Image(m) => {
            mime = Some(m.clone());
            true
        }
        _ => false,
    });
    if all { mime } else { None }
}

struct Side<'a> {
    entry: &'a Entry,
    bytes: &'a [u8],
    class: ContentClass,
}

impl<'a> Side<'a> {
    fn new(path: &RelPath, entry: &'a Entry, bytes: &'a [u8]) -> Self {
        let bytes = match &entry.kind {
            EntryKind::File => bytes,
            EntryKind::Symlink { target } => target.as_bytes(),
            EntryKind::Dir => &[],
        };
        let class = match entry.kind {
            EntryKind::File => classify(path, bytes),
            _ => ContentClass::Text,
        };
        Self {
            entry,
            bytes,
            class,
        }
    }

    fn size(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    /// The entry's stored hash, or the hash of the bytes for a not-yet-hashed working entry.
    fn hash(&self) -> BlobHash {
        self.entry.blob.unwrap_or_else(|| BlobHash::of(self.bytes))
    }
}

impl Dsnap {
    /// Diff one file between `from` (default: version before `to`; for the working tree, the
    /// latest version) and `to`.
    ///
    /// When `path` is the new path of a rename, the old side is loaded from the rename's old
    /// path. Working-tree content is read from disk. [`crate::Error::NotFound`] if `path` is
    /// on neither side. Implemented in `status.rs` (Chain K).
    pub fn file_diff(
        &self,
        project: ProjectId,
        from: Option<VersionId>,
        to: VersionRef,
        path: &RelPath,
        opts: &DiffOptions,
    ) -> Result<FileDiff> {
        self.file_diff_impl(project, from, to, path, opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    fn file(path: &str, bytes: &[u8]) -> Entry {
        Entry {
            path: p(path),
            kind: EntryKind::File,
            blob: Some(BlobHash::of(bytes)),
            size: bytes.len() as u64,
            mtime_ns: 0,
            readonly: false,
        }
    }

    fn image(mime: &str) -> ContentClass {
        ContentClass::Image(mime.to_owned())
    }

    fn png() -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend([0u8; 32]);
        b
    }

    #[test]
    fn classifies_text_and_binary() {
        let a = p("a.txt");
        assert_eq!(classify(&a, b"hello\nworld\n"), ContentClass::Text);
        assert_eq!(classify(&a, b""), ContentClass::Text);
        assert_eq!(
            classify(&a, b"\xFF\xFE invalid utf8 \x80"),
            ContentClass::Text
        );
        assert_eq!(classify(&a, b"abc\0def"), ContentClass::Binary);
        // UTF-16 text is full of NUL bytes but has a BOM.
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("hi\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(classify(&a, &utf16), ContentClass::Text);
        // Only the first 8 KiB is sniffed.
        let mut late_nul = vec![b'a'; SNIFF_BYTES];
        late_nul.push(0);
        assert_eq!(classify(&a, &late_nul), ContentClass::Text);
        late_nul[SNIFF_BYTES - 1] = 0;
        assert_eq!(classify(&a, &late_nul), ContentClass::Binary);
    }

    fn utf16(be: bool, s: &str) -> Vec<u8> {
        let mut b = if be {
            vec![0xFE, 0xFF]
        } else {
            vec![0xFF, 0xFE]
        };
        for u in s.encode_utf16() {
            b.extend(if be { u.to_be_bytes() } else { u.to_le_bytes() });
        }
        b
    }

    #[test]
    fn utf16_bom_needs_plausible_text() {
        let a = p("a.txt");
        for be in [false, true] {
            assert_eq!(
                classify(&a, &utf16(be, "héllo\r\n\tworld 🌍\n")),
                ContentClass::Text
            );
            // NUL code unit, C0 control, lone surrogates: binary after a BOM.
            let mut nul = utf16(be, "ab");
            nul.extend([0, 0]);
            assert_eq!(classify(&a, &nul), ContentClass::Binary, "be={be}");
            assert_eq!(classify(&a, &utf16(be, "a\u{1}b")), ContentClass::Binary);
            let lone_low: Vec<u8> = if be {
                vec![0xFE, 0xFF, 0xDC, 0x00]
            } else {
                vec![0xFF, 0xFE, 0x00, 0xDC]
            };
            assert!(!plausible_utf16(&lone_low, false));
            let high_then_a: Vec<u8> = if be {
                vec![0xFE, 0xFF, 0xD8, 0x3D, 0x00, 0x61]
            } else {
                vec![0xFF, 0xFE, 0x3D, 0xD8, 0x61, 0x00]
            };
            assert!(!plausible_utf16(&high_then_a, false));
            // A high surrogate cut off by the sniff window is fine; at the true end it is not.
            let cut = &high_then_a[..4];
            assert!(plausible_utf16(cut, true));
            assert!(!plausible_utf16(cut, false));
        }
        // Binary that happens to start with a BOM (e.g. random bytes with NULs).
        let mut blob = vec![0xFF, 0xFE, 0x10, 0x00, 0x00, 0x00, 0x00, 0x7F];
        blob.extend([0u8; 32]);
        assert_eq!(classify(&a, &blob), ContentClass::Binary);
        // A long UTF-16 file cut mid-pair at 8 KiB stays text.
        let mut long = utf16(false, &"x".repeat(SNIFF_BYTES / 2 - 2));
        long.extend("🌍 tail".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(long[..SNIFF_BYTES].len(), SNIFF_BYTES);
        assert_eq!(classify(&a, &long), ContentClass::Text);
    }

    #[test]
    fn classifies_images_by_magic_number() {
        let x = p("no_extension");
        assert_eq!(classify(&x, &png()), image("image/png"));
        assert_eq!(
            classify(&x, b"\xFF\xD8\xFF\xE0\0\x10JFIF"),
            image("image/jpeg")
        );
        assert_eq!(classify(&x, b"GIF89a\x01\0\x01\0"), image("image/gif"));
        assert_eq!(classify(&x, b"GIF87a\x01\0\x01\0"), image("image/gif"));
        assert_eq!(classify(&x, b"RIFF\x24\0\0\0WEBPVP8 "), image("image/webp"));
        let mut bmp = b"BM\x3a\0\0\0\0\0\0\0\x36\0\0\0".to_vec();
        bmp.extend([0u8; 16]);
        assert_eq!(classify(&x, &bmp), image("image/bmp"));
        // ICONDIR (1 image) + ICONDIRENTRY: 16x16, 0 colours, reserved 0, 1 plane, 32 bpp,
        // 0x68 bytes at offset 22.
        let ico = b"\0\0\x01\0\x01\0\x10\x10\0\0\x01\0\x20\0\x68\0\0\0\x16\0\0\0";
        assert_eq!(classify(&x, ico), image("image/x-icon"));
        // Same 4-byte prefix without a sane directory entry: not an icon.
        assert_eq!(
            classify(&x, b"\0\0\x01\0\x01\0\x10\x10\0\0"),
            ContentClass::Binary
        );
        let mut bad = ico.to_vec();
        bad[9] = 7; // reserved byte
        assert_eq!(classify(&x, &bad), ContentClass::Binary);
        let mut bad = ico.to_vec();
        bad[18] = 4; // image data inside the directory
        assert_eq!(classify(&x, &bad), ContentClass::Binary);
        // Text that merely starts with "BM" stays text.
        assert_eq!(
            classify(&x, b"BMW owners manual, chapter one: getting started\n"),
            ContentClass::Text
        );
    }

    #[test]
    fn classifies_svg_by_extension_and_sniff() {
        let svg = b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>\n";
        assert_eq!(classify(&p("icon.svg"), svg), image("image/svg+xml"));
        assert_eq!(
            classify(&p("dir/ICON.SVG"), b"<svg/>"),
            image("image/svg+xml")
        );
        // Same bytes without the extension: text.
        assert_eq!(classify(&p("icon.xml"), svg), ContentClass::Text);
        // Extension but not SVG content: text.
        assert_eq!(
            classify(&p("broken.svg"), b"not svg at all"),
            ContentClass::Text
        );
    }

    #[test]
    fn text_diff_for_modified_file() {
        let (o, n) = (b"a\nb\n".as_slice(), b"a\nc\n".as_slice());
        let d = build_file_diff(
            &p("f.txt"),
            Some((&file("f.txt", o), o)),
            Some((&file("f.txt", n), n)),
            &DiffOptions::default(),
        );
        assert_eq!(d.path, p("f.txt"));
        let DiffBody::Text { hunks } = d.body else {
            panic!("expected text, got {:?}", d.body)
        };
        assert_eq!(hunks.len(), 1);
        assert_eq!((hunks[0].old_len, hunks[0].new_len), (2, 2));
    }

    #[test]
    fn added_and_deleted_text_files() {
        let bytes = b"x\ny\n".as_slice();
        let e = file("f.txt", bytes);
        let opts = DiffOptions::default();

        let added = build_file_diff(&p("f.txt"), None, Some((&e, bytes)), &opts);
        let DiffBody::Text { hunks } = added.body else {
            panic!("expected text")
        };
        assert_eq!((hunks[0].old_start, hunks[0].old_len), (0, 0));
        assert_eq!((hunks[0].new_start, hunks[0].new_len), (1, 2));

        let deleted = build_file_diff(&p("f.txt"), Some((&e, bytes)), None, &opts);
        let DiffBody::Text { hunks } = deleted.body else {
            panic!("expected text")
        };
        assert_eq!((hunks[0].old_len, hunks[0].new_len), (2, 0));

        let neither = build_file_diff(&p("f.txt"), None, None, &opts);
        assert_eq!(neither.body, DiffBody::Text { hunks: vec![] });
    }

    #[test]
    fn binary_reports_sizes_and_hashes() {
        let o = b"\0\x01\x02".as_slice();
        let n = b"\0\x01\x02\x03\x04".as_slice();
        let (eo, en) = (file("b.bin", o), file("b.bin", n));
        let d = build_file_diff(
            &p("b.bin"),
            Some((&eo, o)),
            Some((&en, n)),
            &DiffOptions::default(),
        );
        assert_eq!(
            d.body,
            DiffBody::Binary {
                old_size: Some(3),
                new_size: Some(5),
                old_hash: eo.blob,
                new_hash: en.blob,
            }
        );

        // Deleted binary: new side absent.
        let d = build_file_diff(&p("b.bin"), Some((&eo, o)), None, &DiffOptions::default());
        assert_eq!(
            d.body,
            DiffBody::Binary {
                old_size: Some(3),
                new_size: None,
                old_hash: eo.blob,
                new_hash: None,
            }
        );

        // Text replaced by binary: binary.
        let t = b"text\n".as_slice();
        let et = file("b.bin", t);
        let d = build_file_diff(
            &p("b.bin"),
            Some((&et, t)),
            Some((&en, n)),
            &DiffOptions::default(),
        );
        assert!(matches!(d.body, DiffBody::Binary { .. }));
    }

    #[test]
    fn images_and_unhashed_working_entries() {
        let o = png();
        let mut n = png();
        n.push(1);
        let eo = file("i.png", &o);
        let mut en = file("i.png", &n);
        // A working-tree entry may not be hashed yet: the hash is computed from the bytes.
        en.blob = None;
        let d = build_file_diff(
            &p("i.png"),
            Some((&eo, &o)),
            Some((&en, &n)),
            &DiffOptions::default(),
        );
        assert_eq!(
            d.body,
            DiffBody::Image {
                old: eo.blob,
                new: Some(BlobHash::of(&n)),
                mime: "image/png".into(),
            }
        );

        // Added image.
        let d = build_file_diff(&p("i.png"), None, Some((&eo, &o)), &DiffOptions::default());
        assert_eq!(
            d.body,
            DiffBody::Image {
                old: None,
                new: eo.blob,
                mime: "image/png".into(),
            }
        );

        // Image replaced by text: binary, not an image preview.
        let t = b"text\n".as_slice();
        let et = file("i.png", t);
        let d = build_file_diff(
            &p("i.png"),
            Some((&eo, &o)),
            Some((&et, t)),
            &DiffOptions::default(),
        );
        assert!(matches!(d.body, DiffBody::Binary { .. }));
    }

    #[test]
    fn too_large_threshold() {
        let limit = usize::try_from(MAX_TEXT_DIFF_BYTES).unwrap();
        let at_limit = vec![b'a'; limit];
        let over = vec![b'a'; limit + 1];
        let small = b"a\n".as_slice();
        let opts = DiffOptions::default();
        let (es, el, eo) = (
            file("t.txt", small),
            file("t.txt", &at_limit),
            file("t.txt", &over),
        );

        let d = build_file_diff(
            &p("t.txt"),
            Some((&es, small)),
            Some((&el, &at_limit)),
            &opts,
        );
        assert!(matches!(d.body, DiffBody::Text { .. }));

        let d = build_file_diff(&p("t.txt"), Some((&eo, &over)), Some((&es, small)), &opts);
        assert_eq!(
            d.body,
            DiffBody::TooLarge {
                size: MAX_TEXT_DIFF_BYTES + 1
            }
        );

        let d = build_file_diff(&p("t.txt"), None, Some((&eo, &over)), &opts);
        assert!(matches!(d.body, DiffBody::TooLarge { .. }));

        // Large binaries are not "too large": they are never line-diffed.
        let mut big_bin = over.clone();
        big_bin[0] = 0;
        let eb = file("t.txt", &big_bin);
        let d = build_file_diff(&p("t.txt"), None, Some((&eb, &big_bin)), &opts);
        assert!(matches!(d.body, DiffBody::Binary { .. }));
    }

    #[test]
    fn symlink_targets_diff_as_text() {
        let link = |target: &str| Entry {
            kind: EntryKind::Symlink {
                target: target.into(),
            },
            blob: None,
            ..file("l", b"")
        };
        let (a, b) = (link("old/target"), link("new/target"));
        let d = build_file_diff(
            &p("l"),
            Some((&a, b"ignored\0")),
            Some((&b, b"")),
            &DiffOptions::default(),
        );
        let DiffBody::Text { hunks } = d.body else {
            panic!("expected text")
        };
        let texts: Vec<&str> = hunks[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["old/target", "new/target"]);
    }
}
