//! Agent turns end to end (Chain T: DSNA-117, DSNA-118, DSNA-120, DSNA-121).
#![allow(clippy::unwrap_used)] // helpers outside #[test] fns are test code too

#[path = "support/project.rs"]
mod project;

use std::fs;

use dsnap_core::{
    ChangeStatus, DiffBody, DiffOptions, Error, FileDecision, GlobalSettings, SnapshotOptions,
    Turn, TurnStart, TurnState, UndoTurn, VersionKind, VersionRef,
};
use dsnap_test_support::FixtureProject;

use project::{Env, rewrite, rp, snap};

fn cli() -> SnapshotOptions {
    SnapshotOptions {
        kind: VersionKind::Cli,
        ..SnapshotOptions::default()
    }
}

fn begin(env: &Env, session: &str, prompt: &str) -> Turn {
    env.dsnap
        .begin_turn(
            env.project,
            TurnStart {
                session_id: Some(session.into()),
                prompt: prompt.into(),
            },
            cli(),
        )
        .unwrap()
        .1
}

fn end(env: &Env, session: &str) -> Option<Turn> {
    env.dsnap
        .end_turn(env.project, Some(session), cli())
        .unwrap()
        .1
}

fn read(fx: &dsnap_test_support::Fixture, p: &str) -> Option<String> {
    fs::read_to_string(fx.path(p)).ok()
}

fn decisions(env: &Env, t: &Turn) -> Vec<(String, Option<FileDecision>, bool)> {
    env.dsnap
        .turn_files(t.id)
        .unwrap()
        .into_iter()
        .map(|f| {
            (
                f.change.path.as_str().to_owned(),
                f.decision,
                f.changed_since,
            )
        })
        .collect()
}

/// A turn that edits a.txt, adds new.txt and deletes gone.txt.
fn agent_turn(fx: &dsnap_test_support::Fixture, env: &Env) -> Turn {
    begin(env, "s1", "  Add the payment step \n");
    rewrite(&fx.path("a.txt"), b"a1\nagent\n");
    fs::write(fx.path("new.txt"), "new\n").unwrap();
    fs::remove_file(fx.path("gone.txt")).unwrap();
    end(env, "s1").unwrap()
}

fn fixture() -> dsnap_test_support::Fixture {
    FixtureProject::new()
        .file("a.txt", "a1\n")
        .file("gone.txt", "bye\n")
        .file("keep.txt", "k\n")
        .build()
}

#[test]
fn a_turn_pairs_the_hook_snapshots_and_stores_the_prompt() {
    let fx = fixture();
    let env = Env::new(fx.root());
    snap(&env, Some("baseline"));
    let t = agent_turn(&fx, &env);
    assert_eq!(t.seq, 1);
    assert_eq!(t.prompt, "Add the payment step");
    assert_eq!(t.session_id.as_deref(), Some("s1"));
    assert_eq!(t.state, TurnState::Pending);
    assert_eq!(
        (t.counts.added, t.counts.modified, t.counts.deleted),
        (1, 1, 1)
    );
    assert!(t.before.is_some() && t.after.is_some() && t.before != t.after);
    // Nothing changed before the prompt: the turn starts at the latest version.
    let versions = env.db.list_versions(env.project).unwrap();
    assert_eq!(t.before, Some(versions[1].id));
    assert_eq!(t.after, Some(versions[0].id));

    let listed = env.dsnap.list_turns(env.project).unwrap();
    assert_eq!(listed, vec![t]);
}

#[test]
fn a_turn_without_changes_is_empty_and_a_second_stop_starts_nothing() {
    let fx = fixture();
    let env = Env::new(fx.root());
    begin(&env, "s1", "explain the code");
    let t = end(&env, "s1").unwrap();
    assert_eq!(t.state, TurnState::Empty);
    assert!(end(&env, "s1").is_none());
}

#[test]
fn a_new_prompt_ends_an_interrupted_turn_of_the_same_session() {
    let fx = fixture();
    let env = Env::new(fx.root());
    let t1 = begin(&env, "s1", "first");
    rewrite(&fx.path("a.txt"), b"interrupted\n");
    let t2 = begin(&env, "s1", "second");
    let t1 = env.dsnap.turn(t1.id).unwrap();
    assert_eq!(t1.state, TurnState::Pending);
    assert_eq!(t1.after, t2.before);
    assert_eq!(t2.seq, 2);
    assert_eq!(t2.state, TurnState::Running);
}

#[test]
fn stop_from_an_unknown_session_ends_the_projects_running_turn() {
    let fx = fixture();
    let env = Env::new(fx.root());
    begin(&env, "s1", "go");
    rewrite(&fx.path("a.txt"), b"x\n");
    let t = env
        .dsnap
        .end_turn(env.project, Some("other"), cli())
        .unwrap()
        .1
        .unwrap();
    assert_eq!(t.state, TurnState::Pending);
}

