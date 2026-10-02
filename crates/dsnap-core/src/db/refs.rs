//! Blob reference counts (schema v3, DSNA-101).
//!
//! `blobs.refs` is the number of versions in which the blob **starts a run**: versions that
//! use the blob (in any entry) while their predecessor in the same project does not. The
//! first version of a project has no predecessor. Summed over all projects:
//!
//! ```text
//! refs(b) = #{ v : b ∈ B(v) and b ∉ B(pred(v)) }        B(v) = blobs of v's entries
//! ```
//!
//! **`refs(b) > 0` exactly when some entry uses `b`.** If some version uses `b`, the oldest
//! such version of that project starts a run; conversely a run start is a version that uses
//! `b`. So the blob lifetime protocol can read `refs` wherever it used to ask the
//! `entries_blob` index.
//!
//! Counting run starts instead of entries keeps every update proportional to what changed:
//!
//! - **Insert** `v` as the newest version of its project (predecessor `p`): `+1` for each
//!   blob in `B(v) \ B(p)`. A snapshot with 49 changed files touches about 49 rows, however
//!   many versions or blobs exist.
//! - **Delete** `v` (predecessor `p`, successor `s`, either may be absent): `v`'s own term
//!   goes, `−1` for each blob in `B(v) \ B(p)`, and `s`'s term changes from
//!   `[b ∉ B(v)]` to `[b ∉ B(p)]` for each `b ∈ B(s)`. Only blobs where `v` differs from `p`
//!   or `s` change.
//! - **Delete a project**: subtract all of its run starts.
//!
//! A missed decrement can only leave `refs` too high (the blob leaks until
//! [`rebuild`]), never too low; check the cases of the delete rule. A missed increment
//! would be unsafe, so entries are written only by `Db::insert_version`, and the
//! `entries_immutable` trigger forbids changing an entry's blob afterwards.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use super::codec;
use crate::error::{Error, Result};
use crate::types::{BlobHash, ProjectId, VersionId};

/// Change in `refs` per blob. Zero entries are skipped when applied.
pub(crate) type Deltas = HashMap<BlobHash, i64>;

/// Distinct blobs of a version's entries (empty for `None`).
pub(crate) fn blob_set(conn: &Connection, version: Option<VersionId>) -> Result<HashSet<BlobHash>> {
    let Some(version) = version else {
        return Ok(HashSet::new());
    };
    let mut stmt = conn.prepare_cached(
        "SELECT blob_hash FROM entries WHERE version_id = ?1 AND blob_hash IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([version.0], |r| r.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(codec::hash_from_sql).collect()
}

/// The newest version of `project` older than `before` (any version if `None`).
pub(crate) fn predecessor(
    conn: &Connection,
    project: ProjectId,
    before: Option<VersionId>,
) -> Result<Option<VersionId>> {
    Ok(conn
        .query_row(
            "SELECT id FROM versions WHERE project_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT 1",
            params![project.0, before.map_or(i64::MAX, |v| v.0)],
            |r| r.get(0),
        )
        .optional()?
        .map(VersionId))
}

/// The oldest version of `project` newer than `after`.
fn successor(conn: &Connection, project: ProjectId, after: VersionId) -> Result<Option<VersionId>> {
    Ok(conn
        .query_row(
            "SELECT id FROM versions WHERE project_id = ?1 AND id > ?2 ORDER BY id LIMIT 1",
            params![project.0, after.0],
            |r| r.get(0),
        )
        .optional()?
        .map(VersionId))
}

/// Deltas for inserting a version with blobs `new` after the predecessor blobs `prev`.
pub(crate) fn on_insert(prev: &HashSet<BlobHash>, new: &HashSet<BlobHash>) -> Deltas {
    new.difference(prev).map(|b| (*b, 1)).collect()
}

/// Deltas for deleting `version` of `project`. Call before the version's rows are deleted.
pub(crate) fn on_delete(
    conn: &Connection,
    project: ProjectId,
    version: VersionId,
) -> Result<Deltas> {
    let prev = blob_set(conn, predecessor(conn, project, Some(version))?)?;
    let this = blob_set(conn, Some(version))?;
    let mut d = Deltas::new();
    for b in this.difference(&prev) {
        *d.entry(*b).or_default() -= 1;
    }
    if let Some(next) = successor(conn, project, version)? {
        for b in blob_set(conn, Some(next))? {
            let delta = i64::from(!prev.contains(&b)) - i64::from(!this.contains(&b));
            if delta != 0 {
                *d.entry(b).or_default() += delta;
            }
        }
    }
    Ok(d)
}

/// Run starts per blob, over one project or all of them, computed from the entries alone.
pub(crate) fn run_starts(conn: &Connection, project: Option<ProjectId>) -> Result<Deltas> {
    let mut stmt = conn.prepare_cached(
        "SELECT v.project_id, v.id, e.blob_hash FROM versions v
         JOIN entries e ON e.version_id = v.id
         WHERE (?1 IS NULL OR v.project_id = ?1) AND e.blob_hash IS NOT NULL
         ORDER BY v.id",
    )?;
    let mut rows = stmt.query([project.map(|p| p.0)])?;
    let mut last: HashMap<i64, HashSet<BlobHash>> = HashMap::new();
    let mut counts = Deltas::new();
    let mut current: Option<(i64, i64)> = None; // (project, version)
    let mut set = HashSet::new();
    let mut finish = |key: Option<(i64, i64)>, set: HashSet<BlobHash>| {
        if let Some((p, _)) = key {
            let prev = last.entry(p).or_default();
            for b in set.difference(prev) {
                *counts.entry(*b).or_default() += 1;
            }
            *prev = set;
        }
    };
    while let Some(r) = rows.next()? {
        let key = (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?);
        let hash = codec::hash_from_sql(r.get(2)?)?;
        if current != Some(key) {
            finish(current, std::mem::take(&mut set));
            current = Some(key);
        }
        set.insert(hash);
    }
    finish(current, set);
    Ok(counts)
}

/// Add `deltas` to `blobs.refs`. A missing row or a count below zero is an error (the
/// transaction must then be rolled back).
pub(crate) fn apply(conn: &Connection, deltas: &Deltas) -> Result<()> {
    let mut stmt = conn.prepare_cached("UPDATE blobs SET refs = refs + ?2 WHERE hash = ?1")?;
    for (hash, &delta) in deltas {
        if delta == 0 {
            continue;
        }
        if stmt.execute(params![hash.0.as_slice(), delta])? != 1 {
            return Err(Error::Corrupt(format!(
                "blob {hash} is referenced but has no row"
            )));
        }
    }
    Ok(())
}

/// Recompute every `refs` from the entries (schema v3 migration).
pub(crate) fn rebuild(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE blobs SET refs = 0", [])?;
    apply(conn, &run_starts(conn, None)?)
}
