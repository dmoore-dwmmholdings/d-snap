//! Snapshot and status timing (DSNA-52, spec: a 10,000-file project with < 50 changes
//! snapshots in < 1 s on SSD; `status()` runs often for the badge, target < 500 ms).
//!
//! Release only: `#[ignore]`d and run by CI with
//! `cargo test --release --test snapshot_perf -- --ignored --nocapture`.
#![allow(clippy::unwrap_used)] // test code

#[path = "support/project.rs"]
mod project;

use std::time::{Duration, Instant};

use dsnap_test_support::{FixtureProject, generate_tree, touch_n};

use project::{Env, snap};

const FILES: usize = 10_000;
const CHANGED: usize = 49;
const ROUNDS: usize = 5;

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

#[test]
#[ignore = "release-only timing; run with --release -- --ignored"]
fn snapshot_10k_files_with_49_changes_under_1s_and_status_under_500ms() {
    let fx = FixtureProject::new().build();
    generate_tree(fx.root(), FILES, 52);
    let env = Env::new(fx.root());

    let t = Instant::now();
    let first = snap(&env, None).version.unwrap();
    eprintln!("first snapshot ({FILES} files): {:?}", t.elapsed());
    assert_eq!(first.counts.added as usize, FILES);

    let mut snaps = Vec::new();
    let mut statuses = Vec::new();
    for _ in 0..ROUNDS {
        touch_n(fx.root(), CHANGED);

        let t = Instant::now();
        let st = env.dsnap.status(env.project).unwrap();
        statuses.push(t.elapsed());
        assert_eq!(st.len(), CHANGED);

        let t = Instant::now();
        let v = snap(&env, None).version.unwrap();
        snaps.push(t.elapsed());
        assert_eq!(v.counts.modified as usize, CHANGED);
    }
    let t = Instant::now();
    assert!(env.dsnap.status(env.project).unwrap().is_empty());
    let clean = t.elapsed();

    let (snap_med, status_med) = (median(snaps.clone()), median(statuses.clone()));
    eprintln!("snapshot with {CHANGED} changes: median {snap_med:?} of {snaps:?}");
    eprintln!("status with {CHANGED} changes: median {status_med:?} of {statuses:?}");
    eprintln!("status, clean tree: {clean:?}");
    assert!(snap_med < Duration::from_secs(1), "snapshot {snap_med:?}");
    assert!(
        status_med < Duration::from_millis(500),
        "status {status_med:?}"
    );
}
