//! Type parity with the frontend (DSNA-67): one serialized sample of every type and enum
//! variant that crosses the Tauri boundary, kept in `app/src/lib/api/generated/samples.json`.
//! `parity.test.ts` checks every sample against the TypeScript types in `types.ts`.
//!
//! This test fails when the serialized shapes change. Regenerate the file with
//! `DSNAP_UPDATE_SAMPLES=1 cargo test -p dsnap-bridge --test samples`, then run the
//! frontend tests.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use dsnap_bridge::backend::{Op, Phase, ProgressPayload, ProjectChanged, VersionsChanged};
use dsnap_bridge::error::ApiError;
use dsnap_core::*;
use serde_json::{Value, json};

fn rp(s: &str) -> RelPath {
    RelPath::new(s).unwrap()
}

fn hash() -> BlobHash {
    BlobHash::of(b"x")
}

fn entry(kind: EntryKind) -> Entry {
    Entry {
        path: rp("src/a.rs"),
        kind,
        blob: Some(hash()),
        size: 3,
        mtime_ns: 1_700_000_000_000_000_000,
        readonly: false,
    }
}

fn version() -> Version {
    Version {
        id: VersionId(2),
        project_id: ProjectId(1),
        label: "v".into(),
        created_at_ms: 1_700_000_000_000,
        kind: VersionKind::Manual,
        pinned: false,
        unstable: false,
        counts: ChangeCounts {
            added: 1,
            modified: 2,
            deleted: 3,
        },
    }
}

fn v<T: serde::Serialize>(x: &T) -> Value {
    serde_json::to_value(x).unwrap()
}

fn samples() -> Value {
    let settings = ProjectSettings::default();
    let hunk = Hunk {
        old_start: 1,
        old_len: 1,
        new_start: 1,
        new_len: 1,
        lines: vec![DiffLine {
            tag: LineTag::Delete,
            old_no: Some(1),
            new_no: None,
            text: "a".into(),
        }],
    };
    let change = |status| FileChange {
        path: rp("a"),
        status,
        old: Some(entry(EntryKind::File)),
        new: None,
        lines_added: Some(1),
        lines_removed: None,
    };
    json!({
        "Entry": [v(&entry(EntryKind::File))],
        "EntryKind": [
            v(&EntryKind::File),
            v(&EntryKind::Symlink { target: "b".into() }),
            v(&EntryKind::Dir),
        ],
        "VersionKind": [
            v(&VersionKind::Manual),
            v(&VersionKind::Auto),
            v(&VersionKind::Cli),
            v(&VersionKind::Safety),
        ],
        "Version": [v(&version())],
        "ChangeCounts": [v(&version().counts)],
        "AutoSnapshot": [
            v(&AutoSnapshot::Off),
            v(&AutoSnapshot::Every { secs: 60 }),
            v(&AutoSnapshot::AfterIdle { secs: 30 }),
        ],
        "ProjectSettings": [v(&settings)],
        "GlobalSettings": [v(&GlobalSettings::default())],
        "Project": [v(&Project {
            id: ProjectId(1),
            name: "p".into(),
            root: "C:/p".into(),
            settings: settings.clone(),
            missing: false,
        })],
        "ChangeStatus": [
            v(&ChangeStatus::Added),
            v(&ChangeStatus::Modified),
            v(&ChangeStatus::Deleted),
            v(&ChangeStatus::Renamed { from: rp("old") }),
        ],
        "FileChange": [v(&change(ChangeStatus::Modified))],
        "DiffOptions": [v(&DiffOptions::default())],
        "LineTag": [v(&LineTag::Equal), v(&LineTag::Insert), v(&LineTag::Delete)],
        "DiffLine": [v(&hunk.lines[0])],
        "Hunk": [v(&hunk)],
        "DiffBody": [
            v(&DiffBody::Text { hunks: vec![] }),
            v(&DiffBody::Binary {
                old_size: Some(1),
                new_size: None,
                old_hash: Some(hash()),
                new_hash: None,
            }),
            v(&DiffBody::Image {
                old: None,
                new: Some(hash()),
                mime: "image/png".into(),
            }),
            v(&DiffBody::TooLarge { size: 9 }),
        ],
        "FileDiff": [v(&FileDiff {
            path: rp("a"),
            body: DiffBody::TooLarge { size: 9 },
        })],
        "VersionRef": [
            v(&VersionRef::Version(VersionId(2))),
            v(&VersionRef::WorkingTree),
        ],
        "SkipReason": [
            v(&SkipReason::TooLarge { size: 1 }),
            v(&SkipReason::Locked),
            v(&SkipReason::NonUtf8Name),
            v(&SkipReason::Unreadable { msg: "m".into() }),
        ],
        "SkippedFile": [v(&SkippedFile {
            path: "big.bin".into(),
            reason: SkipReason::Locked,
        })],
        "SnapshotReport": [v(&SnapshotReport {
            version: Some(version()),
            skipped: vec![],
            unstable_paths: vec![rp("log")],
        })],
        "RestorePlan": [v(&RestorePlan {
            target: VersionId(2),
            write: vec![rp("a")],
            delete: vec![],
            create_dirs: vec![],
            uncaptured: vec![],
        })],
        "RestoreReport": [v(&RestoreReport {
            safety_version: VersionId(3),
            written: vec![],
            deleted: vec![],
            failed: vec![(rp("a"), "locked".into())],
            uncaptured: vec![],
        })],
        "ProgressOp": [v(&Op::Snapshot), v(&Op::Restore)],
        "ProgressPhase": [
            v(&Phase::Queued),
            v(&Phase::Walk),
            v(&Phase::Hash),
            v(&Phase::Restore),
            v(&Phase::Done),
        ],
        "Progress": [v(&ProgressPayload {
            op_id: "op-1".into(),
            project_id: ProjectId(1),
            op: Op::Snapshot,
            phase: Phase::Walk,
            done: 0,
            total: None,
            path: None,
        })],
        "ProjectChangedEvent": [v(&ProjectChanged {
            project_id: ProjectId(1),
            changed_count: 4,
        })],
        "VersionsChangedEvent": [v(&VersionsChanged {
            project_id: ProjectId(1),
        })],
        "ApiError": [v(&ApiError::new("not_found", "m"))],
    })
}

#[test]
fn samples_json_is_current() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../app/src/lib/api/generated/samples.json");
    let mut want = serde_json::to_string_pretty(&samples()).unwrap();
    want.push('\n');
    if std::env::var_os("DSNAP_UPDATE_SAMPLES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        have == want,
        "{} is out of date; run `DSNAP_UPDATE_SAMPLES=1 cargo test -p dsnap-bridge --test \
         samples` and check the frontend types",
        path.display()
    );
}
