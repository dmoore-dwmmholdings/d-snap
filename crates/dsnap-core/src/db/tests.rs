//! Unit tests for the index database.

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
