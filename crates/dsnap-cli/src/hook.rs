//! Claude Code hook mode (DSNA-64): `dsnap snap --hook` and `dsnap hooks print`.
//!
//! A hook runs on every prompt and every Stop, so it must never block or fail an agent turn:
//! it ignores its stdin, never prompts, and always exits 0. Problems go to stderr and to
//! `<home>/logs/hook.log` (rotated at 1 MB). Hooks serialize with every other D-Snap
//! operation on the project through [`ProjectLock`] (a file in the home directory, never in
//! the project folder).

use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dsnap_core::lock::ProjectLock;
use dsnap_core::{Dsnap, Home, SnapshotOptions, VersionKind};
use time::OffsetDateTime;
use time::macros::format_description;

/// The hook log is rotated (to `hook.log.1`) once it is this large.
const LOG_MAX_BYTES: u64 = 1024 * 1024;
/// A snapshot slower than this is logged: it may hit the hook timeout.
const SLOW: Duration = Duration::from_secs(10);
/// How long a hook waits for another hook on the same project.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// `dsnap snap --hook`: snapshot `path`, logging instead of failing.
pub fn snap(home: Option<PathBuf>, path: &Path, label: Option<String>) {
    drain_stdin();
    let log = Log::new(home.clone());
    if let Err(e) = snap_inner(home, path, label, &log) {
        log.write(&format!("snapshot of {} failed: {e:#}", path.display()));
    }
}

fn snap_inner(
    home: Option<PathBuf>,
    path: &Path,
    label: Option<String>,
    log: &Log,
) -> anyhow::Result<()> {
    let dsnap = Dsnap::open(home)?;
    let project = match crate::try_find_project(&dsnap, path)? {
        Some(p) => p,
        // Another hook may have added it meanwhile.
        None => match dsnap.add_project(path, None) {
            Ok(p) => p,
            Err(e) => crate::try_find_project(&dsnap, path)?.ok_or(e)?,
        },
    };
    let lock_path = dsnap.project_lock_path(project.id);
    let Some(_lock) = ProjectLock::acquire(&lock_path, LOCK_WAIT, None, || {})? else {
        log.write(&format!(
            "skipped snapshot of {}: another snapshot of it is still running",
            project.root.display()
        ));
        return Ok(());
    };
    let start = Instant::now();
    let report = dsnap.snapshot(
        project.id,
        SnapshotOptions {
            label,
            kind: VersionKind::Cli,
            ..SnapshotOptions::default()
        },
    )?;
    let took = start.elapsed();
    if took > SLOW {
        log.write(&format!(
            "snapshot of {} took {took:?}; the hook may time out",
            project.root.display()
        ));
    }
    for s in &report.skipped {
        eprintln!("dsnap: skipped {}", s.path);
    }
    Ok(())
}

/// Read and drop the hook's JSON input so the caller never blocks on a full pipe.
fn drain_stdin() {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        let _ = stdin.lock().read_to_end(&mut Vec::new());
    }
}

/// Appends to `<home>/logs/hook.log`, and always to stderr.
struct Log {
    path: Option<PathBuf>,
}

impl Log {
    fn new(home: Option<PathBuf>) -> Self {
        let path = Home::resolve(home)
            .ok()
            .map(|h| h.root().join("logs").join("hook.log"));
        Self { path }
    }

    fn write(&self, msg: &str) {
        eprintln!("dsnap: {msg}");
        if let Some(path) = &self.path {
            let _ = append(path, &format!("{} {msg}\n", now()));
        }
    }
}

fn append(path: &Path, line: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(path).is_ok_and(|m| m.len() >= LOG_MAX_BYTES) {
        fs::rename(path, path.with_extension("log.1"))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

fn now() -> String {
    let t = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    t.format(format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second]"
    ))
    .unwrap_or_default()
}

/// The hooks JSON for Claude Code settings, calling this executable.
pub fn snippet() -> String {
    let exe = command_name();
    let cmd = |label: &str| format!("{exe} snap --hook \"$CLAUDE_PROJECT_DIR\" -m \"{label}\"");
    let hook = |label: &str| serde_json::json!([{ "hooks": [{ "type": "command", "command": cmd(label) }] }]);
    let v = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": hook("before agent turn"),
            "Stop": hook("after agent turn"),
        }
    });
    let mut s = serde_json::to_string_pretty(&v).unwrap_or_default();
    s.push('\n');
    s
}

/// `dsnap` when PATH finds this executable, else its absolute path (quoted, `/`-separated so
/// it works in both cmd and bash).
fn command_name() -> String {
    let Ok(me) = std::env::current_exe().and_then(fs::canonicalize) else {
        return "dsnap".to_owned();
    };
    let on_path = std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            ["dsnap", "dsnap.exe"]
                .iter()
                .any(|n| fs::canonicalize(dir.join(n)).is_ok_and(|p| p == me))
        })
    });
    if on_path {
        return "dsnap".to_owned();
    }
    let s = me.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/");
    format!("\"{s}\"")
}
