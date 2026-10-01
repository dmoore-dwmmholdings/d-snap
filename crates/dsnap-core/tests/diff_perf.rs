//! Line-diff timing (DSNA-40, spec: a diff of a file under 1 MB opens in < 200 ms).
//!
//! The debug test uses a generous bound so it is stable on slow machines. The release bound
//! is `#[ignore]`d and run by CI with `cargo test --release --test diff_perf -- --ignored`.
#![allow(clippy::unwrap_used, clippy::panic)] // test code

#[path = "support/diff_input.rs"]
mod diff_input;

use std::time::{Duration, Instant};

use dsnap_core::diff::content::build_file_diff;
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

/// Slowest end-to-end `build_file_diff` over the 1%, 10% and 50% cases (best of `runs`).
fn worst_case(runs: usize) -> Vec<(u32, Duration)> {
    let old = diff_input::source();
    assert!(old.len() < 1024 * 1024 + 128);
    let path = RelPath::new("src/big.rs").unwrap();
    let old_entry = entry(&old);
    [1, 10, 50]
        .into_iter()
        .map(|percent| {
            let new = diff_input::modified(&old, percent);
            let new_entry = entry(&new);
            let mut best = Duration::MAX;
            for _ in 0..runs {
                let start = Instant::now();
                let diff = build_file_diff(
                    &path,
                    Some((&old_entry, &old)),
                    Some((&new_entry, &new)),
                    &DiffOptions::default(),
                );
                best = best.min(start.elapsed());
                let DiffBody::Text { hunks } = diff.body else {
                    panic!("expected a text diff")
                };
                assert!(!hunks.is_empty());
            }
            eprintln!("{percent}% changed: {best:?}");
            (percent, best)
        })
        .collect()
}

#[test]
fn one_mb_diff_under_one_second_in_any_build() {
    for (percent, t) in worst_case(1) {
        assert!(t < Duration::from_secs(1), "{percent}%: {t:?}");
    }
}

#[test]
#[ignore = "release-only timing; CI runs it with --release -- --ignored"]
fn one_mb_diff_under_200_ms_in_release() {
    for (percent, t) in worst_case(3) {
        assert!(t < Duration::from_millis(200), "{percent}%: {t:?}");
    }
}
