//! Agent turns and their review decisions (DSNA-117). Owner: Chain T (DSNA-113).
//!
//! A turn row pairs a before and an after version. Both columns are `ON DELETE SET NULL`,
//! so deleting a version (through `delete_version_row`) needs nothing extra here; the turn
//! then reads as [`TurnState::Pruned`].

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::{Db, now_ms};
use crate::error::{Error, Result};
use crate::types::{
    ChangeCounts, FileDecision, ProjectId, RelPath, Turn, TurnId, TurnState, VersionId,
};

/// Pending turns whose versions retention keeps, newest first. Older pending turns may lose
/// their versions, so a user who never reviews does not keep every agent turn forever.
pub(crate) const PROTECTED_PENDING_TURNS: u32 = 20;

/// Columns read by [`turn_from_row`]; `t` is the `turns` alias.
const TURN_COLS: &str = "t.id, t.project_id, t.seq, t.session_id, t.prompt, t.started_at_ms, \
     t.ended_at_ms, t.before_version, t.after_version, t.added, t.modified, t.deleted, \
     t.reviewed_at_ms, \
     (SELECT COUNT(*) FROM turn_files f WHERE f.turn_id = t.id AND f.decision = 'kept'), \
     (SELECT COUNT(*) FROM turn_files f WHERE f.turn_id = t.id AND f.decision = 'undone')";

fn u32_col(row: &Row<'_>, i: usize) -> rusqlite::Result<u32> {
    let v: i64 = row.get(i)?;
    u32::try_from(v).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            i,
            rusqlite::types::Type::Integer,
            format!("{v} out of range").into(),
        )
    })
}

fn turn_from_row(row: &Row<'_>) -> rusqlite::Result<Turn> {
    let ended_at_ms: Option<i64> = row.get(6)?;
    let before: Option<i64> = row.get(7)?;
    let after: Option<i64> = row.get(8)?;
    let counts = ChangeCounts {
        added: u32_col(row, 9)?,
        modified: u32_col(row, 10)?,
        deleted: u32_col(row, 11)?,
    };
    let reviewed_at_ms: Option<i64> = row.get(12)?;
    let state = if ended_at_ms.is_none() {
        TurnState::Running
    } else if before.is_none() || after.is_none() {
        TurnState::Pruned
    } else if before == after || counts.added + counts.modified + counts.deleted == 0 {
        TurnState::Empty
    } else if reviewed_at_ms.is_some() {
        TurnState::Reviewed
    } else {
        TurnState::Pending
    };
    Ok(Turn {
        id: TurnId(row.get(0)?),
        project_id: ProjectId(row.get(1)?),
        seq: u32_col(row, 2)?,
        session_id: row.get(3)?,
        prompt: row.get(4)?,
        started_at_ms: row.get(5)?,
        ended_at_ms,
        before: before.map(VersionId),
        after: after.map(VersionId),
        counts,
        kept: u32_col(row, 13)?,
        undone: u32_col(row, 14)?,
        reviewed_at_ms,
        state,
    })
}

fn turn_by_id(conn: &Connection, id: TurnId) -> Result<Turn> {
    conn.query_row(
        &format!("SELECT {TURN_COLS} FROM turns t WHERE t.id = ?1"),
        [id.0],
        turn_from_row,
    )
    .optional()?
    .ok_or_else(|| not_found_turn(id))
}

pub(crate) fn not_found_turn(id: TurnId) -> Error {
    Error::NotFound(format!("turn {id}"))
}

fn decision_to_sql(d: FileDecision) -> &'static str {
    match d {
        FileDecision::Kept => "kept",
        FileDecision::Undone => "undone",
    }
}

fn decision_from_sql(s: &str) -> Result<FileDecision> {
    match s {
        "kept" => Ok(FileDecision::Kept),
        "undone" => Ok(FileDecision::Undone),
        other => Err(Error::Corrupt(format!("unknown turn decision {other:?}"))),
    }
}

/// Versions retention must keep for `project`: both sides of its newest
/// [`PROTECTED_PENDING_TURNS`] turns that are running or wait for review.
pub(super) fn pending_turn_versions_tx(
    tx: &Transaction<'_>,
    project: ProjectId,
) -> Result<Vec<VersionId>> {
    let mut stmt = tx.prepare_cached(
        "SELECT before_version, after_version FROM turns
         WHERE project_id = ?1 AND reviewed_at_ms IS NULL
           AND (ended_at_ms IS NULL OR added + modified + deleted > 0)
         ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![project.0, PROTECTED_PENDING_TURNS], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .flat_map(|(a, b)| [a, b])
        .flatten()
        .map(VersionId)
        .collect())
}

