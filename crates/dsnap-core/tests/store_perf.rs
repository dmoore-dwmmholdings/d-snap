//! Store write timing for a project's first snapshot (DSNA-102): 10,000 small new files must
//! be stored durably within the 15 s first-snapshot budget on CI windows-latest. Storing is
//! almost all of a first snapshot's time there (walk, hashing and `insert_version` of 10k
//! entries take about 1.5 s).
//!
//! The store is under the temp dir, on `C:` like the real data dir (`%LOCALAPPDATA%`). That
//! volume is much slower than the runner's `D:` and creates 8.3 short names, which makes each
//! 62-character object name cost more.
//!
//! The runner's disk is noisy (the same run varies 12–17 s), so the bound applies to the best
//! of [`ROUNDS`] rounds, each into a new store.
//!
//! `#[ignore]`d; CI runs it in release with
//! `cargo test --release --test store_perf -- --ignored --nocapture`.
#![allow(clippy::unwrap_used, clippy::panic)] // test code

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dsnap_core::store::Store;
use dsnap_test_support::generate_tree;
use rayon::prelude::*;

/// Budget for storing the tree: the first-snapshot bound of DSNA-102.
const BUDGET: Duration = Duration::from_secs(15);

const FILES: usize = 10_000;

/// Rounds timed; the best one must meet the budget.
const ROUNDS: usize = 2;

/// Files used to time the one-by-one `put_file` path, for comparison only.
const SINGLE_FILES: usize = 1_000;

/// Same width as the snapshot engine's store pool (`max(cpus, 16)`, DSNA-52).
fn pool() -> rayon::ThreadPool {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    rayon::ThreadPoolBuilder::new()
        .num_threads(cpus.max(16))
        .build()
        .unwrap()
}

/// Store every file of `files` into a new store under `dir` through one batch, as a first
/// snapshot does; returns (time to stage, total time).
fn store_all(dir: PathBuf, files: &[PathBuf], pool: &rayon::ThreadPool) -> (Duration, Duration) {
    let store = Store::open(dir).unwrap();
    let start = Instant::now();
    let batch = store.batch();
    let infos: Vec<_> = pool.install(|| {
        files
            .par_iter()
            .map(|p| batch.put_file(p).unwrap())
            .collect()
    });
    let staged = start.elapsed();
    batch.commit().unwrap();
    let total = start.elapsed();
    assert!(infos.iter().all(|i| store.contains(&i.hash)));
    (staged, total)
}

#[test]
#[ignore = "release timing test; run with --release -- --ignored"]
fn first_snapshot_store_of_10k_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("project");
    let files: Vec<PathBuf> = generate_tree(&root, FILES, 102)
        .iter()
        .map(|p| p.to_path(&root))
        .collect();
    let pool = pool();

    let mut best = Duration::MAX;
    for round in 0..ROUNDS {
        let (staged, total) = store_all(tmp.path().join(format!("objects-{round}")), &files, &pool);
        eprintln!(
            "store {FILES} new files, round {round}: {total:.2?} (stage {staged:.2?}, commit {:.2?})",
            total - staged
        );
        best = best.min(total);
    }

    let single = Store::open(tmp.path().join("objects-single")).unwrap();
    let start = Instant::now();
    pool.install(|| {
        files[..SINGLE_FILES].par_iter().for_each(|p| {
            single.put_file(p).unwrap();
        })
    });
    eprintln!(
        "for comparison, put_file one by one: {:.2?} for {SINGLE_FILES} files",
        start.elapsed()
    );
    assert!(
        best < BUDGET,
        "storing {FILES} files took {best:.2?} at best (budget {BUDGET:?})"
    );
}
