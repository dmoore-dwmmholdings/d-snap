//! Unit tests for the index database.

use std::collections::HashSet;
use std::thread;

use rusqlite::TransactionBehavior;

use super::*;

fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dsnap.db");
    (dir, path)
}

fn table_names(db: &Db) -> Vec<String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

// ---- DSNA-30: schema, migrations, connection setup ----

#[test]
fn fresh_db_gets_schema_v1() {
    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    assert_eq!(schema::user_version(&db.conn()).unwrap(), 1);
    assert_eq!(schema::SCHEMA_VERSION, 1);
    assert_eq!(
        table_names(&db),
        ["blobs", "entries", "projects", "settings", "versions"]
    );
}

#[test]
fn in_memory_db_gets_schema_v1() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(schema::user_version(&db.conn()).unwrap(), 1);
}

#[test]
fn pragmas_are_set() {
    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    let conn = db.conn();
    let q = |sql: &str| -> String {
        conn.query_row(sql, [], |r| r.get::<_, rusqlite::types::Value>(0))
            .map(|v| match v {
                rusqlite::types::Value::Integer(i) => i.to_string(),
                rusqlite::types::Value::Text(t) => t,
                other => format!("{other:?}"),
            })
            .unwrap()
    };
    assert_eq!(q("PRAGMA journal_mode"), "wal");
    assert_eq!(q("PRAGMA foreign_keys"), "1");
    assert_eq!(q("PRAGMA busy_timeout"), "5000");
    assert_eq!(q("PRAGMA synchronous"), "1"); // NORMAL
}

#[test]
fn reopen_is_a_noop() {
    let (_dir, path) = temp_db();
    {
        let db = Db::open(&path).unwrap();
        db.conn()
            .execute(
                "INSERT INTO settings (key, value_json) VALUES ('probe', '1')",
                [],
            )
            .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(schema::user_version(&db.conn()).unwrap(), 1);
    let v: String = db
        .conn()
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'probe'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, "1");
}

#[test]
fn newer_schema_is_rejected() {
    let (_dir, path) = temp_db();
    {
        let db = Db::open(&path).unwrap();
        db.conn().pragma_update(None, "user_version", 99).unwrap();
    }
    let err = Db::open(&path).unwrap_err();
    assert!(matches!(err, crate::Error::Corrupt(_)), "{err}");
}

#[test]
fn concurrent_open_of_fresh_file_migrates_once() {
    let (_dir, path) = temp_db();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            thread::spawn(move || Db::open(&path).map(|_| ()))
        })
        .collect();
    for h in handles {
        h.join().unwrap().unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(schema::user_version(&db.conn()).unwrap(), 1);
}

