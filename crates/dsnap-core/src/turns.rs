//! Agent turns: the before/after snapshots of one Claude Code prompt, reviewed file by file.
//! Owner: Chain T (DSNA-113).
//!
//! The `UserPromptSubmit` hook calls [`Dsnap::begin_turn`] and the `Stop` hook calls
//! [`Dsnap::end_turn`]. Each takes an ordinary snapshot (nothing is written when nothing
//! changed, F7) and records which version the turn starts and ends at.
//!
//! Review: every file the turn changed can be kept or undone. Undoing restores the files to
//! the turn's before version through the restore engine, so a safety snapshot is always taken
//! first (Rule 1). A file that changed after the turn is skipped unless the caller opts in,
//! because undoing it would also discard those later edits (DSNA-120).

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::diff::entries::{EntryDiffOptions, diff_entries};
use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::types::{
    ChangeCounts, ChangeStatus, DiffOptions, Entry, EntryKind, FileChange, FileDecision,
    PathVersion, ProjectId, RelPath, RestoreReport, RevertHunk, SnapshotOptions, SnapshotReport,
    Turn, TurnFile, TurnId, TurnState, UndoTurn, UndoTurnReport, VersionId, VersionRef,
};
use crate::versions::version_counts;

/// Longest stored prompt in characters; longer prompts are cut (the UI shows the start).
pub const MAX_PROMPT_CHARS: usize = 4000;

/// How a turn starts ([`Dsnap::begin_turn`]).
#[derive(Debug, Clone, Default)]
pub struct TurnStart {
    /// Claude Code session id, so concurrent sessions on one folder pair up correctly.
    pub session_id: Option<String>,
    /// The prompt the user submitted.
    pub prompt: String,
}

