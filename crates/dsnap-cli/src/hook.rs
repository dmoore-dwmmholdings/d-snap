//! Claude Code hook mode (DSNA-64): `dsnap snap --hook` and `dsnap hooks print`.
//!
//! A hook runs on every prompt and every Stop, so it must never block or fail an agent turn:
//! it never prompts and always exits 0. Problems go to stderr and to
//! `<home>/logs/hook.log` (rotated at 1 MB). Hooks serialize with every other D-Snap
//! operation on the project through [`ProjectLock`] (a file in the home directory, never in
//! the project folder).
//!
//! Turns (DSNA-119): the hook reads Claude Code's JSON payload from stdin. A
//! `UserPromptSubmit` begins a turn titled with the prompt, a `Stop` ends it, anything else
//! is a plain snapshot. Without a payload, the `-m` labels the hooks have always used
//! ("before agent turn" / "after agent turn") decide.

use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dsnap_core::lock::ProjectLock;
use dsnap_core::{Dsnap, Home, SnapshotOptions, SnapshotReport, TurnStart, VersionKind};
use serde::Deserialize;
use time::OffsetDateTime;
use time::macros::format_description;

/// The hook log is rotated (to `hook.log.1`) once it is this large.
const LOG_MAX_BYTES: u64 = 1024 * 1024;
/// A snapshot slower than this is logged: it may hit the hook timeout.
const SLOW: Duration = Duration::from_secs(10);
/// How long a hook waits for another hook on the same project.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// Most stdin bytes kept; the rest is read and dropped (a prompt is cut by core anyway).
const MAX_INPUT: usize = 1024 * 1024;

/// What a hook invocation does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A prompt was submitted: snapshot and begin a turn.
    Begin {
        /// Claude Code session id.
        session: Option<String>,
        /// The prompt.
        prompt: String,
    },
    /// The agent stopped: snapshot and end the running turn.
    End {
        /// Claude Code session id.
        session: Option<String>,
    },
    /// Any other hook: a plain snapshot.
    Plain,
}

/// The fields of Claude Code's hook payload that D-Snap reads.
#[derive(Debug, Deserialize)]
struct Payload {
    hook_event_name: Option<String>,
    session_id: Option<String>,
    prompt: Option<String>,
}

/// Decide what to do from the hook's stdin and its `-m` label.
pub fn classify(input: &[u8], label: Option<&str>) -> Action {
    if let Ok(p) = serde_json::from_slice::<Payload>(input) {
        let session = p.session_id.filter(|s| !s.is_empty());
        match p.hook_event_name.as_deref() {
            Some("UserPromptSubmit") => {
                return Action::Begin {
                    session,
                    prompt: p.prompt.unwrap_or_default(),
                };
            }
            Some("Stop") => return Action::End { session },
            Some(_) => return Action::Plain,
            None => {}
        }
    }
    match label.map(str::trim) {
        Some(l) if l.eq_ignore_ascii_case("before agent turn") => Action::Begin {
            session: None,
            prompt: String::new(),
        },
        Some(l) if l.eq_ignore_ascii_case("after agent turn") => Action::End { session: None },
        _ => Action::Plain,
    }
}

/// `dsnap snap --hook`: snapshot `path`, logging instead of failing.
pub fn snap(home: Option<PathBuf>, path: &Path, label: Option<String>) {
    let input = read_stdin();
    let action = classify(&input, label.as_deref());
    let log = Log::new(home.clone());
    if let Err(e) = snap_inner(home, path, label, &action, &log) {
        log.write(&format!("snapshot of {} failed: {e:#}", path.display()));
    }
}

fn snap_inner(
    home: Option<PathBuf>,
    path: &Path,
    label: Option<String>,
    action: &Action,
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
    let opts = SnapshotOptions {
        label,
        kind: VersionKind::Cli,
        ..SnapshotOptions::default()
    };
    let report: SnapshotReport = match action {
        Action::Begin { session, prompt } => {
            let start = TurnStart {
                session_id: session.clone(),
                prompt: prompt.clone(),
            };
            dsnap.begin_turn(project.id, start, opts)?.0
        }
        Action::End { session } => dsnap.end_turn(project.id, session.as_deref(), opts)?.0,
        Action::Plain => dsnap.snapshot(project.id, opts)?,
    };
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

/// Read the hook's JSON input (all of it, so the caller never blocks on a full pipe),
/// keeping at most [`MAX_INPUT`] bytes. Empty from a terminal.
fn read_stdin() -> Vec<u8> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Vec::new();
    }
    let mut lock = stdin.lock();
    let mut kept = Vec::new();
    let _ = (&mut lock).take(MAX_INPUT as u64).read_to_end(&mut kept);
    let _ = io::copy(&mut lock, &mut io::sink());
    kept
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_submit_begins_a_turn_with_the_prompt() {
        let input = br#"{"session_id":"abc","hook_event_name":"UserPromptSubmit","prompt":"Add tests","cwd":"/x"}"#;
        assert_eq!(
            classify(input, Some("before agent turn")),
            Action::Begin {
                session: Some("abc".into()),
                prompt: "Add tests".into()
            }
        );
    }

    #[test]
    fn stop_ends_the_turn_and_other_events_snapshot() {
        let stop = br#"{"session_id":"abc","hook_event_name":"Stop","stop_hook_active":false}"#;
        assert_eq!(
            classify(stop, None),
            Action::End {
                session: Some("abc".into())
            }
        );
        let other = br#"{"hook_event_name":"SubagentStop"}"#;
        assert_eq!(classify(other, Some("after agent turn")), Action::Plain);
    }

    #[test]
    fn without_a_payload_the_label_decides() {
        assert_eq!(
            classify(b"", Some("Before agent turn ")),
            Action::Begin {
                session: None,
                prompt: String::new()
            }
        );
        assert_eq!(
            classify(b"not json", Some("after agent turn")),
            Action::End { session: None }
        );
        assert_eq!(classify(b"", Some("my label")), Action::Plain);
        assert_eq!(classify(b"{}", None), Action::Plain);
    }
}