#[test]
fn turn_files_report_decisions_and_later_edits() {
    let fx = fixture();
    let env = Env::new(fx.root());
    let t = agent_turn(&fx, &env);
    let files = env.dsnap.turn_files(t.id).unwrap();
    let paths: Vec<_> = files.iter().map(|f| f.change.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "gone.txt", "new.txt"]);
    assert!(
        files
            .iter()
            .all(|f| f.decision.is_none() && !f.changed_since)
    );
    let a = &files[0];
    assert_eq!(
        (a.change.lines_added, a.change.lines_removed),
        (Some(1), Some(0))
    );

    rewrite(&fx.path("a.txt"), b"a1\nagent\nmine\n");
    let changed: Vec<_> = decisions(&env, &t).into_iter().map(|d| d.2).collect();
    assert_eq!(changed, [true, false, false]);
}

#[test]
fn deciding_every_file_reviews_the_turn_and_clearing_one_reopens_it() {
    let fx = fixture();
    let env = Env::new(fx.root());
    let t = agent_turn(&fx, &env);
    let t = env
        .dsnap
        .set_turn_decisions(t.id, &[(rp("a.txt"), Some(FileDecision::Kept))])
        .unwrap();
    assert_eq!((t.state, t.kept), (TurnState::Pending, 1));
    let t = env.dsnap.keep_turn(t.id).unwrap();
    assert_eq!((t.state, t.kept, t.undone), (TurnState::Reviewed, 3, 0));
    assert!(t.reviewed_at_ms.is_some());
    let t = env
        .dsnap
        .set_turn_decisions(t.id, &[(rp("new.txt"), None)])
        .unwrap();
    assert_eq!(t.state, TurnState::Pending);
    let err = env
        .dsnap
        .set_turn_decisions(t.id, &[(rp("keep.txt"), Some(FileDecision::Kept))])
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
}

#[test]
fn undo_turn_puts_files_back_and_deletes_added_ones() {
    let fx = fixture();
    let env = Env::new(fx.root());
    let t = agent_turn(&fx, &env);
    let r = env.dsnap.undo_turn(t.id, &UndoTurn::default()).unwrap();
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("a1\n"));
    assert_eq!(read(&fx, "gone.txt").as_deref(), Some("bye\n"));
    assert_eq!(read(&fx, "new.txt"), None);
    assert_eq!(read(&fx, "keep.txt").as_deref(), Some("k\n"));
    assert!(r.skipped_changed.is_empty());
    assert_eq!((r.turn.undone, r.turn.state), (3, TurnState::Reviewed));
    let safety = env.db.get_version(r.restore.safety_version).unwrap();
    assert_eq!(safety.kind, VersionKind::Safety);
    assert_eq!(safety.label, "Before undoing Turn 1");

    // Redo puts the turn's result back and reopens the review.
    let r = env.dsnap.redo_turn(t.id, None).unwrap();
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("a1\nagent\n"));
    assert_eq!(read(&fx, "new.txt").as_deref(), Some("new\n"));
    assert_eq!(read(&fx, "gone.txt"), None);
    assert_eq!((r.turn.undone, r.turn.state), (0, TurnState::Pending));
}

#[test]
fn undo_turn_skips_files_edited_since_unless_asked() {
    let fx = fixture();
    let env = Env::new(fx.root());
    let t = agent_turn(&fx, &env);
    rewrite(&fx.path("a.txt"), b"a1\nagent\nmine\n");

    let r = env.dsnap.undo_turn(t.id, &UndoTurn::default()).unwrap();
    assert_eq!(r.skipped_changed, [rp("a.txt")]);
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("a1\nagent\nmine\n"));
    assert_eq!(read(&fx, "new.txt"), None);
    assert_eq!((r.turn.undone, r.turn.state), (2, TurnState::Pending));

    let r = env
        .dsnap
        .undo_turn(
            t.id,
            &UndoTurn {
                paths: Some(vec![rp("a.txt")]),
                include_changed: true,
            },
        )
        .unwrap();
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("a1\n"));
    assert_eq!(r.turn.state, TurnState::Reviewed);
    // The later edit is in the safety version.
    let safety = r.restore.safety_version;
    env.dsnap
        .restore_file(env.project, safety, &rp("a.txt"))
        .unwrap();
    assert_eq!(read(&fx, "a.txt").as_deref(), Some("a1\nagent\nmine\n"));
}

#[test]
fn undo_turn_with_only_changed_files_is_refused() {
    let fx = FixtureProject::new().file("a.txt", "1\n").build();
    let env = Env::new(fx.root());
    begin(&env, "s", "p");
    rewrite(&fx.path("a.txt"), b"2\n");
    let t = end(&env, "s").unwrap();
    rewrite(&fx.path("a.txt"), b"3\n");
    let err = env.dsnap.undo_turn(t.id, &UndoTurn::default()).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
}

