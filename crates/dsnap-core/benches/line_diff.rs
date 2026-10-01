//! Criterion benchmark for the line diff (DSNA-40): `build_file_diff` end to end on a 1 MiB
//! source file with 1%, 10% and 50% of lines changed. Blob reads are excluded.
//!
//! Run with `cargo bench -p dsnap-core --bench line_diff`.
#![allow(missing_docs, clippy::unwrap_used)]

#[path = "../tests/support/diff_input.rs"]
mod diff_input;

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use dsnap_core::diff::content::build_file_diff;
use dsnap_core::{BlobHash, DiffOptions, Entry, EntryKind, RelPath};

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

fn line_diff(c: &mut Criterion) {
    let old = diff_input::source();
    let old_entry = entry(&old);
    let path = RelPath::new("src/big.rs").unwrap();
    let mut group = c.benchmark_group("build_file_diff_1mib");
    group.sample_size(20);
    group.throughput(Throughput::Bytes(old.len() as u64));
    for percent in [1u32, 10, 50] {
        let new = diff_input::modified(&old, percent);
        let new_entry = entry(&new);
        for ignore_whitespace in [false, true] {
            let opts = DiffOptions {
                ignore_whitespace,
                ..DiffOptions::default()
            };
            let id = if ignore_whitespace {
                "ignore_ws"
            } else {
                "exact"
            };
            group.bench_with_input(
                BenchmarkId::new(id, format!("{percent}%")),
                &opts,
                |b, o| {
                    b.iter(|| {
                        build_file_diff(
                            black_box(&path),
                            Some((&old_entry, black_box(&old))),
                            Some((&new_entry, black_box(&new))),
                            o,
                        )
                    })
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, line_diff);
criterion_main!(benches);