#[test]
fn two_connections_write_under_contention_without_busy_errors() {
    let (_dir, path) = temp_db();
    Db::open(&path).unwrap();
    const PER_THREAD: i64 = 200;
    let handles: Vec<_> = (0..2)
        .map(|t| {
            let path = path.clone();
            thread::spawn(move || -> crate::Result<()> {
                let db = Db::open(&path)?;
                for i in 0..PER_THREAD {
                    let mut conn = db.conn();
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    tx.execute(
                        "INSERT INTO projects (name, root_path, settings_json, created_at_ms)
                         VALUES (?1, ?2, '{}', 0)",
                        (format!("p{t}-{i}"), format!("/r/{t}/{i}")),
                    )?;
                    tx.commit()?;
                }
                Ok(())
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap().unwrap();
    }
    let db = Db::open(&path).unwrap();
    let n: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2 * PER_THREAD);
}

// ---- DSNA-31: repository layer ----

use std::path::PathBuf;
use std::time::Instant;

use crate::types::{AutoSnapshot, EntryKind};

/// In-memory stand-in for the blob store (Chain C's `Store` is not on main yet).
#[derive(Default)]
struct FakeFiles {
    /// `contains` is always true (for tests that do not exercise the blob check).
    all_present: bool,
    present: std::sync::Mutex<HashSet<BlobHash>>,
    deleted: std::sync::Mutex<Vec<BlobHash>>,
}

impl FakeFiles {
    fn with(hashes: &[BlobHash]) -> Self {
        let f = Self::default();
        f.present.lock().unwrap().extend(hashes.iter().copied());
        f
    }
    fn has(&self, h: &BlobHash) -> bool {
        self.present.lock().unwrap().contains(h)
    }
}

impl BlobFiles for FakeFiles {
    fn contains(&self, hash: &BlobHash) -> bool {
        self.all_present || self.has(hash)
    }
    fn delete(&self, hash: &BlobHash) -> crate::Result<u64> {
        self.deleted.lock().unwrap().push(*hash);
        Ok(if self.present.lock().unwrap().remove(hash) {
            100
        } else {
            0
        })
    }
}

/// A store where every blob exists; for tests that do not exercise the blob check.
fn dummy_store() -> FakeFiles {
    FakeFiles {
        all_present: true,
        ..FakeFiles::default()
    }
}

fn rp(s: &str) -> RelPath {
    RelPath::new(s).unwrap()
}

fn file_entry(path: &str, content: &[u8]) -> Entry {
    Entry {
        path: rp(path),
        kind: EntryKind::File,
        blob: Some(BlobHash::of(content)),
        size: content.len() as u64,
        mtime_ns: 1_700_000_000_000_000_000,
        readonly: false,
    }
}

fn blob(content: &[u8]) -> BlobInfo {
    BlobInfo {
        hash: BlobHash::of(content),
        size: content.len() as u64,
        stored_size: content.len() as u64 / 2,
    }
}

fn new_version(project: ProjectId, label: &str, entries: Vec<Entry>) -> NewVersion {
    NewVersion {
        project_id: project,
        label: label.into(),
        created_at_ms: 1_700_000_000_000,
        kind: VersionKind::Manual,
        unstable: false,
        counts: ChangeCounts::default(),
        entries,
        new_blobs: Vec::new(),
    }
}

fn add_project(db: &Db, name: &str) -> ProjectId {
    let root = std::env::temp_dir().join("dsnap-db-tests").join(name);
    db.insert_project(name, &root, &ProjectSettings::default())
        .unwrap()
}

fn count(db: &Db, table: &str) -> i64 {
    db.conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn project_crud() {
    let db = Db::open_in_memory().unwrap();
    let root = PathBuf::from("/work/alpha");
    let id = db
        .insert_project("alpha", &root, &ProjectSettings::default())
        .unwrap();
    let p = db.get_project(id).unwrap();
    assert_eq!(
        (p.name.as_str(), p.root.as_path()),
        ("alpha", root.as_path())
    );
    assert_eq!(p.settings, ProjectSettings::default());
    assert!(!p.missing);

    db.set_project_name(id, "Alpha 2").unwrap();
    db.set_project_root(id, Path::new("/work/alpha2")).unwrap();
    let settings = ProjectSettings {
        extra_ignore: vec!["*.log".into()],
        respect_gitignore: false,
        auto_snapshot: AutoSnapshot::AfterIdle { secs: 30 },
    };
    db.set_project_settings(id, &settings).unwrap();
    let p = db.get_project(id).unwrap();
    assert_eq!(p.name, "Alpha 2");
    assert_eq!(p.root, PathBuf::from("/work/alpha2"));
    assert_eq!(p.settings, settings);

    db.delete_project(id).unwrap();
    assert!(matches!(db.get_project(id), Err(Error::NotFound(_))));
    assert!(matches!(db.delete_project(id), Err(Error::NotFound(_))));
    assert!(matches!(
        db.set_project_name(id, "x"),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn duplicate_project_root_is_invalid_input() {
    let db = Db::open_in_memory().unwrap();
    let root = PathBuf::from("/work/same");
    db.insert_project("a", &root, &ProjectSettings::default())
        .unwrap();
    let err = db
        .insert_project("b", &root, &ProjectSettings::default())
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    let other = db
        .insert_project("c", Path::new("/work/other"), &ProjectSettings::default())
        .unwrap();
    let err = db.set_project_root(other, &root).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
}

#[test]
fn projects_list_by_name() {
    let db = Db::open_in_memory().unwrap();
    for n in ["charlie", "Bravo", "alpha"] {
        add_project(&db, n);
    }
    let names: Vec<_> = db
        .list_projects()
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["alpha", "Bravo", "charlie"]);
}

#[test]
fn version_round_trip_and_entry_kinds() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let entries = vec![
        Entry {
            path: rp("a/dir"),
            kind: EntryKind::Dir,
            blob: None,
            size: 0,
            mtime_ns: -5,
            readonly: false,
        },
        file_entry("a/file.txt", b"hello"),
        Entry {
            path: rp("link"),
            kind: EntryKind::Symlink {
                target: "../outside".into(),
            },
            blob: None,
            size: 10,
            mtime_ns: 7,
            readonly: true,
        },
    ];
    let mut nv = new_version(p, "first", entries.clone());
    nv.kind = VersionKind::Safety;
    nv.unstable = true;
    nv.counts = ChangeCounts {
        added: 3,
        modified: 1,
        deleted: 2,
    };
    nv.new_blobs = vec![blob(b"hello")];
    let v = db.insert_version_with(&nv, &dummy_store()).unwrap();
    assert_eq!(db.get_version(v.id).unwrap(), v);
    assert_eq!(v.kind, VersionKind::Safety);
    assert!(v.unstable && !v.pinned);
    assert_eq!(v.counts.deleted, 2);
    assert_eq!(db.entries(v.id).unwrap(), entries);
    assert_eq!(
        db.entry(v.id, &rp("link")).unwrap().as_ref(),
        Some(&entries[2])
    );
    assert_eq!(db.entry(v.id, &rp("nope")).unwrap(), None);
    assert_eq!(count(&db, "blobs"), 1);
}

#[test]
fn entries_are_sorted_by_path() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let unsorted = vec![
        file_entry("b", b"1"),
        file_entry("a/z", b"2"),
        file_entry("B", b"3"),
        file_entry("a.txt", b"4"),
    ];
    let v = db
        .insert_version_with(&new_version(p, "v", unsorted), &dummy_store())
        .unwrap();
    let paths: Vec<_> = db
        .entries(v.id)
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect();
    let mut expected = paths.clone();
    expected.sort();
    assert_eq!(paths, expected);
    assert_eq!(paths[0].as_str(), "B");
}

#[test]
fn version_listing_latest_and_previous() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let other = add_project(&db, "other");
    assert_eq!(db.latest_version(p).unwrap(), None);
    assert!(db.latest_entry_index(p).unwrap().is_empty());

    let store = dummy_store();
    let v1 = db
        .insert_version_with(&new_version(p, "v1", vec![]), &store)
        .unwrap();
    let o1 = db
        .insert_version_with(&new_version(other, "o1", vec![]), &store)
        .unwrap();
    let v2 = db
        .insert_version_with(&new_version(p, "v2", vec![file_entry("x", b"x")]), &store)
        .unwrap();

    let labels: Vec<_> = db
        .list_versions(p)
        .unwrap()
        .into_iter()
        .map(|v| v.label)
        .collect();
    assert_eq!(labels, ["v2", "v1"]);
    assert_eq!(db.latest_version(p).unwrap(), Some(v2.clone()));
    assert_eq!(db.previous_version(v2.id).unwrap(), Some(v1.clone()));
    assert_eq!(db.previous_version(v1.id).unwrap(), None);
    assert_eq!(db.previous_version(o1.id).unwrap(), None);
    assert!(matches!(
        db.previous_version(VersionId(999)),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        db.entries(VersionId(999)),
        Err(Error::NotFound(_))
    ));

    let index = db.latest_entry_index(p).unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[&rp("x")], file_entry("x", b"x"));
}

#[test]
fn edit_and_delete_versions() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let v = db
        .insert_version_with(
            &new_version(p, "v", vec![file_entry("f", b"f")]),
            &dummy_store(),
        )
        .unwrap();
    db.set_version_label(v.id, "renamed").unwrap();
    db.set_version_pinned(v.id, true).unwrap();
    let got = db.get_version(v.id).unwrap();
    assert_eq!(got.label, "renamed");
    assert!(got.pinned);
    db.set_version_pinned(v.id, false).unwrap();
    assert!(!db.get_version(v.id).unwrap().pinned);

    db.delete_version(v.id).unwrap();
    assert!(matches!(db.get_version(v.id), Err(Error::NotFound(_))));
    assert_eq!(count(&db, "entries"), 0);
    assert!(matches!(
        db.set_version_label(v.id, "x"),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(db.delete_version(v.id), Err(Error::NotFound(_))));
}

#[test]
fn deleting_a_project_cascades_but_keeps_blobs() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let keep = add_project(&db, "keep");
    let mut nv = new_version(p, "v", vec![file_entry("f", b"f")]);
    nv.new_blobs = vec![blob(b"f")];
    db.insert_version_with(&nv, &dummy_store()).unwrap();
    let nk = new_version(keep, "k", vec![file_entry("g", b"g")]);
    db.insert_version_with(&nk, &dummy_store()).unwrap();

    db.delete_project(p).unwrap();
    assert_eq!(count(&db, "versions"), 1);
    assert_eq!(count(&db, "entries"), 1);
    assert_eq!(count(&db, "blobs"), 1);
    assert_eq!(db.unreferenced_blobs().unwrap(), vec![blob(b"f")]);
}