#[test]
fn undo_turn_reverses_a_rename_on_both_paths() {
    let fx = FixtureProject::new().file("old.txt", "same\n").build();
    let env = Env::new(fx.root());
    begin(&env, "s", "rename it");
    fs::rename(fx.path("old.txt"), fx.path("new.txt")).unwrap();
    let t = end(&env, "s").unwrap();
    let files = env.dsnap.turn_files(t.id).unwrap();
    assert!(matches!(
        files[0].change.status,
        ChangeStatus::Renamed { .. }
    ));
    env.dsnap.undo_turn(t.id, &UndoTurn::default()).unwrap();
    assert_eq!(read(&fx, "old.txt").as_deref(), Some("same\n"));
    assert_eq!(read(&fx, "new.txt"), None);
}

#[test]
fn undo_turn_hunk_reverts_one_part_and_refuses_after_later_edits() {
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    let fx = FixtureProject::new().file("f.txt", &body).build();
    let env = Env::new(fx.root());
    begin(&env, "s", "two edits");
    let edited = body
        .replace("line 2\n", "line 2 agent\n")
        .replace("line 28\n", "line 28 agent\n");
    rewrite(&fx.path("f.txt"), edited.as_bytes());
    let t = end(&env, "s").unwrap();

    let opts = DiffOptions::default();
    let diff = env
        .dsnap
        .file_diff(
            env.project,
            t.before,
            VersionRef::Version(t.after.unwrap()),
            &rp("f.txt"),
            &opts,
        )
        .unwrap();
    let DiffBody::Text { hunks } = diff.body else {
        panic!("text diff expected")
    };
    assert_eq!(hunks.len(), 2);

    env.dsnap
        .undo_turn_hunk(t.id, &rp("f.txt"), 1, &opts)
        .unwrap();
    let now = read(&fx, "f.txt").unwrap();
    assert!(now.contains("line 2 agent\n") && now.contains("line 28\n"));
    // The file no longer matches the turn's result, so hunk undo is refused.
    let err = env
        .dsnap
        .undo_turn_hunk(t.id, &rp("f.txt"), 0, &opts)
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
}

#[test]
fn running_turns_list_changes_so_far_and_cannot_be_undone() {
    let fx = fixture();
    let env = Env::new(fx.root());
    let t = begin(&env, "s", "working");
    rewrite(&fx.path("a.txt"), b"wip\n");
    let files = env.dsnap.turn_files(t.id).unwrap();
    assert_eq!(files.len(), 1);
    let err = env.dsnap.undo_turn(t.id, &UndoTurn::default()).unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
}

#[test]
fn retention_keeps_versions_of_pending_turns_and_deleting_one_prunes_the_turn() {
    let fx = fixture();
    let env = Env::new(fx.root());
    env.db
        .set_global_settings(&GlobalSettings {
            retention_keep: 1,
            ..GlobalSettings::default()
        })
        .unwrap();
    let t = agent_turn(&fx, &env);
    for i in 0..3 {
        rewrite(&fx.path("keep.txt"), format!("{i}\n").as_bytes());
        snap(&env, None);
    }
    let t = env.dsnap.turn(t.id).unwrap();
    assert_eq!(t.state, TurnState::Pending, "retention kept both sides");
    env.dsnap.keep_turn(t.id).unwrap();
    rewrite(&fx.path("keep.txt"), b"last\n");
    snap(&env, None);
    let t = env.dsnap.turn(t.id).unwrap();
    assert_eq!(t.state, TurnState::Pruned);
    assert!(env.dsnap.turn_files(t.id).is_err());
}

#[test]
fn removing_the_project_removes_its_turns() {
    let fx = fixture();
    let env = Env::new(fx.root());
    let t = agent_turn(&fx, &env);
    env.dsnap.remove_project(env.project, true).unwrap();
    assert!(matches!(env.dsnap.turn(t.id), Err(Error::NotFound(_))));
}

#[test]
fn path_history_lists_the_versions_where_a_file_changed() {
    let fx = fixture();
    let env = Env::new(fx.root());
    snap(&env, Some("v1"));
    rewrite(&fx.path("keep.txt"), b"other\n");
    snap(&env, Some("v2 other file"));
    rewrite(&fx.path("a.txt"), b"a2\n");
    snap(&env, Some("v3"));
    fs::remove_file(fx.path("a.txt")).unwrap();
    snap(&env, Some("v4 deleted"));

    let h = env.dsnap.path_history(env.project, &rp("a.txt")).unwrap();
    let labels: Vec<_> = h.iter().map(|p| p.version.label.as_str()).collect();
    assert_eq!(labels, ["v4 deleted", "v3", "v1"]);
    assert!(h[0].entry.is_none() && h[1].entry.is_some());
}

#[test]
fn known_paths_include_the_latest_version_and_new_files() {
    let fx = fixture();
    let env = Env::new(fx.root());
    snap(&env, None);
    fs::write(fx.path("fresh.txt"), "f").unwrap();
    let paths: Vec<_> = env
        .dsnap
        .known_paths(env.project)
        .unwrap()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(paths, ["a.txt", "fresh.txt", "gone.txt", "keep.txt"]);
}
