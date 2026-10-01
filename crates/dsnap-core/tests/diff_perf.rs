//! Line-diff timing and quality (DSNA-40, spec: a diff of a file under 1 MB opens in
//! < 200 ms).
//!
//! The debug test uses a generous bound so it is stable on slow machines. The release bound
//! is `#[ignore]`d and run by CI with `cargo test --release --test diff_perf -- --ignored`.
//! Every case also checks that the diff stays close to the edit that produced it, so a
//! deadline fallback that reports the whole file as replaced fails the test.
#![allow(clippy::unwrap_used, clippy::panic)] // test code

#[path = "support/diff_input.rs"]
mod diff_input;

use std::time::{Duration, Instant};

use diff_input::Edited;
use dsnap_core::diff::content::build_file_diff;
use dsnap_core::diff::lines::line_counts_bytes;
use dsnap_core::{BlobHash, DiffBody, DiffOptions, Entry, EntryKind, RelPath};

fn entry(bytes: &[u8]) -> Entry {
    Entry {
        path: RelPath::new("src/big.rs").unwrap(),
        kind: EntryKind::File,
        blob: Some(BlobHash::of(bytes)),
        size: bytes.len() as u64,
        mtime_ns: 0,
        readonly: false,
    }
}

struct Case {
    name: String,
    old: Vec<u8>,
    new: Edited,
}

fn cases() -> Vec<Case> {
    let source = diff_input::source();
    assert!(source.len() < 1024 * 1024 + 128);
    let mut cases: Vec<Case> = [1, 10, 50]
        .into_iter()
        .map(|percent| Case {
            name: format!("source 1 MiB, {percent}%"),
            new: diff_input::modified(&source, percent),
            old: source.clone(),
        })
        .collect();
    // Duplicate-heavy data files (DSNA-40 review round 1): few or no unique lines.
    for (rows, distinct) in [(100_000, 3000), (120_000, 50), (60_000, 100), (150_000, 4)] {
        let old = diff_input::data_rows(rows, distinct);
        assert!(old.len() < 1024 * 1024, "{} bytes", old.len());
        for percent in [1, 10] {
            cases.push(Case {
                name: format!("data {rows} rows / {distinct} values, {percent}%"),
                new: diff_input::data_modified(&old, percent),
                old: old.clone(),
            });
        }
    }
    cases
}

/// Best end-to-end `build_file_diff` time per case (best of `runs`), after checking the
/// diff's size against the real edit.
fn timings(runs: usize) -> Vec<(String, Duration)> {
    let opts = DiffOptions::default();
    let path = RelPath::new("src/big.rs").unwrap();
    cases()
        .into_iter()
        .map(|case| {
            let (old, new) = (&case.old, &case.new.bytes);
            let (old_entry, new_entry) = (entry(old), entry(new));
            let mut best = Duration::MAX;
            for _ in 0..runs {
                let start = Instant::now();
                let diff = build_file_diff(
                    &path,
                    Some((&old_entry, old)),
                    Some((&new_entry, new)),
                    &opts,
                );
                best = best.min(start.elapsed());
                let DiffBody::Text { hunks } = diff.body else {
                    panic!("expected a text diff")
                };
                assert!(!hunks.is_empty());
            }
            // The diff may beat the edit (a replacement can equal a nearby row) but must not
            // be much larger than it.
            let (added, removed) = line_counts_bytes(old, new, &opts);
            let limit = |real: u32| real + real / 2 + 10;
            eprintln!(
                "{}: {best:?}, +{added} -{removed} (edit +{} -{})",
                case.name, case.new.added, case.new.removed
            );
            assert!(
                added <= limit(case.new.added) && removed <= limit(case.new.removed),
                "{}: diff +{added} -{removed} vs edit +{} -{}",
                case.name,
                case.new.added,
                case.new.removed
            );
            (case.name, best)
        })
        .collect()
}

#[test]
fn diff_under_one_second_in_any_build() {
    for (name, t) in timings(1) {
        assert!(t < Duration::from_secs(1), "{name}: {t:?}");
    }
}

#[test]
#[ignore = "release-only timing; CI runs it with --release -- --ignored"]
fn diff_under_200_ms_in_release() {
    for (name, t) in timings(3) {
        assert!(t < Duration::from_millis(200), "{name}: {t:?}");
    }
}