#[test]
fn failed_insert_leaves_nothing_behind() {
    // Crash simulation: the version row and the first entries are written, then an entry
    // fails (duplicate path) and the transaction is dropped. Nothing may remain.
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let entries = vec![
        file_entry("a", b"a"),
        file_entry("b", b"b"),
        file_entry("a", b"again"),
    ];
    let mut nv = new_version(p, "bad", entries);
    nv.new_blobs = vec![blob(b"a")];
    let err = db.insert_version_with(&nv, &dummy_store()).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    assert_eq!(count(&db, "versions"), 0);
    assert_eq!(count(&db, "entries"), 0);
    assert_eq!(count(&db, "blobs"), 0);

    // An out-of-range value late in the list fails the same way.
    let mut nv = new_version(
        p,
        "bad2",
        vec![file_entry("a", b"a"), file_entry("b", b"b")],
    );
    nv.entries[1].size = u64::MAX;
    assert!(db.insert_version_with(&nv, &dummy_store()).is_err());
    assert_eq!(count(&db, "versions"), 0);
    assert_eq!(count(&db, "entries"), 0);

    // The connection is still usable afterwards.
    let ok = new_version(p, "ok", vec![file_entry("a", b"a")]);
    db.insert_version_with(&ok, &dummy_store()).unwrap();
    assert_eq!(count(&db, "versions"), 1);
}

