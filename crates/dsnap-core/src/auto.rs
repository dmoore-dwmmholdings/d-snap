//! Auto-snapshot scheduling (F8). Owner: Chain Q (DSNA-20).
//!
//! [`AutoScheduler`] follows each project's [`AutoSnapshot`] setting:
//!
//! - `Every { secs }`: a snapshot every `secs` seconds, counted from when the scheduler
//!   first sees the setting.
//! - `AfterIdle { secs }`: a snapshot `secs` seconds after the last change the file watcher
//!   reported ([`AutoScheduler::on_change`]); each new change restarts the wait.
//!
//! Snapshots have [`VersionKind::Auto`]; one that finds nothing changed writes no version
//! (F7), and retention runs after each one that does ([`Dsnap::after_snapshot`]).
//! Settings and projects are read again on every [`AutoScheduler::tick`], so a settings
//! change or a removed project takes effect without a restart. The clock is injectable for
//! tests ([`ManualClock`]).

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{AutoSnapshot, ProjectId, SnapshotOptions, SnapshotReport, VersionKind};

/// How often [`AutoRunner`] calls [`AutoScheduler::tick`].
pub const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Source of the current time.
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> Instant;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A clock that moves only when told to (tests).
#[derive(Debug)]
pub struct ManualClock(Mutex<Instant>);

impl ManualClock {
    /// Start at the real current time.
    pub fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }

    /// Move forward by `d`.
    pub fn advance(&self, d: Duration) {
        *lock(&self.0) += d;
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *lock(&self.0)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Schedule state of one project.
#[derive(Debug, Clone, Copy)]
struct State {
    /// The setting this state belongs to; a different one starts over.
    mode: AutoSnapshot,
    /// `Every`: when the next snapshot is due.
    next: Option<Instant>,
    /// `AfterIdle`: last reported change not yet snapshotted.
    last_change: Option<Instant>,
}

/// One auto snapshot that ran.
#[derive(Debug)]
pub struct AutoRun {
    /// The project.
    pub project: ProjectId,
    /// What the snapshot did (`version: None` when nothing changed), or why it failed.
    pub result: Result<SnapshotReport>,
}

/// Decides when to take auto snapshots and takes them. Drive it with [`tick`] (or an
/// [`AutoRunner`]) and feed it watcher changes with [`on_change`].
///
/// [`tick`]: AutoScheduler::tick
/// [`on_change`]: AutoScheduler::on_change
pub struct AutoScheduler {
    dsnap: Arc<Dsnap>,
    clock: Arc<dyn Clock>,
    states: Mutex<HashMap<ProjectId, State>>,
}

impl AutoScheduler {
    /// A scheduler using `clock`.
    pub fn new(dsnap: Arc<Dsnap>, clock: Arc<dyn Clock>) -> Self {
        Self {
            dsnap,
            clock,
            states: Mutex::default(),
        }
    }

    /// Files changed in `project` (from the watcher): restarts its idle wait.
    pub fn on_change(&self, project: ProjectId) {
        let now = self.clock.now();
        lock(&self.states)
            .entry(project)
            .or_insert(State {
                mode: AutoSnapshot::Off,
                next: None,
                last_change: None,
            })
            .last_change = Some(now);
    }

    /// Take every snapshot that is due now. Reads projects and settings fresh, so changes
    /// apply on the next tick. Returns the snapshots taken (none when nothing was due).
    ///
    /// Errors only if the project list cannot be read; a failing snapshot is reported in
    /// its [`AutoRun`] and retried at the next due time.
    pub fn tick(&self) -> Result<Vec<AutoRun>> {
        let now = self.clock.now();
        let projects = self.dsnap.list_projects()?;
        let mut due = Vec::new();
        {
            let mut states = lock(&self.states);
            states.retain(|id, _| projects.iter().any(|p| p.id == *id));
            for p in &projects {
                let mode = p.settings.auto_snapshot;
                let st = states.entry(p.id).or_insert(State {
                    mode,
                    next: None,
                    last_change: None,
                });
                if st.mode != mode {
                    // New setting: start over, but keep a pending change.
                    *st = State {
                        mode,
                        next: None,
                        last_change: st.last_change,
                    };
                }
                if p.missing {
                    continue;
                }
                match mode {
                    AutoSnapshot::Off => {}
                    AutoSnapshot::Every { secs } => {
                        let period = Duration::from_secs(secs.max(1));
                        let next = *st.next.get_or_insert(now + period);
                        if now >= next {
                            st.next = Some(now + period);
                            due.push(p.id);
                        }
                    }
                    AutoSnapshot::AfterIdle { secs } => {
                        let idle = Duration::from_secs(secs.max(1));
                        if st.last_change.is_some_and(|t| now >= t + idle) {
                            st.last_change = None;
                            due.push(p.id);
                        }
                    }
                }
            }
        }
        Ok(due
            .into_iter()
            .map(|project| AutoRun {
                project,
                result: self.dsnap.snapshot(
                    project,
                    SnapshotOptions {
                        kind: VersionKind::Auto,
                        ..SnapshotOptions::default()
                    },
                ),
            })
            .collect())
    }
}

/// Calls [`AutoScheduler::tick`] every [`TICK_INTERVAL`] on its own thread and passes each
/// run to a callback. Stops on drop.
pub struct AutoRunner {
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl AutoRunner {
    /// Start ticking `scheduler`.
    pub fn new(scheduler: Arc<AutoScheduler>, on_run: Arc<dyn Fn(AutoRun) + Send + Sync>) -> Self {
        let (stop, rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("dsnap-auto".into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(TICK_INTERVAL) {
                    if let Ok(runs) = scheduler.tick() {
                        runs.into_iter().for_each(|r| on_run(r));
                    }
                }
            })
            .ok();
        Self { stop, worker }
    }
}

impl Drop for AutoRunner {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
