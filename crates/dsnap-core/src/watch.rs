//! File watching for the change badge (F3) and database change polling. Owner: Chain Q
//! (DSNA-20).
//!
//! [`Watcher`] watches project folders recursively with `notify` (ReadDirectoryChangesW on
//! Windows). Events under ignored paths are dropped, so `node_modules` churn wakes nothing.
//! The rest are debounced per project; then the project's status is computed once and
//! [`WatchEvent::Changed`] carries the count. A lost-events signal from the OS just marks
//! the project for a status recompute. A root that disappears gives
//! [`WatchEvent::Missing`]; it is checked on every report and every
//! [`ROOT_CHECK_INTERVAL`], since a watched folder that is moved away may send nothing.
//!
//! [`DbChangeWatcher`] polls [`Dsnap::db_change_token`] to see versions another process
//! (the CLI) added.
//!
//! No UI types: events go to a callback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::ignore_rules::IgnoreRules;
use crate::types::{ProjectId, RelPath, VersionId};

/// Quiet time after the last relevant event before the status is computed.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);
/// How often watched roots are checked for having been moved or deleted.
pub const ROOT_CHECK_INTERVAL: Duration = Duration::from_secs(2);
/// How often [`DbChangeWatcher`] polls the database.
pub const DB_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// What a watcher reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// Files changed in the project folder; `changed_count` paths differ from the latest
    /// version.
    Changed {
        /// The project.
        project: ProjectId,
        /// Number of changed paths ([`Dsnap::status`]).
        changed_count: u32,
    },
    /// The project folder no longer exists at its root.
    Missing {
        /// The project.
        project: ProjectId,
    },
    /// Versions of the project were added or removed (by any process).
    VersionsChanged {
        /// The project.
        project: ProjectId,
    },
    /// The database changed in a way not tied to one project's versions (a project was
    /// added, removed or edited).
    DbChanged,
}

/// Receives watcher events, on the watcher's own thread.
pub type Sink = Arc<dyn Fn(WatchEvent) + Send + Sync>;

/// Messages to the worker thread.
enum Msg {
    /// File system activity under a project (already filtered).
    Touched(ProjectId),
    /// Stop.
    Quit,
}

/// One watched project.
struct Watched {
    root: PathBuf,
    /// Kept alive to keep watching; dropped to stop.
    _notify: RecommendedWatcher,
}

type Projects = Arc<Mutex<HashMap<ProjectId, Watched>>>;

/// Watches project folders and reports debounced change counts. Stops on drop.
pub struct Watcher {
    dsnap: Arc<Dsnap>,
    projects: Projects,
    tx: Sender<Msg>,
    worker: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Start a watcher (no projects yet) with [`DEFAULT_DEBOUNCE`].
    pub fn new(dsnap: Arc<Dsnap>, sink: Sink) -> Self {
        Self::with_debounce(dsnap, sink, DEFAULT_DEBOUNCE)
    }

    /// Start a watcher with a custom debounce.
    pub fn with_debounce(dsnap: Arc<Dsnap>, sink: Sink, debounce: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let projects: Projects = Arc::default();
        let worker = {
            let dsnap = Arc::clone(&dsnap);
            let projects = Arc::clone(&projects);
            std::thread::Builder::new()
                .name("dsnap-watch".into())
                .spawn(move || worker(&dsnap, &projects, &rx, &sink, debounce))
                .ok()
        };
        Self {
            dsnap,
            projects,
            tx,
            worker,
        }
    }