impl Dsnap {
    /// Snapshot the folder as a turn starts and record the turn (`UserPromptSubmit`).
    ///
    /// The turn's before version is the new version, or the latest one when nothing changed.
    /// A turn of the same session that is still running (interrupted, so it never got a
    /// `Stop`) is ended first, at this same version.
    ///
    /// Errors: those of [`Dsnap::snapshot`]. The snapshot is committed before the turn row;
    /// if recording the turn fails, the version stays.
    pub fn begin_turn(
        &self,
        project: ProjectId,
        start: TurnStart,
        opts: SnapshotOptions,
    ) -> Result<(SnapshotReport, Turn)> {
        let report = self.snapshot(project, opts)?;
        let before = self.current_version(project, &report)?;
        let session = start.session_id.as_deref();
        if let Some(open) = self.db.running_turn(project, session)? {
            // Ended by another hook meanwhile is fine: NotFound is ignored.
            match self.finish_turn(&open, before) {
                Ok(_) | Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let prompt = clean_prompt(&start.prompt);
        let turn = self
            .db
            .insert_turn(project, session, &prompt, now_ms(), Some(before))?;
        Ok((report, turn))
    }

    /// Snapshot the folder as the agent stops and end the running turn (`Stop`).
    ///
    /// Ends the newest running turn of `session_id`, or, when that session has none, the
    /// project's newest running turn. With no running turn it only snapshots and returns
    /// `None` (a second `Stop`, or hooks installed mid-turn).
    pub fn end_turn(
        &self,
        project: ProjectId,
        session_id: Option<&str>,
        opts: SnapshotOptions,
    ) -> Result<(SnapshotReport, Option<Turn>)> {
        let report = self.snapshot(project, opts)?;
        let after = self.current_version(project, &report)?;
        let open = match self.db.running_turn(project, session_id)? {
            Some(t) => Some(t),
            None if session_id.is_some() => self.db.running_turn(project, None)?,
            None => None,
        };
        let Some(open) = open else {
            return Ok((report, None));
        };
        match self.finish_turn(&open, after) {
            Ok(t) => Ok((report, Some(t))),
            Err(Error::NotFound(_)) => Ok((report, None)),
            Err(e) => Err(e),
        }
    }

    /// A project's turns, newest first.
    pub fn list_turns(&self, project: ProjectId) -> Result<Vec<Turn>> {
        self.db.get_project(project)?;
        self.db.list_turns(project)
    }

    /// One turn.
    pub fn turn(&self, id: TurnId) -> Result<Turn> {
        self.db.get_turn(id)
    }

    /// The files a turn changed, with line counts, decisions and whether each changed again
    /// since. A running turn lists its changes so far (before → folder now).
    ///
    /// Errors: [`Error::InvalidInput`] for a pruned turn, [`Error::NotFound`].
    pub fn turn_files(&self, id: TurnId) -> Result<Vec<TurnFile>> {
        let turn = self.db.get_turn(id)?;
        let before = turn.before.ok_or_else(|| pruned(&turn))?;
        let decisions = self.db.turn_decisions(id)?;
        let Some(after) = turn.after.filter(|_| turn.state != TurnState::Running) else {
            if turn.state == TurnState::Pruned {
                return Err(pruned(&turn));
            }
            let changes = self.changes(turn.project_id, Some(before), VersionRef::WorkingTree)?;
            return Ok(changes
                .into_iter()
                .map(|change| TurnFile {
                    decision: None,
                    changed_since: false,
                    change,
                })
                .collect());
        };
        let changes = self.changes(turn.project_id, Some(before), VersionRef::Version(after))?;
        let working = self.working_index(turn.project_id)?;
        Ok(changes
            .into_iter()
            .map(|change| TurnFile {
                decision: decisions.get(&change.path).copied(),
                changed_since: working.as_ref().is_some_and(|w| changed_since(&change, w)),
                change,
            })
            .collect())
    }

    /// Set or clear (`None`) decisions on files of a turn. The turn is reviewed once every
    /// file has one, and pending again when one is cleared.
    ///
    /// Errors: [`Error::InvalidInput`] if a path is not one of the turn's files, or the turn
    /// is running or pruned.
    pub fn set_turn_decisions(
        &self,
        id: TurnId,
        decisions: &[(RelPath, Option<FileDecision>)],
    ) -> Result<Turn> {
        let turn = self.db.get_turn(id)?;
        let paths = self.turn_paths(&turn)?;
        if let Some((p, _)) = decisions.iter().find(|(p, _)| !paths.contains(p)) {
            return Err(Error::InvalidInput(format!(
                "{p} is not a file of Turn {}",
                turn.seq
            )));
        }
        self.db.decide_turn_files(id, decisions, &paths)
    }

    /// Keep every file of a turn that has no decision yet.
    pub fn keep_turn(&self, id: TurnId) -> Result<Turn> {
        let turn = self.db.get_turn(id)?;
        let paths = self.turn_paths(&turn)?;
        let decided = self.db.turn_decisions(id)?;
        let keep: Vec<_> = paths
            .iter()
            .filter(|p| !decided.contains_key(*p))
            .map(|p| (p.clone(), Some(FileDecision::Kept)))
            .collect();
        self.db.decide_turn_files(id, &keep, &paths)
    }

    /// Put files of a turn back as they were before it (all of them, or `req.paths`), in one
    /// restore with one safety snapshot ("Before undoing Turn N").
    ///
    /// Files that changed after the turn are skipped and listed in `skipped_changed` unless
    /// `req.include_changed` is set. Files the turn added are deleted; a rename is undone on
    /// both paths. Every file written or deleted is recorded as undone.
    ///
    /// Errors: [`Error::InvalidInput`] when the turn is running or pruned, a path is not one
    /// of its files, or nothing is left to undo; the errors of [`Dsnap::restore_project`].
    pub fn undo_turn(&self, id: TurnId, req: &UndoTurn) -> Result<UndoTurnReport> {
        let turn = self.ended_turn(id)?;
        let before = turn.before.ok_or_else(|| pruned(&turn))?;
        let files = self.turn_files(id)?;
        let selected = select(&turn, &files, req.paths.as_deref())?;
        let (undo, skipped_changed): (Vec<&TurnFile>, Vec<&TurnFile>) = selected
            .into_iter()
            .partition(|f| req.include_changed || !f.changed_since);
        if undo.is_empty() {
            return Err(Error::InvalidInput(
                "nothing to undo: every selected file changed after the turn".into(),
            ));
        }
        let scope = scope_of(&undo);
        let restore = self.restore_scoped(
            turn.project_id,
            before,
            Some(&scope),
            format!("Before undoing Turn {}", turn.seq),
        )?;
        let turn = self.record_after_restore(&turn, &undo, &restore, Some(FileDecision::Undone))?;
        Ok(UndoTurnReport {
            restore,
            skipped_changed: skipped_changed
                .into_iter()
                .map(|f| f.change.path.clone())
                .collect(),
            turn,
        })
    }

    /// Put undone files of a turn back as the turn left them (all undone files, or
    /// `paths`), with one safety snapshot ("Before putting back Turn N"). Their decisions
    /// are cleared, so the turn waits for review again.
    pub fn redo_turn(&self, id: TurnId, paths: Option<&[RelPath]>) -> Result<UndoTurnReport> {
        let turn = self.ended_turn(id)?;
        let after = turn.after.ok_or_else(|| pruned(&turn))?;
        let files = self.turn_files(id)?;
        let selected: Vec<&TurnFile> = select(&turn, &files, paths)?
            .into_iter()
            .filter(|f| paths.is_some() || f.decision == Some(FileDecision::Undone))
            .collect();
        if selected.is_empty() {
            return Err(Error::InvalidInput(format!(
                "nothing of Turn {} is undone",
                turn.seq
            )));
        }
        let scope = scope_of(&selected);
        let restore = self.restore_scoped(
            turn.project_id,
            after,
            Some(&scope),
            format!("Before putting back Turn {}", turn.seq),
        )?;
        let turn = self.record_after_restore(&turn, &selected, &restore, None)?;
        Ok(UndoTurnReport {
            restore,
            skipped_changed: Vec::new(),
            turn,
        })
    }

    /// Undo hunk `hunk_index` of a turn's diff of `path` (before → after, with `opts`) in the
    /// folder, with a safety snapshot first. No decision is recorded: the file stays pending.
    ///
    /// Only allowed while the folder's file is exactly as the turn left it, so the hunk is
    /// the one the user saw. Errors: [`Error::InvalidInput`] if the file changed since (undo
    /// the whole file instead), and the errors of [`Dsnap::revert_hunk`].
    pub fn undo_turn_hunk(
        &self,
        id: TurnId,
        path: &RelPath,
        hunk_index: u32,
        opts: &DiffOptions,
    ) -> Result<RestoreReport> {
        let turn = self.ended_turn(id)?;
        let before = turn.before.ok_or_else(|| pruned(&turn))?;
        let files = self.turn_files(id)?;
        let file = files
            .iter()
            .find(|f| &f.change.path == path)
            .ok_or_else(|| not_a_file(&turn, path))?;
        if file.changed_since {
            return Err(Error::InvalidInput(format!(
                "{path} changed after Turn {}; undo the whole file instead",
                turn.seq
            )));
        }
        let req = RevertHunk {
            from: Some(before),
            to: VersionRef::WorkingTree,
            path: path.clone(),
            hunk_index,
            opts: *opts,
            working_hash: file.change.new.as_ref().and_then(|e| e.blob),
        };
        self.revert_hunk_labelled(
            turn.project_id,
            &req,
            Some(format!("Before undoing part of Turn {}", turn.seq)),
        )
    }

    /// Every version where `path` changed, newest first: its entry there, or `None` where it
    /// was deleted. One lookup per version; nothing scans entries by path.
    pub fn path_history(&self, project: ProjectId, path: &RelPath) -> Result<Vec<PathVersion>> {
        let versions = self.list_versions(project)?;
        let mut out = Vec::new();
        let mut prev: Option<Entry> = None;
        for v in versions.into_iter().rev() {
            let entry = self.db.entry(v.id, path)?;
            let changed = match (&prev, &entry) {
                (None, None) => false,
                (Some(a), Some(b)) => !same_content(a, b),
                _ => true,
            };
            if changed {
                out.push(PathVersion {
                    version: v,
                    entry: entry.clone(),
                });
            }
            prev = entry;
        }
        out.reverse();
        Ok(out)
    }

    /// Files and links in the latest version plus the paths the folder added since, sorted.
    /// A missing folder lists the latest version only.
    pub fn known_paths(&self, project: ProjectId) -> Result<Vec<RelPath>> {
        let mut paths: HashSet<RelPath> = HashSet::new();
        if let Some(v) = self.db.latest_version(project)? {
            paths.extend(
                self.db
                    .entries(v.id)?
                    .into_iter()
                    .filter(|e| e.kind != EntryKind::Dir)
                    .map(|e| e.path),
            );
        } else {
            self.db.get_project(project)?;
        }
        match self.status(project) {
            Ok(changes) => paths.extend(
                changes
                    .into_iter()
                    .filter(|c| c.new.as_ref().is_some_and(|e| e.kind != EntryKind::Dir))
                    .map(|c| c.path),
            ),
            Err(Error::ProjectMissing { .. }) => {}
            Err(e) => return Err(e),
        }
        let mut out: Vec<RelPath> = paths.into_iter().collect();
        out.sort();
        Ok(out)
    }

    // ---------- helpers ----------

    /// The version the folder is at after `report`: the new one, or the latest.
    fn current_version(&self, project: ProjectId, report: &SnapshotReport) -> Result<VersionId> {
        match &report.version {
            Some(v) => Ok(v.id),
            None => self
                .db
                .latest_version(project)?
                .map(|v| v.id)
                .ok_or_else(|| Error::Corrupt("snapshot left the project without versions".into())),
        }
    }

    /// End `turn` at `after`, storing its before → after counts.
    fn finish_turn(&self, turn: &Turn, after: VersionId) -> Result<Turn> {
        let counts = match turn.before {
            Some(b) if b != after => match (self.db.entries(b), self.db.entries(after)) {
                (Ok(old), Ok(new)) => version_counts(&old, &new),
                // The before version was deleted meanwhile: the turn reads as pruned.
                (Err(Error::NotFound(_)), _) => ChangeCounts::default(),
                (Err(e), _) | (_, Err(e)) => return Err(e),
            },
            _ => ChangeCounts::default(),
        };
        self.db.end_turn_row(turn.id, now_ms(), after, counts)
    }

    /// The turn, if it has ended with both versions present.
    fn ended_turn(&self, id: TurnId) -> Result<Turn> {
        let turn = self.db.get_turn(id)?;
        match turn.state {
            TurnState::Running => Err(Error::InvalidInput(format!(
                "Turn {} is still running",
                turn.seq
            ))),
            TurnState::Pruned => Err(pruned(&turn)),
            _ => Ok(turn),
        }
    }

    /// Paths a turn changed (before → after), without line counts.
    fn turn_paths(&self, turn: &Turn) -> Result<Vec<RelPath>> {
        let turn = self.ended_turn(turn.id)?;
        let (Some(b), Some(a)) = (turn.before, turn.after) else {
            return Err(pruned(&turn));
        };
        if a == b {
            return Ok(Vec::new());
        }
        let changes = diff_entries(
            &self.db.entries(b)?,
            &self.db.entries(a)?,
            EntryDiffOptions::default(),
        );
        Ok(changes.into_iter().map(|c| c.path).collect())
    }

    /// The folder's entries by path, or `None` when the folder is missing.
    fn working_index(&self, project: ProjectId) -> Result<Option<HashMap<RelPath, Entry>>> {
        match self.working_entries(project) {
            Ok(entries) => Ok(Some(
                entries.into_iter().map(|e| (e.path.clone(), e)).collect(),
            )),
            Err(Error::ProjectMissing { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Record `decision` for every file whose paths the restore neither failed nor left
    /// uncaptured.
    fn record_after_restore(
        &self,
        turn: &Turn,
        files: &[&TurnFile],
        restore: &RestoreReport,
        decision: Option<FileDecision>,
    ) -> Result<Turn> {
        let bad: HashSet<&RelPath> = restore
            .failed
            .iter()
            .map(|(p, _)| p)
            .chain(&restore.uncaptured)
            .collect();
        let decisions: Vec<(RelPath, Option<FileDecision>)> = files
            .iter()
            .filter(|f| paths_of(&f.change).iter().all(|p| !bad.contains(p)))
            .map(|f| (f.change.path.clone(), decision))
            .collect();
        let paths = self.turn_paths(turn)?;
        self.db.decide_turn_files(turn.id, &decisions, &paths)
    }
}

/// The files of `files` named in `paths` (all when `None`).
fn select<'a>(
    turn: &Turn,
    files: &'a [TurnFile],
    paths: Option<&[RelPath]>,
) -> Result<Vec<&'a TurnFile>> {
    let Some(paths) = paths else {
        return Ok(files.iter().collect());
    };
    paths
        .iter()
        .map(|p| {
            files
                .iter()
                .find(|f| &f.change.path == p)
                .ok_or_else(|| not_a_file(turn, p))
        })
        .collect()
}

/// Both paths of a change (a rename has two).
fn paths_of(c: &FileChange) -> Vec<RelPath> {
    match &c.status {
        ChangeStatus::Renamed { from } => vec![c.path.clone(), from.clone()],
        _ => vec![c.path.clone()],
    }
}

/// The restore scope for these files.
fn scope_of(files: &[&TurnFile]) -> Vec<RelPath> {
    let mut scope: Vec<RelPath> = files.iter().flat_map(|f| paths_of(&f.change)).collect();
    scope.sort();
    scope.dedup();
    scope
}

/// Whether the folder no longer holds what the turn left at this change's paths.
fn changed_since(c: &FileChange, working: &HashMap<RelPath, Entry>) -> bool {
    let differs = |expected: Option<&Entry>, path: &RelPath| match (expected, working.get(path)) {
        (None, None) => false,
        (Some(a), Some(b)) => !same_content(a, b),
        _ => true,
    };
    match &c.status {
        ChangeStatus::Renamed { from } => differs(c.new.as_ref(), &c.path) || differs(None, from),
        _ => differs(c.new.as_ref(), &c.path),
    }
}

/// Same kind and content (directories compare by kind; a file without a hash compares by
/// size, as an unreadable working file has none).
fn same_content(a: &Entry, b: &Entry) -> bool {
    if a.kind != b.kind {
        return false;
    }
    match a.kind {
        EntryKind::Dir | EntryKind::Symlink { .. } => true,
        EntryKind::File => match (a.blob, b.blob) {
            (Some(x), Some(y)) => x == y,
            _ => a.size == b.size && a.mtime_ns == b.mtime_ns,
        },
    }
}

fn clean_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    match trimmed.char_indices().nth(MAX_PROMPT_CHARS) {
        Some((cut, _)) => trimmed[..cut].trim_end().to_owned(),
        None => trimmed.to_owned(),
    }
}

fn pruned(turn: &Turn) -> Error {
    Error::InvalidInput(format!(
        "the snapshots of Turn {} were deleted, so it cannot be compared or undone",
        turn.seq
    ))
}

fn not_a_file(turn: &Turn, path: &RelPath) -> Error {
    Error::InvalidInput(format!("{path} is not a file of Turn {}", turn.seq))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_trimmed_and_capped() {
        assert_eq!(clean_prompt("  fix it \n"), "fix it");
        let long = "é".repeat(MAX_PROMPT_CHARS + 10);
        assert_eq!(clean_prompt(&long).chars().count(), MAX_PROMPT_CHARS);
    }
}