impl Db {
    /// Start a turn: the next `seq` in the project, `before` as its before version.
    pub fn insert_turn(
        &self,
        project: ProjectId,
        session_id: Option<&str>,
        prompt: &str,
        started_at_ms: i64,
        before: Option<VersionId>,
    ) -> Result<Turn> {
        self.write(|tx| {
            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE project_id = ?1",
                [project.0],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO turns (project_id, seq, session_id, prompt, started_at_ms,
                                    before_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    project.0,
                    seq,
                    session_id,
                    prompt,
                    started_at_ms,
                    before.map(|v| v.0)
                ],
            )?;
            turn_by_id(tx, TurnId(tx.last_insert_rowid()))
        })
    }

    /// End a running turn with its after version and before→after counts.
    pub fn end_turn_row(
        &self,
        id: TurnId,
        ended_at_ms: i64,
        after: VersionId,
        counts: ChangeCounts,
    ) -> Result<Turn> {
        self.write(|tx| {
            let n = tx.execute(
                "UPDATE turns SET ended_at_ms = ?2, after_version = ?3,
                                  added = ?4, modified = ?5, deleted = ?6
                 WHERE id = ?1 AND ended_at_ms IS NULL",
                params![
                    id.0,
                    ended_at_ms,
                    after.0,
                    counts.added,
                    counts.modified,
                    counts.deleted
                ],
            )?;
            if n == 0 {
                // Unknown, or another hook ended it first.
                return Err(not_found_turn(id));
            }
            turn_by_id(tx, id)
        })
    }

    /// The newest running turn of `project` for `session_id`; with `None`, of any session.
    pub fn running_turn(
        &self,
        project: ProjectId,
        session_id: Option<&str>,
    ) -> Result<Option<Turn>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {TURN_COLS} FROM turns t
             WHERE t.project_id = ?1 AND t.ended_at_ms IS NULL
               AND (?2 IS NULL OR t.session_id = ?2)
             ORDER BY t.id DESC LIMIT 1"
        );
        Ok(conn
            .query_row(&sql, params![project.0, session_id], turn_from_row)
            .optional()?)
    }

    /// A project's turns, newest first.
    pub fn list_turns(&self, project: ProjectId) -> Result<Vec<Turn>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {TURN_COLS} FROM turns t WHERE t.project_id = ?1 ORDER BY t.id DESC"
        ))?;
        let rows = stmt
            .query_map([project.0], turn_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// One turn.
    pub fn get_turn(&self, id: TurnId) -> Result<Turn> {
        turn_by_id(&self.conn(), id)
    }

    /// The decisions recorded on a turn's files.
    pub fn turn_decisions(&self, id: TurnId) -> Result<HashMap<RelPath, FileDecision>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare_cached("SELECT path, decision FROM turn_files WHERE turn_id = ?1")?;
        let rows = stmt
            .query_map([id.0], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(p, d)| Ok((RelPath::new(p)?, decision_from_sql(&d)?)))
            .collect()
    }

    /// Set (`Some`) or clear (`None`) decisions on a turn's files, then mark the turn
    /// reviewed when every path in `files` has a decision, or pending again when one lacks
    /// it. One transaction.
    pub fn decide_turn_files(
        &self,
        id: TurnId,
        decisions: &[(RelPath, Option<FileDecision>)],
        files: &[RelPath],
    ) -> Result<Turn> {
        let now = now_ms();
        self.write(|tx| {
            let turn = turn_by_id(tx, id)?;
            for (path, d) in decisions {
                match d {
                    Some(d) => tx.execute(
                        "INSERT INTO turn_files (turn_id, path, decision, decided_at_ms)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(turn_id, path) DO UPDATE
                         SET decision = excluded.decision, decided_at_ms = excluded.decided_at_ms",
                        params![id.0, path.as_str(), decision_to_sql(*d), now],
                    )?,
                    None => tx.execute(
                        "DELETE FROM turn_files WHERE turn_id = ?1 AND path = ?2",
                        params![id.0, path.as_str()],
                    )?,
                };
            }
            let mut decided = 0usize;
            {
                let mut stmt =
                    tx.prepare_cached("SELECT 1 FROM turn_files WHERE turn_id = ?1 AND path = ?2")?;
                for p in files {
                    if stmt.exists(params![id.0, p.as_str()])? {
                        decided += 1;
                    }
                }
            }
            let reviewed = !files.is_empty() && decided == files.len();
            let reviewed_at = match (reviewed, turn.reviewed_at_ms) {
                (true, Some(t)) => Some(t),
                (true, None) => Some(now),
                (false, _) => None,
            };
            tx.execute(
                "UPDATE turns SET reviewed_at_ms = ?2 WHERE id = ?1",
                params![id.0, reviewed_at],
            )?;
            turn_by_id(tx, id)
        })
    }
}