    /// Start watching `project` (no-op if already watched). Its change count is reported
    /// once right away, so a badge starts out correct.
    ///
    /// Errors: [`Error::NotFound`], [`Error::ProjectMissing`], an unreadable `.gitignore`,
    /// or [`Error::Io`] if the OS watch cannot be set up.
    pub fn watch(&self, project: ProjectId) -> Result<()> {
        if lock(&self.projects).contains_key(&project) {
            return Ok(());
        }
        let p = self.dsnap.load_project(project)?;
        let rules = Mutex::new(IgnoreRules::new(&p.root, &p.settings)?);
        let root = p.root.clone();
        let settings = p.settings.clone();
        let tx = self.tx.clone();
        let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let relevant = match &res {
                // Lost events or an error: recompute to be safe.
                Err(_) => true,
                Ok(ev) if ev.need_rescan() || ev.paths.is_empty() => true,
                Ok(ev) => {
                    if ev
                        .paths
                        .iter()
                        .any(|p| p.file_name().is_some_and(|n| n == ".gitignore"))
                    {
                        if let Ok(r) = IgnoreRules::new(&root, &settings) {
                            *lock(&rules) = r;
                        }
                    }
                    let rules = lock(&rules);
                    ev.paths.iter().any(|p| !ignored(&rules, &root, p))
                }
            };
            if relevant {
                let _ = tx.send(Msg::Touched(project));
            }
        })
        .map_err(|e| notify_error(&p.root, &e))?;
        w.watch(&p.root, RecursiveMode::Recursive)
            .map_err(|e| notify_error(&p.root, &e))?;
        lock(&self.projects).insert(
            project,
            Watched {
                root: p.root,
                _notify: w,
            },
        );
        let _ = self.tx.send(Msg::Touched(project));
        Ok(())
    }

    /// Stop watching `project` (no-op if not watched).
    pub fn unwatch(&self, project: ProjectId) {
        lock(&self.projects).remove(&project);
    }

    /// Projects being watched.
    pub fn watched(&self) -> Vec<ProjectId> {
        lock(&self.projects).keys().copied().collect()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        lock(&self.projects).clear();
        let _ = self.tx.send(Msg::Quit);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn notify_error(root: &Path, e: &notify::Error) -> Error {
    Error::io(root, std::io::Error::other(e.to_string()))
}

/// Whether an event path is ignored. The root itself is not; paths outside it are. A path
/// that no longer exists is checked as a file and as a directory: either rule ignoring it
/// is enough.
fn ignored(rules: &IgnoreRules, root: &Path, abs: &Path) -> bool {
    let Ok(rel) = RelPath::from_abs(root, abs) else {
        return abs != root;
    };
    match std::fs::symlink_metadata(abs) {
        Ok(m) => rules.is_ignored(&rel, m.is_dir()),
        Err(_) => rules.is_ignored(&rel, false) || rules.is_ignored(&rel, true),
    }
}

/// The worker: debounce, compute status one project at a time, check roots.
fn worker(
    dsnap: &Dsnap,
    projects: &Mutex<HashMap<ProjectId, Watched>>,
    rx: &mpsc::Receiver<Msg>,
    sink: &Sink,
    debounce: Duration,
) {
    // Project → time of its last relevant event not yet reported.
    let mut pending: HashMap<ProjectId, Instant> = HashMap::new();
    let mut missing: HashMap<ProjectId, bool> = HashMap::new();
    let mut next_root_check = Instant::now() + ROOT_CHECK_INTERVAL;
    loop {
        let wake = pending
            .values()
            .map(|t| *t + debounce)
            .min()
            .map_or(next_root_check, |d| d.min(next_root_check));
        match rx.recv_timeout(wake.saturating_duration_since(Instant::now())) {
            Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Msg::Touched(p)) => {
                pending.insert(p, Instant::now());
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        let now = Instant::now();
        let due: Vec<ProjectId> = pending
            .iter()
            .filter(|(_, t)| now >= **t + debounce)
            .map(|(p, _)| *p)
            .collect();
        for p in due {
            pending.remove(&p);
            let root = lock(projects).get(&p).map(|w| w.root.clone());
            // Unwatched meanwhile: nothing to report.
            if let Some(root) = root {
                report(dsnap, sink, p, &root, &mut missing);
            }
        }

        if now >= next_root_check {
            next_root_check = now + ROOT_CHECK_INTERVAL;
            let roots: Vec<(ProjectId, PathBuf)> = lock(projects)
                .iter()
                .map(|(p, w)| (*p, w.root.clone()))
                .collect();
            for (p, root) in roots {
                let gone = !root.is_dir();
                let was = missing.get(&p).copied().unwrap_or(false);
                if gone != was {
                    report(dsnap, sink, p, &root, &mut missing);
                }
            }
        }
    }
}

/// Compute and report one project's change count, or that its folder is missing (once,
/// until it comes back).
fn report(
    dsnap: &Dsnap,
    sink: &Sink,
    project: ProjectId,
    root: &Path,
    missing: &mut HashMap<ProjectId, bool>,
) {
    let gone = |missing: &mut HashMap<ProjectId, bool>| {
        if !missing.insert(project, true).unwrap_or(false) {
            sink(WatchEvent::Missing { project });
        }
    };
    if !root.is_dir() {
        gone(missing);
        return;
    }
    match dsnap.status(project) {
        Ok(changes) => {
            missing.insert(project, false);
            sink(WatchEvent::Changed {
                project,
                changed_count: u32::try_from(changes.len()).unwrap_or(u32::MAX),
            });
        }
        Err(Error::ProjectMissing { .. }) => gone(missing),
        // A transient failure (a busy database, a locked .gitignore): the next event retries.
        Err(_) => {}
    }
}

/// Polls the database for changes by any process, e.g. versions the CLI added (DSNA-62).
/// Stops on drop.
pub struct DbChangeWatcher {
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl DbChangeWatcher {
    /// Poll every [`DB_POLL_INTERVAL`].
    pub fn new(dsnap: Arc<Dsnap>, sink: Sink) -> Self {
        Self::with_interval(dsnap, sink, DB_POLL_INTERVAL)
    }

    /// Poll every `interval`.
    pub fn with_interval(dsnap: Arc<Dsnap>, sink: Sink, interval: Duration) -> Self {
        let (stop, rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("dsnap-db-watch".into())
            .spawn(move || {
                let mut token = dsnap.db_change_token().ok();
                let mut seen = version_heads(&dsnap).unwrap_or_default();
                while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(interval) {
                    let now = dsnap.db_change_token().ok();
                    if now.is_none() || now == token {
                        continue;
                    }
                    token = now;
                    let Ok(heads) = version_heads(&dsnap) else {
                        sink(WatchEvent::DbChanged);
                        continue;
                    };
                    let mut changed: Vec<ProjectId> = heads
                        .iter()
                        .filter(|(p, h)| seen.get(*p) != Some(*h))
                        .map(|(p, _)| *p)
                        .collect();
                    changed.sort();
                    if changed.is_empty() {
                        sink(WatchEvent::DbChanged);
                    }
                    for project in changed {
                        sink(WatchEvent::VersionsChanged { project });
                    }
                    seen = heads;
                }
            })
            .ok();
        Self { stop, worker }
    }
}

impl Drop for DbChangeWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Newest version id and version count per project, to tell whose versions changed.
fn version_heads(dsnap: &Dsnap) -> Result<HashMap<ProjectId, (Option<VersionId>, usize)>> {
    let mut out = HashMap::new();
    for p in dsnap.db.list_projects()? {
        let versions = dsnap.db.list_versions(p.id)?;
        out.insert(p.id, (versions.first().map(|v| v.id), versions.len()));
    }
    Ok(out)
}
