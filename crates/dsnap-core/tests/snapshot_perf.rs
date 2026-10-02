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
    let first_time = t.elapsed();
    eprintln!("first snapshot ({FILES} files): {first_time:?}");
    // DSNA-110: new blobs go through one store batch.
    assert!(
        first_time < Duration::from_secs(15),
        "first snapshot of {FILES} files took {first_time:?}"
    );
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

    diagnose(&env);

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

/// Versions of history for [`snapshot_with_200_versions_of_history_under_1s`] (the default
/// retention, DSNA-3).
const HISTORY: usize = 200;

/// DSNA-101: the snapshot target must hold with a full retention window of history, not only
/// on a fresh database. History is inserted straight into the index (copies of the first
/// version, 2M entry rows), which is what made `insert_version` grow before schema v3.
#[test]
#[ignore = "release-only timing; run with --release -- --ignored"]
fn snapshot_with_200_versions_of_history_under_1s() {
    use dsnap_core::db::NewVersion;
    use dsnap_core::store::Store;

    let fx = FixtureProject::new().build();
    generate_tree(fx.root(), FILES, 52);
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();

    let latest = env.db.latest_version(env.project).unwrap().unwrap();
    let entries = env.db.entries(latest.id).unwrap();
    let store = Store::open(env.home.path().join("objects")).unwrap();
    let t = Instant::now();
    for i in 1..HISTORY {
        let nv = NewVersion {
            project_id: env.project,
            label: format!("history {i}"),
            created_at_ms: latest.created_at_ms,
            kind: dsnap_core::VersionKind::Auto,
            unstable: false,
            counts: dsnap_core::ChangeCounts::default(),
            entries: entries.clone(),
            new_blobs: Vec::new(),
        };
        env.db.insert_version(&nv, &store).unwrap();
    }
    eprintln!(
        "{HISTORY} versions of history inserted in {:?}",
        t.elapsed()
    );

    let mut snaps = Vec::new();
    for _ in 0..ROUNDS {
        touch_n(fx.root(), CHANGED);
        let t = Instant::now();
        let v = snap(&env, None).version.unwrap();
        snaps.push(t.elapsed());
        assert_eq!(v.counts.modified as usize, CHANGED);
    }
    let snap_med = median(snaps.clone());
    eprintln!(
        "snapshot with {CHANGED} changes after {HISTORY} versions: median {snap_med:?} of {snaps:?}"
    );
    assert!(snap_med < Duration::from_secs(1), "snapshot {snap_med:?}");
}

/// Print where a snapshot's time goes beyond the walk and hashing that `status()` covers:
/// the index insert of a 10k-entry version, and storing 49 new blobs. Informational only.
fn diagnose(env: &Env) {
    use dsnap_core::db::NewVersion;
    use dsnap_core::store::Store;
    use rayon::prelude::*;

    let latest = env.db.latest_version(env.project).unwrap().unwrap();
    let entries = env.db.entries(latest.id).unwrap();
    let store = Store::open(env.home.path().join("objects")).unwrap();
    let nv = NewVersion {
        project_id: env.project,
        label: "diagnose".into(),
        created_at_ms: 0,
        kind: dsnap_core::VersionKind::Manual,
        unstable: false,
        counts: dsnap_core::ChangeCounts::default(),
        entries,
        new_blobs: Vec::new(),
    };
    let t = Instant::now();
    env.db.insert_version(&nv, &store).unwrap();
    eprintln!(
        "diagnose: insert_version of {} entries: {:?}",
        nv.entries.len(),
        t.elapsed()
    );

    let blobs: Vec<Vec<u8>> = (0..CHANGED)
        .map(|i| {
            format!(
                "diagnose blob {i} {:?}
",
                Instant::now()
            )
            .repeat(20)
            .into_bytes()
        })
        .collect();
    let t = Instant::now();
    blobs.par_iter().for_each(|b| {
        store.put(b).unwrap();
    });
    eprintln!(
        "diagnose: {CHANGED} new blobs stored in parallel: {:?}",
        t.elapsed()
    );
}