#[test]
fn version_for_unknown_project_is_not_found() {
    let db = Db::open_in_memory().unwrap();
    let err = db
        .insert_version_with(&new_version(ProjectId(42), "v", vec![]), &dummy_store())
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
}

#[test]
fn blobs_insert_is_idempotent() {
    let db = Db::open_in_memory().unwrap();
    db.insert_blob(&blob(b"x")).unwrap();
    db.insert_blob(&blob(b"x")).unwrap();
    assert_eq!(count(&db, "blobs"), 1);
    assert_eq!(db.unreferenced_blobs().unwrap(), vec![blob(b"x")]);
}

#[test]
fn global_settings_default_and_round_trip() {
    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    assert_eq!(db.global_settings().unwrap(), GlobalSettings::default());
    let s = GlobalSettings {
        size_cap_bytes: 1234,
        retention_keep: 7,
    };
    db.set_global_settings(&s).unwrap();
    db.set_global_settings(&s).unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(db.global_settings().unwrap(), s);

    // Fields missing from the stored JSON take their defaults.
    db.conn()
        .execute(
            r#"UPDATE settings SET value_json = '{"retentionKeep": 3}' WHERE key = 'global'"#,
            [],
        )
        .unwrap();
    let got = db.global_settings().unwrap();
    assert_eq!(got.retention_keep, 3);
    assert_eq!(got.size_cap_bytes, GlobalSettings::DEFAULT_SIZE_CAP_BYTES);
}

