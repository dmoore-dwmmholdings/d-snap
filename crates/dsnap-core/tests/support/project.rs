//! A `Dsnap` on a temporary home with one project registered (Chain K tests).
//!
//! Projects are inserted through a second `Db` handle on the same database file, so these
//! tests do not depend on `Dsnap::add_project` (Chain L).
#![allow(dead_code, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use dsnap_core::db::Db;
use dsnap_core::{Dsnap, ProjectId, ProjectSettings, RelPath, SnapshotOptions, SnapshotReport};
use dsnap_test_support::TestHome;

pub struct Env {
    pub dsnap: Dsnap,
    /// Second handle on the same database, for setup and inspection.
    pub db: Db,
    pub project: ProjectId,
    pub home: TestHome,
}

impl Env {
    pub fn new(root: &Path) -> Self {
        Self::with_settings(root, ProjectSettings::default())
    }

    pub fn with_settings(root: &Path, settings: ProjectSettings) -> Self {
        let home = TestHome::new();
        let dsnap = home.open();
        let db = Db::open(&home.path().join("dsnap.db")).unwrap();
        let project = db.insert_project("proj", root, &settings).unwrap();
        Self {
            dsnap,
            db,
            project,
            home,
        }
    }
}

pub fn rp(s: &str) -> RelPath {
    RelPath::new(s).unwrap()
}

pub fn snap(env: &Env, label: Option<&str>) -> SnapshotReport {
    env.dsnap
        .snapshot(
            env.project,
            SnapshotOptions {
                label: label.map(str::to_owned),
                ..SnapshotOptions::default()
            },
        )
        .unwrap()
}

pub fn versions(env: &Env) -> usize {
    env.db.list_versions(env.project).unwrap().len()
}

/// Number of blob files in the store.
pub fn store_files(env: &Env) -> usize {
    env.dsnap
        .home()
        .objects_dir()
        .read_dir()
        .unwrap()
        .flatten()
        .filter(|d| d.file_type().unwrap().is_dir())
        .map(|d| fs::read_dir(d.path()).unwrap().count())
        .sum()
}

/// Write new content and move the mtime forward, so size+mtime detection always sees it.
pub fn rewrite(abs: &Path, bytes: &[u8]) {
    let old = fs::metadata(abs).and_then(|m| m.modified()).ok();
    fs::write(abs, bytes).unwrap();
    let base = old.map_or(SystemTime::now(), |o| o.max(SystemTime::now()));
    filetime::set_file_mtime(
        abs,
        filetime::FileTime::from_system_time(base + Duration::from_secs(2)),
    )
    .unwrap();
}
