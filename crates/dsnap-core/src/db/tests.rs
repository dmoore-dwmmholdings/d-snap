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
fn fresh_db_gets_current_schema() {
    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    assert_eq!(
        schema::user_version(&db.conn()).unwrap(),
        schema::SCHEMA_VERSION
    );
    assert_eq!(schema::SCHEMA_VERSION, 3);
    assert_eq!(
        table_names(&db),
        ["blobs", "entries", "projects", "settings", "versions"]
    );
}

#[test]
fn in_memory_db_gets_current_schema() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(
        schema::user_version(&db.conn()).unwrap(),
        schema::SCHEMA_VERSION
    );
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
    assert_eq!(q("PRAGMA synchronous"), "2"); // FULL: reference removals are durable
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
    assert_eq!(
        schema::user_version(&db.conn()).unwrap(),
        schema::SCHEMA_VERSION
    );
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
    assert_eq!(
        schema::user_version(&db.conn()).unwrap(),
        schema::SCHEMA_VERSION
    );
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

/// In-memory stand-in for the blob store, so tests can control which blob files exist.
#[derive(Default)]
struct FakeFiles {
    /// `present_size` is always `Some` (for tests that do not exercise the blob check).
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
    fn present_size(&self, hash: &BlobHash) -> Option<u64> {
        (self.all_present || self.has(hash)).then_some(100)
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

impl BlobSweep for FakeFiles {
    fn sweep(&self, referenced: &HashSet<BlobHash>) -> crate::Result<Swept> {
        let mut present = self.present.lock().unwrap();
        let before = present.len();
        present.retain(|h| referenced.contains(h));
        let deleted = (before - present.len()) as u64;
        Ok(Swept {
            deleted,
            bytes_freed: deleted * 100,
            failed: 0,
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
    // "g" was not in new_blobs, so insert_version recorded a row for it (DSNA-101).
    assert_eq!(count(&db, "blobs"), 2);
    assert_eq!(db.unreferenced_blobs().unwrap(), vec![blob(b"f")]);
    assert!(db.blob_referenced(&BlobHash::of(b"g")).unwrap());
    assert_refs_exact(&db);
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

/// Store whose delete always fails for one hash (e.g. a file held open by a scanner).
struct FailingFor(BlobHash, FakeFiles);

impl BlobFiles for FailingFor {
    fn present_size(&self, h: &BlobHash) -> Option<u64> {
        self.1.present_size(h)
    }
    fn delete(&self, h: &BlobHash) -> crate::Result<u64> {
        if *h == self.0 {
            Err(Error::io("objects/xx", std::io::Error::other("denied")))
        } else {
            self.1.delete(h)
        }
    }
}

#[test]
fn failed_store_delete_keeps_the_row_and_reports_the_error() {
    let db = Db::open_in_memory().unwrap();
    db.insert_blob(&blob(b"a")).unwrap();
    let files = FailingFor(BlobHash::of(b"a"), FakeFiles::with(&[BlobHash::of(b"a")]));
    assert!(matches!(
        db.prune_unreferenced_with(&files, PRUNE_BATCH),
        Err(Error::Io { .. })
    ));
    assert_eq!(count(&db, "blobs"), 1);
    assert!(files.1.has(&BlobHash::of(b"a")));
}

#[test]
fn undeletable_blob_does_not_block_later_batches() {
    let db = Db::open_in_memory().unwrap();
    let hashes: Vec<_> = (0..4u8).map(|i| BlobHash::of(&[i])).collect();
    for i in 0..4u8 {
        db.insert_blob(&blob(&[i])).unwrap();
    }
    // Make the first blob in hash order the stuck one.
    let stuck = *hashes.iter().min().unwrap();
    let files = FailingFor(stuck, FakeFiles::with(&hashes));

    // Batch 1 picks only the stuck blob: error, failure counted, row kept.
    assert!(db.prune_unreferenced_with(&files, 1).is_err());
    // Later batches move past it.
    let mut pruned = 0;
    for _ in 0..3 {
        pruned += db.prune_unreferenced_with(&files, 1).unwrap().blobs_pruned;
    }
    assert_eq!(pruned, 3);
    assert_eq!(count(&db, "blobs"), 1);
    assert!(files.1.has(&stuck));

    // A batch mixing a failure and successes commits the successes.
    db.insert_blob(&blob(b"new")).unwrap();
    files.1.present.lock().unwrap().insert(BlobHash::of(b"new"));
    let r = db.prune_unreferenced_with(&files, PRUNE_BATCH).unwrap();
    assert_eq!(r.blobs_pruned, 1);
    assert_eq!(count(&db, "blobs"), 1);
}

#[test]
fn migration_from_v1_adds_prune_failures() {
    let (_dir, path) = temp_db();
    {
        // Build a v1 database with the released v1 migration.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema::MIGRATIONS[0]).unwrap();
        conn.execute_batch(
            "INSERT INTO blobs VALUES (x'00', 1, 1);
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(
        schema::user_version(&db.conn()).unwrap(),
        schema::SCHEMA_VERSION
    );
    let f: i64 = db
        .conn()
        .query_row("SELECT prune_failures FROM blobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(f, 0);
}

// ---- DSNA-88: sweep orphan blob files ----

#[test]
fn sweep_deletes_orphans_and_keeps_referenced() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let kept = BlobHash::of(b"kept");
    let orphan = BlobHash::of(b"orphan"); // file only, no row
    let unref_row = BlobHash::of(b"unref"); // file and row, no entry
    let files = FakeFiles::with(&[kept, orphan, unref_row]);
    let mut v = new_version(p, "v", vec![file_entry("k", b"kept")]);
    v.new_blobs = vec![blob(b"kept")];
    db.insert_version_with(&v, &files).unwrap();
    db.insert_blob(&blob(b"unref")).unwrap();

    let r = db.sweep_orphans_with(&files).unwrap();
    assert_eq!(
        (r.blobs_pruned, r.bytes_freed, r.versions_deleted),
        (2, 200, 0)
    );
    assert!(files.has(&kept));
    assert!(!files.has(&orphan) && !files.has(&unref_row));
    assert_eq!(count(&db, "blobs"), 1);
    assert!(db.unreferenced_blobs().unwrap().is_empty());
}

#[test]
fn snapshot_committing_after_sweep_gets_blob_missing() {
    let (_dir, path) = temp_db();
    let snap_side = Db::open(&path).unwrap();
    let sweep_side = Db::open(&path).unwrap();
    let p = add_project(&snap_side, "p");
    let files = FakeFiles::default();

    // Snapshot side stores Y (new to the DB) but has not committed yet.
    let y = BlobHash::of(b"y");
    files.present.lock().unwrap().insert(y);
    let mut nv = new_version(p, "v", vec![file_entry("y", b"y")]);
    nv.new_blobs = vec![blob(b"y")];

    // Sweep runs first and removes Y as an orphan.
    let r = sweep_side.sweep_orphans_with(&files).unwrap();
    assert_eq!(r.blobs_pruned, 1);

    let err = snap_side.insert_version_with(&nv, &files).unwrap_err();
    assert!(matches!(err, Error::BlobMissing(h) if h == y), "{err}");
    assert_eq!(count(&snap_side, "versions"), 0);
    assert_eq!(count(&snap_side, "entries"), 0);
}

#[test]
fn failed_sweep_rolls_back_row_deletes() {
    struct Failing;
    impl BlobSweep for Failing {
        fn sweep(&self, _: &HashSet<BlobHash>) -> crate::Result<Swept> {
            Err(Error::io("objects", std::io::Error::other("denied")))
        }
    }
    let db = Db::open_in_memory().unwrap();
    db.insert_blob(&blob(b"a")).unwrap();
    assert!(db.sweep_orphans_with(&Failing).is_err());
    assert_eq!(count(&db, "blobs"), 1);
}

// ---- DSNA-101: blob reference counts ----

/// Release-only timing: `insert_version` of a 10k-entry version must not grow with history.
/// Run by CI with
/// `cargo test -p dsnap-core --release --lib db::tests::perf_ -- --ignored --nocapture`.
#[test]
#[ignore = "release-only timing; run with --release -- --ignored"]
fn perf_insert_version_stays_flat_over_200_versions() {
    const FILES: u32 = 10_000;
    const CHANGED: u32 = 49;
    const VERSIONS: u32 = 200;
    const SAMPLES: usize = 20;

    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    let p = add_project(&db, "perf");
    let files = dummy_store();
    let mut contents: Vec<Vec<u8>> = (0..FILES).map(|i| format!("{i}:0").into_bytes()).collect();
    let mut entries: Vec<Entry> = (0..FILES)
        .map(|i| {
            file_entry(
                &format!("d{:02}/f{i:05}.txt", i % 50),
                &contents[i as usize],
            )
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let mut times = Vec::new();
    for round in 0..VERSIONS {
        let mut nv = new_version(p, "v", entries.clone());
        nv.new_blobs = if round == 0 {
            contents.iter().map(|c| blob(c)).collect()
        } else {
            Vec::new()
        };
        if round > 0 {
            // Change CHANGED files, spread over the tree.
            for k in 0..CHANGED {
                let i = ((round * CHANGED + k) * 7919 % FILES) as usize;
                contents[i] = format!("{i}:{round}").into_bytes();
                let e = &mut nv.entries[i];
                e.blob = Some(BlobHash::of(&contents[i]));
                e.size = contents[i].len() as u64;
                nv.new_blobs.push(blob(&contents[i]));
            }
            nv.new_blobs.sort_by_key(|b| b.hash);
            nv.new_blobs.dedup_by_key(|b| b.hash);
        }
        let t = Instant::now();
        db.insert_version_with(&nv, &files).unwrap();
        times.push(t.elapsed());
        entries = nv.entries;
    }
    let median = |s: &[std::time::Duration]| {
        let mut s = s.to_vec();
        s.sort();
        s[s.len() / 2]
    };
    let early = median(&times[1..=SAMPLES]);
    let late = median(&times[times.len() - SAMPLES..]);
    eprintln!(
        "insert_version, {FILES} entries: first {:?}, versions 2..={} median {early:?}, \
         last {SAMPLES} (of {VERSIONS}) median {late:?}",
        times[0],
        SAMPLES + 1
    );
    assert!(
        late <= early * 2,
        "insert_version grew with history: {early:?} -> {late:?}"
    );

    // Retention cost with the counts: delete the oldest version (10k decrements) and prune.
    let oldest = *db.list_versions(p).unwrap().last().map(|v| &v.id).unwrap();
    let t = Instant::now();
    db.delete_version(oldest).unwrap();
    let deleted = t.elapsed();
    let t = Instant::now();
    let r = db.prune_unreferenced_with(&files, PRUNE_BATCH).unwrap();
    eprintln!(
        "delete_version of {FILES} entries: {deleted:?}; prune of {} blobs: {:?}",
        r.blobs_pruned,
        t.elapsed()
    );
    assert_refs_exact(&db);

    let t = Instant::now();
    db.delete_project(p).unwrap();
    eprintln!(
        "delete_project with {} versions: {:?}",
        VERSIONS - 1,
        t.elapsed()
    );
    assert!(db.referenced_hashes().unwrap().is_empty());
}

/// Check the schema v3 invariants: every entry blob has a row; `refs > 0` exactly when an
/// entry uses the blob; and `refs` equals the run-start count. On small databases the count
/// comes from an independent SQL query; large ones (the perf test) compare with
/// `refs::run_starts`, which recomputes it from the entries in one pass.
fn assert_refs_exact(db: &Db) {
    let conn = db.conn();
    let entries: i64 = conn
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    let rowless: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT DISTINCT blob_hash AS h FROM entries
                                   WHERE blob_hash IS NOT NULL)
             WHERE NOT EXISTS (SELECT 1 FROM blobs b WHERE b.hash = h)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rowless, 0, "entries whose blob has no row");

    if entries > 100_000 {
        let expected = refs::run_starts(&conn, None).unwrap();
        let mut stmt = conn.prepare("SELECT hash, refs FROM blobs").unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))
            .unwrap();
        for row in rows {
            let (hash, n) = row.unwrap();
            let hash = codec::hash_from_sql(hash).unwrap();
            assert_eq!(
                n,
                expected.get(&hash).copied().unwrap_or(0),
                "refs of {hash}"
            );
        }
        return;
    }

    let (wrong, mismatched): (i64, i64) = conn
        .query_row(
            "WITH vp AS (SELECT id, LAG(id) OVER (PARTITION BY project_id ORDER BY id) AS prev
                         FROM versions),
                  s AS (SELECT DISTINCT e.version_id, e.blob_hash AS h
                        FROM entries e JOIN vp ON vp.id = e.version_id
                        WHERE e.blob_hash IS NOT NULL AND NOT EXISTS
                            (SELECT 1 FROM entries p
                             WHERE p.version_id = vp.prev AND p.blob_hash = e.blob_hash)),
                  c AS (SELECT h, COUNT(*) AS n FROM s GROUP BY h)
             SELECT (SELECT COUNT(*) FROM blobs b LEFT JOIN c ON c.h = b.hash
                     WHERE b.refs <> COALESCE(c.n, 0)),
                    (SELECT COUNT(*) FROM blobs b WHERE (b.refs > 0) <>
                        EXISTS (SELECT 1 FROM entries e WHERE e.blob_hash = b.hash))",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        wrong, 0,
        "blobs whose refs differ from their run-start count"
    );
    assert_eq!(
        mismatched, 0,
        "blobs where refs > 0 disagrees with the entries"
    );
}

fn refs(db: &Db, content: &[u8]) -> Option<i64> {
    db.conn()
        .query_row(
            "SELECT refs FROM blobs WHERE hash = ?1",
            [BlobHash::of(content).0.as_slice()],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
}

fn no_recount(_: &[Entry], _: &[Entry]) -> ChangeCounts {
    ChangeCounts::default()
}

#[test]
fn refs_follow_inserts_version_deletes_and_project_deletes() {
    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    let p = add_project(&db, "p");
    let q = add_project(&db, "q");
    let files = dummy_store();
    let abc = |db: &Db| (refs(db, b"a"), refs(db, b"b"), refs(db, b"c"));

    // "a" twice in v1 (two paths, same content), then in v2, and in project q.
    let mut v1 = new_version(
        p,
        "v1",
        vec![
            file_entry("a1", b"a"),
            file_entry("a2", b"a"),
            file_entry("b", b"b"),
        ],
    );
    v1.new_blobs = vec![blob(b"a"), blob(b"b")];
    let v1 = db.insert_version_with(&v1, &files).unwrap();
    let v2 = new_version(p, "v2", vec![file_entry("a1", b"a"), file_entry("c", b"c")]);
    let v2 = db.insert_version_with(&v2, &files).unwrap();
    let w = new_version(q, "w", vec![file_entry("a", b"a")]);
    db.insert_version_with(&w, &files).unwrap();
    // Run starts: a in v1 and w; b in v1; c in v2 (v2 keeps a, so no new start).
    assert_eq!(abc(&db), (Some(2), Some(1), Some(1)));
    assert_refs_exact(&db);

    // Deleting v1 moves a's run start to v2 and ends b's only run.
    db.delete_version(v1.id).unwrap();
    assert_eq!(abc(&db), (Some(2), Some(0), Some(1)));
    assert!(!db.blob_referenced(&BlobHash::of(b"b")).unwrap());
    assert_eq!(db.unreferenced_blobs().unwrap(), vec![blob(b"b")]);
    assert_eq!(db.referenced_hashes().unwrap().len(), 2);
    assert_refs_exact(&db);

    db.delete_project(p).unwrap();
    assert_eq!(abc(&db), (Some(1), Some(0), Some(0)));
    assert!(db.blob_referenced(&BlobHash::of(b"a")).unwrap());
    assert_refs_exact(&db);
    assert!(matches!(db.get_version(v2.id), Err(Error::NotFound(_))));

    // Prune takes exactly the refs = 0 rows.
    let pruned = db.prune_unreferenced_with(&files, PRUNE_BATCH).unwrap();
    assert_eq!(pruned.blobs_pruned, 2);
    assert_eq!(count(&db, "blobs"), 1);
    assert_refs_exact(&db);
}

#[test]
fn insert_touches_only_blobs_new_since_the_previous_version() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let files = dummy_store();
    let entries: Vec<_> = (0..100u8)
        .map(|i| file_entry(&format!("f{i:03}"), &[i]))
        .collect();
    db.insert_version_with(&new_version(p, "v1", entries.clone()), &files)
        .unwrap();
    let mut next = entries;
    next[7] = file_entry("f007", b"changed");
    let before = db.conn().total_changes();
    db.insert_version_with(&new_version(p, "v2", next), &files)
        .unwrap();
    // 1 version + 100 entries + 1 new blob row + 1 refs update.
    assert_eq!(db.conn().total_changes() - before, 103);
    assert_refs_exact(&db);
}

#[test]
fn guard_triggers_refuse_unsafe_writes() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let mut v = new_version(p, "v", vec![file_entry("a", b"a")]);
    v.new_blobs = vec![blob(b"a")];
    let v = db.insert_version_with(&v, &dummy_store()).unwrap();
    db.insert_blob(&blob(b"b")).unwrap();
    let conn = db.conn();

    // An entry may not reference a blob without a row.
    let err = conn
        .execute(
            "INSERT INTO entries (version_id, path, kind, blob_hash, size, mtime_ns)
             VALUES (?1, 'x', 'file', ?2, 1, 0)",
            params![v.id.0, BlobHash::of(b"nope").0.as_slice()],
        )
        .unwrap_err();
    assert!(is_constraint(&err), "{err}");
    // Entries never change once written (a new blob would bypass the counts).
    let err = conn
        .execute(
            "UPDATE entries SET blob_hash = ?1",
            [BlobHash::of(b"b").0.as_slice()],
        )
        .unwrap_err();
    assert!(is_constraint(&err), "{err}");
    // A referenced row may not be deleted, by prune-like SQL or any other.
    let err = conn
        .execute(
            "DELETE FROM blobs WHERE hash = ?1",
            [BlobHash::of(b"a").0.as_slice()],
        )
        .unwrap_err();
    assert!(is_constraint(&err), "{err}");
    // Counts never go below zero.
    let err = conn
        .execute("UPDATE blobs SET refs = refs - 1 WHERE refs = 0", [])
        .unwrap_err();
    assert!(is_constraint(&err), "{err}");
    drop(conn);
    assert_refs_exact(&db);
}

#[test]
fn only_delete_version_row_deletes_versions_in_db_rs() {
    // Deleting versions or projects any other way would skip the count updates.
    let src = include_str!("../db.rs");
    let code: Vec<&str> = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let uses = |pat: &str| code.iter().filter(|l| l.contains(pat)).count();
    assert_eq!(uses("DELETE FROM versions"), 1);
    assert_eq!(uses("DELETE FROM entries"), 0);
    assert_eq!(uses("DELETE FROM projects"), 1);
    assert_eq!(uses("INSERT INTO entries"), 1);
}

#[test]
fn present_blob_without_row_gets_a_row_and_missing_one_is_blob_missing() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let x = BlobHash::of(b"xx");
    let files = FakeFiles::with(&[x]);

    // A dedupe hit on a store file the DB never recorded (e.g. stored by a snapshot that did
    // not commit): the row is created from the entry size and the store's size.
    let v = new_version(p, "v", vec![file_entry("x", b"xx")]);
    db.insert_version_with(&v, &files).unwrap();
    let row = db
        .conn()
        .query_row(&format!("SELECT {BLOB_COLS}, refs FROM blobs"), [], |r| {
            Ok((blob_row(r)?, r.get::<_, i64>(3)?))
        })
        .unwrap();
    let info = blob_from_row(row.0).unwrap();
    assert_eq!(
        (info.hash, info.size, info.stored_size, row.1),
        (x, 2, 100, 1)
    );

    // No row and no file: BlobMissing, and the rollback leaves nothing.
    let y = new_version(p, "y", vec![file_entry("y", b"yy")]);
    let err = db.insert_version_with(&y, &files).unwrap_err();
    assert!(
        matches!(err, Error::BlobMissing(h) if h == BlobHash::of(b"yy")),
        "{err}"
    );
    assert_eq!(count(&db, "blobs"), 1);
    assert_eq!(count(&db, "versions"), 1);
    assert_refs_exact(&db);
}

#[test]
fn delete_on_one_connection_then_prune_on_another_frees_only_unshared_blobs() {
    let (_dir, path) = temp_db();
    let app = Db::open(&path).unwrap();
    let cli = Db::open(&path).unwrap();
    let p = add_project(&app, "p");
    let files = FakeFiles::with(&[BlobHash::of(b"s"), BlobHash::of(b"u")]);
    let mut v1 = new_version(p, "v1", vec![file_entry("s", b"s"), file_entry("u", b"u")]);
    v1.new_blobs = vec![blob(b"s"), blob(b"u")];
    let v1 = cli.insert_version_with(&v1, &files).unwrap();
    cli.insert_version_with(&new_version(p, "v2", vec![file_entry("s", b"s")]), &files)
        .unwrap();

    app.delete_version(v1.id).unwrap();
    let r = cli.prune_unreferenced_with(&files, PRUNE_BATCH).unwrap();
    assert_eq!(r.blobs_pruned, 1);
    assert!(files.has(&BlobHash::of(b"s")) && !files.has(&BlobHash::of(b"u")));
    assert_refs_exact(&app);
}

#[test]
fn retention_deletes_keep_counts_exact() {
    let db = Db::open_in_memory().unwrap();
    let p = add_project(&db, "p");
    let files = dummy_store();
    let mut ids = Vec::new();
    // Each version changes one file; file "k" keeps the same content throughout.
    for i in 0..8u8 {
        let entries = vec![file_entry("k", b"k"), file_entry("f", &[i % 3])];
        ids.push(
            db.insert_version_with(&new_version(p, "v", entries), &files)
                .unwrap()
                .id,
        );
    }
    db.set_version_pinned(ids[2], true).unwrap();
    assert_eq!(
        db.delete_versions(&[ids[4], ids[1]], &no_recount).unwrap(),
        2
    );
    assert_refs_exact(&db);
    assert_eq!(
        db.delete_unpinned_beyond(p, 2, &[ids[3]], 10, &no_recount)
            .unwrap(),
        2
    );
    assert_refs_exact(&db);
    let left: Vec<_> = db.list_versions(p).unwrap().iter().map(|v| v.id).collect();
    assert_eq!(left, [ids[7], ids[6], ids[3], ids[2]]);
    assert_eq!(refs(&db, b"k"), Some(1));
}

/// One step of [`refs_stay_exact_under_random_operations`].
#[derive(Debug, Clone)]
enum Op {
    /// Insert a version in project `.0` whose files use blobs from the bit mask `.1`.
    Insert(usize, u16),
    /// Delete the n-th surviving version (modulo the count).
    Delete(usize),
    /// Delete two versions in one `delete_versions` call.
    DeleteTwo(usize, usize),
    /// Retention: keep `.1` unpinned versions of project `.0`.
    Retain(usize, u32),
    /// Pin the n-th surviving version.
    Pin(usize),
    /// Delete project `.0` and add it again.
    DropProject(usize),
}

fn op() -> impl proptest::strategy::Strategy<Value = Op> {
    use proptest::prelude::*;
    prop_oneof![
        6 => (0..2usize, any::<u16>()).prop_map(|(p, m)| Op::Insert(p, m)),
        2 => any::<usize>().prop_map(Op::Delete),
        1 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::DeleteTwo(a, b)),
        1 => (0..2usize, 0..4u32).prop_map(|(p, k)| Op::Retain(p, k)),
        1 => any::<usize>().prop_map(Op::Pin),
        1 => (0..2usize).prop_map(Op::DropProject),
    ]
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// Any mix of inserts and deletes keeps `refs` equal to the run-start count, and
    /// `refs > 0` equal to "an entry uses it" (DSNA-101).
    #[test]
    fn refs_stay_exact_under_random_operations(ops in proptest::collection::vec(op(), 1..40)) {
        let db = Db::open_in_memory().unwrap();
        let files = dummy_store();
        let mut projects = [add_project(&db, "p0"), add_project(&db, "p1")];
        let pool: Vec<Vec<u8>> = (0..10u8).map(|i| vec![b'b', i]).collect();
        for op in ops {
            let all: Vec<VersionId> = projects
                .iter()
                .flat_map(|p| db.list_versions(*p).unwrap())
                .map(|v| v.id)
                .collect();
            match op {
                Op::Insert(p, mask) => {
                    // Two paths may share a blob; bit 10+ picks a duplicate path.
                    let mut entries: Vec<Entry> = (0..10)
                        .filter(|i| mask & (1 << i) != 0)
                        .map(|i| file_entry(&format!("f{i}"), &pool[i]))
                        .collect();
                    if mask & (1 << 10) != 0 {
                        entries.push(file_entry("g", &pool[(mask >> 11) as usize % 10]));
                    }
                    entries.sort_by(|a, b| a.path.cmp(&b.path));
                    db.insert_version_with(&new_version(projects[p], "v", entries), &files)
                        .unwrap();
                }
                Op::Delete(n) if !all.is_empty() => {
                    db.delete_version(all[n % all.len()]).unwrap();
                }
                Op::DeleteTwo(a, b) if !all.is_empty() => {
                    let ids = [all[a % all.len()], all[b % all.len()]];
                    db.delete_versions(&ids, &no_recount).unwrap();
                }
                Op::Retain(p, keep) => {
                    db.delete_unpinned_beyond(projects[p], keep, &[], 32, &no_recount)
                        .unwrap();
                }
                Op::Pin(n) if !all.is_empty() => {
                    db.set_version_pinned(all[n % all.len()], true).unwrap();
                }
                Op::DropProject(p) => {
                    db.delete_project(projects[p]).unwrap();
                    projects[p] = add_project(&db, &format!("p{p}"));
                }
                _ => {}
            }
            assert_refs_exact(&db);
        }
        db.prune_unreferenced_with(&files, PRUNE_BATCH).unwrap();
        assert_refs_exact(&db);
        proptest::prop_assert!(db.unreferenced_blobs().unwrap().is_empty());
    }
}

#[test]
fn crash_mid_prune_rolls_back_and_the_insert_recheck_catches_reuse() {
    // A prune deletes X's file, then the process dies before COMMIT: the row comes back with
    // refs = 0 although the file is gone.
    let (_dir, snap_side, prune_side, p, x) = two_handles_with_unreferenced_x();
    let files = FakeFiles::with(&[x]);
    {
        let mut conn = prune_side.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        files.delete(&x).unwrap();
        tx.execute("DELETE FROM blobs WHERE hash = ?1", [x.0.as_slice()])
            .unwrap();
        drop(tx); // no commit: rolled back, as after a crash
    }
    assert_eq!(refs(&snap_side, b"x"), Some(0));
    assert!(!files.has(&x));

    // A snapshot that deduplicated against X must not commit a reference to it.
    let nv = new_version(p, "dedupe", vec![file_entry("f", b"x")]);
    let err = snap_side.insert_version_with(&nv, &files).unwrap_err();
    assert!(matches!(err, Error::BlobMissing(h) if h == x), "{err}");

    // The next prune clears the stale row and frees nothing.
    let r = prune_side
        .prune_unreferenced_with(&files, PRUNE_BATCH)
        .unwrap();
    assert_eq!((r.blobs_pruned, r.bytes_freed), (1, 0));
    assert_eq!(count(&prune_side, "blobs"), 0);

    // The retry with X stored again commits and counts the reference.
    files.present.lock().unwrap().insert(x);
    let mut retry = nv.clone();
    retry.new_blobs = vec![blob(b"x")];
    snap_side.insert_version_with(&retry, &files).unwrap();
    assert_eq!(refs(&snap_side, b"x"), Some(1));
    assert_refs_exact(&snap_side);
}

#[test]
fn crash_mid_delete_rolls_back_rows_and_counts_together() {
    // Counts and rows change in one transaction: a delete that dies before COMMIT leaves
    // both as they were, so no blob becomes prunable while a version still uses it.
    let (_dir, path) = temp_db();
    let db = Db::open(&path).unwrap();
    let p = add_project(&db, "p");
    let mut v = new_version(p, "v", vec![file_entry("a", b"a")]);
    v.new_blobs = vec![blob(b"a")];
    let v = db.insert_version_with(&v, &dummy_store()).unwrap();
    {
        let mut conn = db.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(delete_version_row(&tx, v.id).unwrap(), Some(p));
        assert_eq!(
            tx.query_row("SELECT refs FROM blobs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(tx); // crash before COMMIT
    }
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(refs(&db, b"a"), Some(1));
    assert!(db.unreferenced_blobs().unwrap().is_empty());
    assert_refs_exact(&db);
}

#[test]
fn migration_from_v2_builds_refs_and_drops_the_entries_blob_index() {
    let (_dir, path) = temp_db();
    let h = |c: &[u8]| BlobHash::of(c).0.to_vec();
    {
        // A v2 database with history in two projects, built with the released migrations.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema::MIGRATIONS[0]).unwrap();
        conn.execute_batch(schema::MIGRATIONS[1]).unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 2;
             INSERT INTO projects VALUES (1, 'p', '/p', '{}', 0), (2, 'q', '/q', '{}', 0);
             INSERT INTO versions (id, project_id, label, created_at_ms, kind)
                 VALUES (1, 1, 'v1', 0, 'manual'), (2, 2, 'w', 0, 'manual'),
                        (3, 1, 'v2', 0, 'manual');",
        )
        .unwrap();
        for (hash, size, failures) in [
            (h(b"shared"), 6, 0),
            (h(b"once"), 4, 0),
            (h(b"loose"), 5, 3),
        ] {
            conn.execute(
                "INSERT INTO blobs (hash, size, stored_size, prune_failures)
                 VALUES (?1, ?2, 1, ?3)",
                params![hash, size, failures],
            )
            .unwrap();
        }
        let mut entry = conn
            .prepare(
                "INSERT INTO entries (version_id, path, kind, blob_hash, size, mtime_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            )
            .unwrap();
        let none: Option<Vec<u8>> = None;
        entry
            .execute(params![1, "a", "file", h(b"shared"), 6])
            .unwrap();
        entry
            .execute(params![1, "b", "file", h(b"once"), 4])
            .unwrap();
        entry.execute(params![1, "d", "dir", none, 0]).unwrap();
        entry
            .execute(params![2, "s", "file", h(b"shared"), 6])
            .unwrap();
        entry
            .execute(params![3, "a", "file", h(b"shared"), 6])
            .unwrap();
        entry
            .execute(params![3, "c", "file", h(b"shared"), 6])
            .unwrap();
        // A reference with no blobs row (never written by released code, but possible in a
        // damaged or hand-edited file): the migration must give it a row so it is kept.
        entry
            .execute(params![3, "r", "file", h(b"rowless"), 7])
            .unwrap();
    }

    let db = Db::open(&path).unwrap();
    assert_eq!(schema::user_version(&db.conn()).unwrap(), 3);
    // shared starts runs in v1 (project p) and w (project q); v2 continues p's run.
    assert_eq!(
        (
            refs(&db, b"shared"),
            refs(&db, b"once"),
            refs(&db, b"loose"),
            refs(&db, b"rowless")
        ),
        (Some(2), Some(1), Some(0), Some(1))
    );
    assert_refs_exact(&db);
    let (size, stored): (i64, i64) = db
        .conn()
        .query_row(
            "SELECT size, stored_size FROM blobs WHERE hash = ?1",
            [h(b"rowless")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((size, stored), (7, 0));
    let names = |kind: &str| -> Vec<String> {
        let conn = db.conn();
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = ?1 AND sql IS NOT NULL
                 ORDER BY name",
            )
            .unwrap();
        stmt.query_map([kind], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(names("index"), ["blobs_unreferenced", "versions_project"]);
    assert_eq!(
        names("trigger"),
        [
            "blobs_keep_referenced",
            "entries_blob_recorded",
            "entries_immutable"
        ]
    );

    // After migrating: only "loose" is unreferenced; deleting v1 frees "once".
    assert_eq!(db.unreferenced_blobs().unwrap().len(), 1);
    db.delete_version(VersionId(1)).unwrap();
    assert_eq!(
        (refs(&db, b"shared"), refs(&db, b"once")),
        (Some(2), Some(0))
    );
    let files = FakeFiles::with(&[
        BlobHash::of(b"shared"),
        BlobHash::of(b"once"),
        BlobHash::of(b"loose"),
        BlobHash::of(b"rowless"),
    ]);
    let r = db.prune_unreferenced_with(&files, PRUNE_BATCH).unwrap();
    assert_eq!(r.blobs_pruned, 2);
    let r = db.sweep_orphans_with(&files).unwrap();
    assert_eq!(r.blobs_pruned, 0);
    assert!(files.has(&BlobHash::of(b"shared")) && files.has(&BlobHash::of(b"rowless")));
    assert_refs_exact(&db);
}