#[test]
fn insert_10k_entries_is_fast() {
    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    let p = add_project(&db, "big");
    let entries: Vec<_> = (0..10_000u32)
        .map(|i| file_entry(&format!("d{:02}/f{i:05}.txt", i % 50), &i.to_le_bytes()))
        .collect();
    let mut nv = new_version(p, "big", entries);
    nv.entries.sort_by(|a, b| a.path.cmp(&b.path));
    nv.new_blobs = (0..10_000u32).map(|i| blob(&i.to_le_bytes())).collect();
    let start = Instant::now();
    let v = db.insert_version_with(&nv, &dummy_store()).unwrap();
    let took = start.elapsed();
    assert_eq!(db.entries(v.id).unwrap().len(), 10_000);
    // Target is < 150 ms; the bound is generous so slow CI machines do not flake.
    assert!(
        took.as_millis() < 1000,
        "inserting 10k entries took {took:?}"
    );
}

// ---- DSNA-32: change token ----

#[test]
fn change_token_is_stable_when_idle() {
    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    add_project(&db, "p");
    let t = db.change_token().unwrap();
    // Reads do not change it.
    db.list_projects().unwrap();
    assert_eq!(db.change_token().unwrap(), t);
    assert_eq!(db.change_token().unwrap(), t);
}

#[test]
fn change_token_sees_other_connection_writes() {
    let (_dir, path) = temp_db();
    let app = Db::open(&path).unwrap();
    let cli = Db::open(&path).unwrap();
    let p = add_project(&cli, "p");

    let t0 = app.change_token().unwrap();
    let v = cli
        .insert_version_with(&new_version(p, "from cli", vec![]), &dummy_store())
        .unwrap();
    let t1 = app.change_token().unwrap();
    assert_ne!(t0, t1, "insert from another connection");

    // A label edit leaves MAX(versions.id) unchanged but must still change the token.
    cli.set_version_label(v.id, "renamed").unwrap();
    let t2 = app.change_token().unwrap();
    assert_ne!(t1, t2, "label edit from another connection");
    assert_eq!(app.change_token().unwrap(), t2);
}

#[test]
fn change_token_sees_own_writes() {
    let db = Db::open_in_memory().unwrap();
    let t0 = db.change_token().unwrap();
    let p = add_project(&db, "p");
    let t1 = db.change_token().unwrap();
    assert_ne!(t0, t1);
    db.set_project_name(p, "q").unwrap();
    assert_ne!(db.change_token().unwrap(), t1);
}

// ---- DSNA-82: blob lifetime protocol ----

/// Two handles on one file, as the CLI and the app would have, plus a version that used to
/// reference blob X and was deleted, leaving X recorded but unreferenced.
fn two_handles_with_unreferenced_x() -> (tempfile::TempDir, Db, Db, ProjectId, BlobHash) {
    let (dir, path) = temp_db();
    let snap_side = Db::open(&path).unwrap();
    let prune_side = Db::open(&path).unwrap();
    let p = add_project(&snap_side, "p");
    let x = BlobHash::of(b"x");
    let mut old = new_version(p, "old", vec![file_entry("f", b"x")]);
    old.new_blobs = vec![blob(b"x")];
    let old = snap_side.insert_version_with(&old, &dummy_store()).unwrap();
    snap_side.delete_version(old.id).unwrap();
    assert!(!snap_side.blob_referenced(&x).unwrap());
    (dir, snap_side, prune_side, p, x)
}

