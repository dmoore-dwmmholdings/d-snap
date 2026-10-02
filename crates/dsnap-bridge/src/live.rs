//! Live updates for the app (DSNA-73, DSNA-74): file watchers for the change badge, the
//! database poll for versions the CLI adds, and auto-snapshots, all reported as events.
//!
//! - A [`Watcher`] watches every project whose folder exists; [`Live::sync`] follows adds,
//!   removals, relocations and settings changes. A change count goes out as
//!   `dsnap://project-changed` and restarts that project's idle timer.
//! - A folder that disappears, or a project edited by another process, goes out as
//!   `dsnap://projects-changed` (reload the list).
//! - [`DbChangeWatcher`] turns versions added elsewhere into `dsnap://versions-changed`.
//! - [`AutoScheduler`] snapshots through [`Backend`], so auto snapshots queue and lock like
//!   manual ones.

use std::sync::{Arc, Weak};

use dsnap_core::auto::{AutoRunner, AutoScheduler, SystemClock};
use dsnap_core::watch::{DbChangeWatcher, WatchEvent, Watcher};
use dsnap_core::{Error, ProjectId, VersionKind};

use crate::backend::{Backend, EVENT_PROJECTS_CHANGED, Op};

/// The running live-update services. Stops everything on drop.
pub struct Live {
    watcher: Watcher,
    _db: DbChangeWatcher,
    _auto: AutoRunner,
}

impl Live {
    /// Start watching every project of `backend` and running auto-snapshots. `None` when
    /// the data folder could not be opened.
    pub fn start(backend: &Arc<Backend>) -> Option<Self> {
        let dsnap = Arc::clone(backend.dsnap().ok()?);
        let scheduler = Arc::new(AutoScheduler::new(
            Arc::clone(&dsnap),
            Arc::new(SystemClock),
        ));

        let weak: Weak<Backend> = Arc::downgrade(backend);
        let watcher = {
            let weak = weak.clone();
            let scheduler = Arc::clone(&scheduler);
            Watcher::new(
                Arc::clone(&dsnap),
                Arc::new(move |e| {
                    let Some(b) = weak.upgrade() else { return };
                    match e {
                        WatchEvent::Changed {
                            project,
                            changed_count,
                        } => {
                            scheduler.on_change(project);
                            b.project_changed(project, changed_count);
                        }
                        _ => b.emit_unit(EVENT_PROJECTS_CHANGED),
                    }
                }),
            )
        };
        let db = {
            let weak = weak.clone();
            DbChangeWatcher::new(
                dsnap,
                Arc::new(move |e| {
                    let Some(b) = weak.upgrade() else { return };
                    match e {
                        WatchEvent::VersionsChanged { project } => b.versions_changed(project),
                        _ => b.emit_unit(EVENT_PROJECTS_CHANGED),
                    }
                }),
            )
        };
        let auto = {
            let weak = weak.clone();
            AutoRunner::with_tick(
                move || {
                    scheduler.tick_with(|project| {
                        let b = weak.upgrade().ok_or(Error::Cancelled)?;
                        auto_snapshot(&b, project)
                    })
                },
                Arc::new(|_| {}),
            )
        };
        let live = Self {
            watcher,
            _db: db,
            _auto: auto,
        };
        live.sync(backend);
        Some(live)
    }

    /// Watch exactly the projects whose folders exist, with their current settings.
    pub fn sync(&self, backend: &Backend) {
        let Ok(projects) = backend.list_projects() else {
            return;
        };
        for id in self.watcher.watched() {
            if !projects.iter().any(|p| p.id == id && !p.missing) {
                self.watcher.unwatch(id);
            }
        }
        for p in projects.iter().filter(|p| !p.missing) {
            // Errors (a folder that vanished just now) are reported by the next sync.
            let _ = self.watcher.watch(p.id);
        }
    }

    /// Watch `project` again, e.g. after its ignore settings changed.
    pub fn rewatch(&self, project: ProjectId) {
        self.watcher.unwatch(project);
        let _ = self.watcher.watch(project);
    }
}

/// An auto snapshot through the backend's queue and lock.
fn auto_snapshot(
    b: &Backend,
    project: ProjectId,
) -> dsnap_core::Result<dsnap_core::SnapshotReport> {
    let ctx = b.begin(project, Op::Snapshot);
    b.snapshot_as(&ctx, None, VersionKind::Auto)
        .map_err(|e| Error::InvalidInput(format!("{}: {}", e.code, e.message)))
}
