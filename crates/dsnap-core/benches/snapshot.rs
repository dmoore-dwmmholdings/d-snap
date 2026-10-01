//! Criterion benchmark for snapshots and status (DSNA-52): a 10,000-file project, 49 files
//! changed before each measured call. The baseline snapshot (every file stored) runs once in
//! setup and is not measured.
//!
//! Run with `cargo bench -p dsnap-core --bench snapshot`.
#![allow(missing_docs, clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dsnap_core::db::Db;
use dsnap_core::{ProjectSettings, SnapshotOptions};
use dsnap_test_support::{FixtureProject, TestHome, generate_tree, touch_n};

const FILES: usize = 10_000;
const CHANGED: usize = 49;

fn snapshot(c: &mut Criterion) {
    let fx = FixtureProject::new().build();
    generate_tree(fx.root(), FILES, 52);
    let home = TestHome::new();
    let dsnap = home.open();
    let db = Db::open(&home.path().join("dsnap.db")).unwrap();
    let project = db
        .insert_project("bench", fx.root(), &ProjectSettings::default())
        .unwrap();
    dsnap.snapshot(project, SnapshotOptions::default()).unwrap();

    let mut group = c.benchmark_group("snapshot_10k_files");
    group.sample_size(10);
    group.bench_function("snapshot_49_changed", |b| {
        b.iter_batched(
            || touch_n(fx.root(), CHANGED),
            |_| {
                let r = dsnap.snapshot(project, SnapshotOptions::default()).unwrap();
                black_box(r.version.unwrap());
            },
            BatchSize::PerIteration,
        )
    });
    group.bench_function("status_49_changed", |b| {
        b.iter_batched(
            || touch_n(fx.root(), CHANGED),
            |_| black_box(dsnap.status(project).unwrap()),
            BatchSize::PerIteration,
        )
    });
    group.bench_function("status_clean", |b| {
        dsnap.snapshot(project, SnapshotOptions::default()).unwrap();
        b.iter(|| black_box(dsnap.status(project).unwrap()))
    });
    group.finish();
}

criterion_group!(benches, snapshot);
criterion_main!(benches);