#[test]
fn prune_then_insert_referencing_pruned_blob_is_blob_missing() {
    let (_dir, snap_side, prune_side, p, x) = two_handles_with_unreferenced_x();
    let files = FakeFiles::with(&[x]);

    // Snapshot side: X's file exists, so `Store::put` dedupes and X is not in new_blobs.
    let nv = new_version(p, "dedupe", vec![file_entry("f", b"x")]);

    // Prune side runs to completion first.
    let report = prune_side
        .prune_unreferenced_with(&files, PRUNE_BATCH)
        .unwrap();
    assert_eq!(report.blobs_pruned, 1);
    assert_eq!(report.bytes_freed, 100);
    assert_eq!(report.versions_deleted, 0);
    assert!(!files.has(&x));

    // Then the snapshot commits: it must fail and commit nothing.
    let err = snap_side.insert_version_with(&nv, &files).unwrap_err();
    assert!(matches!(err, Error::BlobMissing(h) if h == x), "{err}");
    assert_eq!(count(&snap_side, "versions"), 0);
    assert_eq!(count(&snap_side, "entries"), 0);
    assert_eq!(count(&snap_side, "blobs"), 0);
    assert_eq!(snap_side.latest_version(p).unwrap(), None);

    // Retry after re-storing X (protocol step 3) succeeds.
    files.present.lock().unwrap().insert(x);
    let mut retry = nv.clone();
    retry.new_blobs = vec![blob(b"x")];
    snap_side.insert_version_with(&retry, &files).unwrap();
    assert!(snap_side.blob_referenced(&x).unwrap());
}

#[test]
fn insert_then_prune_keeps_referenced_blob() {
    let (_dir, snap_side, prune_side, p, x) = two_handles_with_unreferenced_x();
    let files = FakeFiles::with(&[x]);

    let nv = new_version(p, "dedupe", vec![file_entry("f", b"x")]);
    snap_side.insert_version_with(&nv, &files).unwrap();

    let report = prune_side
        .prune_unreferenced_with(&files, PRUNE_BATCH)
        .unwrap();
    assert_eq!(report, RetentionReport::default());
    assert!(files.has(&x));
    assert!(files.deleted.lock().unwrap().is_empty());
    assert_eq!(count(&prune_side, "blobs"), 1);
}

#[test]
fn insert_waits_for_a_prune_holding_the_write_lock() {
    // The prune side holds BEGIN IMMEDIATE while it deletes X; the snapshot side's insert
    // must block on the lock and then see X gone, never commit a dangling reference.
    let (_dir, snap_side, prune_side, p, x) = two_handles_with_unreferenced_x();
    let files = std::sync::Arc::new(FakeFiles::with(&[x]));

    let mut conn = prune_side.conn();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();

    let insert = {
        let files = files.clone();
        let nv = new_version(p, "dedupe", vec![file_entry("f", b"x")]);
        thread::spawn(move || snap_side.insert_version_with(&nv, &*files).map(|_| ()))
    };
    thread::sleep(std::time::Duration::from_millis(200));
    assert!(!insert.is_finished(), "insert must wait for the write lock");

    files.delete(&x).unwrap();
    tx.execute("DELETE FROM blobs WHERE hash = ?1", [x.0.as_slice()])
        .unwrap();
    tx.commit().unwrap();
    drop(conn);

    let err = insert.join().unwrap().unwrap_err();
    assert!(matches!(err, Error::BlobMissing(h) if h == x), "{err}");
    assert_eq!(count(&prune_side, "versions"), 0);
}

#[test]
fn blob_check_only_applies_to_unreferenced_blobs() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let a = BlobHash::of(b"a");
    let files = FakeFiles::with(&[a]);
    let v1 = new_version(p, "v1", vec![file_entry("a", b"a")]);
    db.insert_version_with(&v1, &files).unwrap();
    // A blob an existing entry references cannot be pruned, so it is not checked.
    files.present.lock().unwrap().clear();
    let v2 = new_version(p, "v2", vec![file_entry("a2", b"a")]);
    db.insert_version_with(&v2, &files).unwrap();
    // A new blob missing from the store is rejected.
    let v3 = new_version(p, "v3", vec![file_entry("b", b"b")]);
    let err = db.insert_version_with(&v3, &files).unwrap_err();
    assert!(matches!(err, Error::BlobMissing(h) if h == BlobHash::of(b"b")));
    // Dirs and symlinks carry no blob and need no check.
    let dir = Entry {
        path: rp("d"),
        kind: EntryKind::Dir,
        blob: None,
        size: 0,
        mtime_ns: 0,
        readonly: false,
    };
    db.insert_version_with(&new_version(p, "v4", vec![dir]), &files)
        .unwrap();
}

#[test]
fn prune_respects_limit_and_reports() {
    let db = Db::open_in_memory().unwrap();
    let hashes: Vec<_> = (0..5u8).map(|i| BlobHash::of(&[i])).collect();
    for i in 0..5u8 {
        db.insert_blob(&blob(&[i])).unwrap();
    }
    let p = add_project(&db, "p");
    // Blob 0 stays referenced.
    let v = new_version(p, "v", vec![file_entry("f", &[0])]);
    db.insert_version_with(&v, &dummy_store()).unwrap();
    let files = FakeFiles::with(&hashes);

    let r = db.prune_unreferenced_with(&files, 3).unwrap();
    assert_eq!((r.blobs_pruned, r.bytes_freed), (3, 300));
    let r = db.prune_unreferenced_with(&files, 3).unwrap();
    assert_eq!((r.blobs_pruned, r.bytes_freed), (1, 100));
    let r = db.prune_unreferenced_with(&files, 3).unwrap();
    assert_eq!(r.blobs_pruned, 0);
    assert_eq!(
        db.prune_unreferenced_with(&files, 0).unwrap().blobs_pruned,
        0
    );

    assert!(files.has(&hashes[0]));
    assert_eq!(files.present.lock().unwrap().len(), 1);
    assert!(db.blob_referenced(&hashes[0]).unwrap());
    assert!(!db.blob_referenced(&hashes[1]).unwrap());
    assert!(db.unreferenced_blobs().unwrap().is_empty());
}

#[test]
fn prune_of_rows_whose_files_are_gone_frees_zero_bytes() {
    // A prune that crashed after deleting files leaves rows; the next prune clears them.
    let db = Db::open_in_memory().unwrap();
    db.insert_blob(&blob(b"gone")).unwrap();
    let files = FakeFiles::default();
    let r = db.prune_unreferenced_with(&files, PRUNE_BATCH).unwrap();
    assert_eq!((r.blobs_pruned, r.bytes_freed), (1, 0));
    assert_eq!(count(&db, "blobs"), 0);
}

#[test]
fn failed_store_delete_rolls_back_the_batch() {
    struct Failing;
    impl BlobFiles for Failing {
        fn contains(&self, _: &BlobHash) -> bool {
            true
        }
        fn delete(&self, _: &BlobHash) -> crate::Result<u64> {
            Err(Error::io("objects/xx", std::io::Error::other("denied")))
        }
    }
    let db = Db::open_in_memory().unwrap();
    db.insert_blob(&blob(b"a")).unwrap();
    assert!(matches!(
        db.prune_unreferenced_with(&Failing, PRUNE_BATCH),
        Err(Error::Io { .. })
    ));
    assert_eq!(count(&db, "blobs"), 1);
}
