//! `dsnap` command-line entry point (DSNA-63, F9).
//!
//! Exit codes: 0 ok (including "no changes"), 1 error, 2 usage, 3 partial restore.

mod hook;
mod output;

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use dsnap_core::lock::ProjectLock;
use dsnap_core::projects::normalize_root;
use dsnap_core::{
    DiffOptions, Dsnap, Project, ProjectId, RelPath, SnapshotOptions, VersionId, VersionKind,
    VersionRef,
};
use serde::Serialize;

use output::Style;

/// Local snapshots of project folders.
#[derive(Debug, Parser)]
#[command(name = "dsnap", version, about)]
struct Cli {
    /// Data directory (overrides DSNAP_HOME).
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Print JSON instead of text (the core types, serialized).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Snapshot a folder, tracking it first if needed.
    Snap {
        /// Project folder.
        path: PathBuf,
        /// Version label (default: the current time).
        #[arg(short = 'm', long = "message", value_name = "LABEL")]
        label: Option<String>,
        /// Print nothing on success.
        #[arg(short, long)]
        quiet: bool,
        /// Hook mode for Claude Code: ignore stdin, never prompt, always exit 0 (problems
        /// go to stderr and the hook log).
        #[arg(long)]
        hook: bool,
    },
    /// List versions, newest first.
    List {
        /// Project folder.
        path: PathBuf,
        /// Show at most this many versions.
        #[arg(long, short = 'n')]
        limit: Option<usize>,
    },
    /// Show unsaved changes since the latest version.
    Status {
        /// Project folder.
        path: PathBuf,
    },
    /// Compare two versions, or a version and the folder.
    ///
    /// Without versions: the latest version against the folder. With FROM only: FROM against
    /// the folder.
    Diff {
        /// Project folder.
        path: PathBuf,
        /// Old side (version id; default: the version before TO, or the latest).
        from: Option<i64>,
        /// New side (version id; default: the folder).
        #[arg(conflicts_with = "working")]
        to: Option<i64>,
        /// Compare against the folder (the default when TO is not given).
        #[arg(long)]
        working: bool,
        /// Show the line diff of one file.
        #[arg(long, value_name = "REL_PATH")]
        file: Option<String>,
        /// Ignore whitespace and line-ending changes.
        #[arg(long, short = 'w')]
        ignore_whitespace: bool,
    },
    /// Restore the folder (or one file) to a version.
    ///
    /// Takes a safety snapshot first; restoring that version undoes the restore.
    Restore {
        /// Project folder.
        path: PathBuf,
        /// Version id to restore.
        version: i64,
        /// Restore only this file.
        #[arg(long, value_name = "REL_PATH")]
        file: Option<String>,
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Only print what would change.
        #[arg(long)]
        dry_run: bool,
    },
    /// Claude Code hooks.
    Hooks {
        #[command(subcommand)]
        cmd: HooksCmd,
    },
    /// Manage tracked projects (default: list).
    Projects {
        #[command(subcommand)]
        cmd: Option<ProjectsCmd>,
    },
}

#[derive(Debug, Subcommand)]
enum HooksCmd {
    /// Print the hooks JSON for Claude Code settings (snapshot before and after each turn).
    Print,
}

#[derive(Debug, Subcommand)]
enum ProjectsCmd {
    /// List tracked projects.
    List,
    /// Track a folder.
    Add {
        /// Folder to track.
        path: PathBuf,
        /// Display name (default: the folder name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Stop tracking a project (its folder is not touched).
    Remove {
        /// Project folder or id.
        project: String,
        /// Also delete stored content no other project uses.
        #[arg(long)]
        delete_snapshots: bool,
    },
    /// Rename a project.
    Rename {
        /// Project folder or id.
        project: String,
        /// New name.
        name: String,
    },
}

/// Ends the run with this exit code; any message was already printed.
#[derive(Debug)]
struct Exit(u8);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

const EXIT_USAGE: u8 = 2;
const EXIT_PARTIAL: u8 = 3;

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Cmd::Snap {
        path,
        label,
        hook: true,
        ..
    } = &cli.cmd
    {
        hook::snap(cli.home.clone(), path, label.clone());
        return ExitCode::SUCCESS;
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(code)) => ExitCode::from(*code),
            None => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

fn run(cli: Cli) -> Result<()> {
    let dsnap = Dsnap::open(cli.home.clone()).context("cannot open the D-Snap data directory")?;
    let out = Out {
        json: cli.json,
        style: Style::detect(),
    };
    match cli.cmd {
        Cmd::Snap {
            path, label, quiet, ..
        } => snap(&dsnap, &out, &path, label, quiet),
        Cmd::Hooks {
            cmd: HooksCmd::Print,
        } => {
            print!("{}", hook::snippet());
            Ok(())
        }
        Cmd::List { path, limit } => {
            let p = find_project(&dsnap, &path)?;
            let mut versions = dsnap.list_versions(p.id)?;
            if let Some(n) = limit {
                versions.truncate(n);
            }
            out.print(&versions, || output::versions(&versions))
        }
        Cmd::Status { path } => {
            let p = find_project(&dsnap, &path)?;
            let report = dsnap.status_report(p.id)?;
            out.print(&report, || {
                let mut s = output::changes(&report.changes, out.style);
                if report.changes.is_empty() {
                    s.push_str("No unsaved changes.\n");
                }
                s.push_str(&output::skipped(&report.skipped));
                s
            })
        }
        Cmd::Diff {
            path,
            from,
            to,
            working: _,
            file,
            ignore_whitespace,
        } => {
            let p = find_project(&dsnap, &path)?;
            let from = from.map(VersionId);
            let to = to.map_or(VersionRef::WorkingTree, |id| {
                VersionRef::Version(VersionId(id))
            });
            match file {
                None => {
                    let changes = dsnap.changes(p.id, from, to)?;
                    out.print(&changes, || output::changes(&changes, out.style))
                }
                Some(f) => {
                    let rel = rel_path(&f)?;
                    let opts = DiffOptions {
                        ignore_whitespace,
                        ..DiffOptions::default()
                    };
                    let diff = dsnap.file_diff(p.id, from, to, &rel, &opts)?;
                    out.print(&diff, || output::file_diff(&diff, out.style))
                }
            }
        }
        Cmd::Restore {
            path,
            version,
            file,
            yes,
            dry_run,
        } => {
            let req = RestoreArgs {
                version: VersionId(version),
                file: file.map(|f| rel_path(&f)).transpose()?,
                yes,
                dry_run,
            };
            restore(&dsnap, &out, &path, &req)
        }
        Cmd::Projects { cmd } => projects(&dsnap, &out, cmd.unwrap_or(ProjectsCmd::List)),
    }
}

/// Where and how to print.
struct Out {
    json: bool,
    style: Style,
}

impl Out {
    /// Print `value` as JSON, or the text `text` builds.
    fn print<T: Serialize>(&self, value: &T, text: impl FnOnce() -> String) -> Result<()> {
        let s = if self.json {
            let mut s = serde_json::to_string_pretty(value)?;
            s.push('\n');
            s
        } else {
            text()
        };
        let mut stdout = io::stdout().lock();
        stdout.write_all(s.as_bytes())?;
        stdout.flush()?;
        Ok(())
    }
}

fn snap(dsnap: &Dsnap, out: &Out, path: &Path, label: Option<String>, quiet: bool) -> Result<()> {
    let p = match try_find_project(dsnap, path)? {
        Some(p) => p,
        None => dsnap
            .add_project(path, None)
            .with_context(|| format!("cannot track {}", path.display()))?,
    };
    let _lock = lock_project(dsnap, p.id)?;
    let report = dsnap.snapshot(
        p.id,
        SnapshotOptions {
            label,
            kind: VersionKind::Cli,
            ..SnapshotOptions::default()
        },
    )?;
    eprint!("{}", output::skipped(&report.skipped));
    for u in &report.unstable_paths {
        eprintln!("warning: {u} kept changing while it was captured");
    }
    if quiet {
        return Ok(());
    }
    let latest = match &report.version {
        Some(_) => None,
        None => dsnap.list_versions(p.id)?.into_iter().next(),
    };
    out.print(&report, || match (&report.version, &latest) {
        (Some(v), _) => format!(
            "Saved version {} \"{}\" ({})\n",
            v.id,
            v.label,
            output::counts(&v.counts)
        ),
        (None, Some(l)) => format!("No changes since version {} \"{}\".\n", l.id, l.label),
        (None, None) => "No changes.\n".to_owned(),
    })
}

struct RestoreArgs {
    version: VersionId,
    file: Option<RelPath>,
    yes: bool,
    dry_run: bool,
}

fn restore(dsnap: &Dsnap, out: &Out, path: &Path, args: &RestoreArgs) -> Result<()> {
    let p = find_project(dsnap, path)?;
    let v = dsnap.version(args.version)?;
    if v.project_id != p.id {
        bail!(
            "version {} is not a version of {}",
            args.version,
            p.root.display()
        );
    }

    // Show what will happen.
    let plan = match &args.file {
        None => Some(dsnap.restore_plan(p.id, args.version)?),
        Some(_) => None,
    };
    let text = match (&plan, &args.file) {
        (Some(plan), _) => output::plan(plan, &v, out.style),
        (None, Some(f)) => format!("Restore {f} to version {} \"{}\".\n", v.id, v.label),
        (None, None) => String::new(),
    };
    if args.dry_run {
        return match &plan {
            Some(plan) => out.print(plan, || text),
            None => out.print(&args.file, || text),
        };
    }
    if !out.json {
        print!("{text}");
    }
    if plan.as_ref().is_some_and(output::plan_is_empty) {
        // Nothing to write, so no safety version either.
        return Ok(());
    }
    if !args.yes && !confirm()? {
        eprintln!("Not restored. Pass --yes to restore without asking.");
        return Err(anyhow!(Exit(EXIT_USAGE)));
    }

    let _lock = lock_project(dsnap, p.id)?;
    let report = match &args.file {
        Some(f) => dsnap.restore_file(p.id, args.version, f)?,
        None => dsnap.restore_project(p.id, args.version)?,
    };
    out.print(&report, || output::restore_report(&report, &p.root))?;
    if report.failed.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(Exit(EXIT_PARTIAL)))
    }
}

/// Wait for any other D-Snap operation on the project (the app, a hook), saying so.
fn lock_project(dsnap: &Dsnap, project: ProjectId) -> Result<ProjectLock> {
    ProjectLock::acquire(
        &dsnap.project_lock_path(project),
        dsnap_core::lock::STALE_AFTER,
        None,
        || eprintln!("Waiting for another D-Snap operation on this project..."),
    )?
    .ok_or_else(|| anyhow!("another D-Snap operation on this project is still running"))
}

/// Ask on a terminal; anything else (a pipe, a hook) counts as "no".
fn confirm() -> Result<bool> {
    if !io::stdin().is_terminal() {
        return Ok(false);
    }
    eprint!("Restore now? [y/N] ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES"))
}

fn projects(dsnap: &Dsnap, out: &Out, cmd: ProjectsCmd) -> Result<()> {
    match cmd {
        ProjectsCmd::List => {
            let list = dsnap.list_projects()?;
            out.print(&list, || output::projects(&list))
        }
        ProjectsCmd::Add { path, name } => {
            let p = dsnap.add_project(&path, name.as_deref())?;
            out.print(&p, || {
                format!(
                    "Tracking {} as \"{}\" (id {}).\n",
                    p.root.display(),
                    p.name,
                    p.id
                )
            })
        }
        ProjectsCmd::Remove {
            project,
            delete_snapshots,
        } => {
            let p = project_arg(dsnap, &project)?;
            dsnap.remove_project(p.id, delete_snapshots)?;
            out.print(&p, || {
                format!("Stopped tracking {} (id {}).\n", p.root.display(), p.id)
            })
        }
        ProjectsCmd::Rename { project, name } => {
            let p = project_arg(dsnap, &project)?;
            dsnap.rename_project(p.id, &name)?;
            let p = dsnap.project(p.id)?;
            out.print(&p, || {
                format!("Renamed project {} to \"{}\".\n", p.id, p.name)
            })
        }
    }
}

/// A project-relative path as typed (`\` and a leading `./` accepted).
fn rel_path(s: &str) -> Result<RelPath> {
    let s = s.replace('\\', "/");
    Ok(RelPath::new(s.trim_start_matches("./"))?)
}

/// A project given as an id or a folder.
fn project_arg(dsnap: &Dsnap, s: &str) -> Result<Project> {
    match s.parse::<i64>() {
        Ok(id) if !Path::new(s).exists() => Ok(dsnap.project(ProjectId(id))?),
        _ => find_project(dsnap, Path::new(s)),
    }
}

/// The tracked project at or around `path`.
fn find_project(dsnap: &Dsnap, path: &Path) -> Result<Project> {
    try_find_project(dsnap, path)?.ok_or_else(|| {
        anyhow!(
            "{} is not tracked; run `dsnap snap` or `dsnap projects add` on it first",
            path.display()
        )
    })
}

/// The tracked project whose folder is `path` or contains it (the innermost one).
pub(crate) fn try_find_project(dsnap: &Dsnap, path: &Path) -> Result<Option<Project>> {
    let target = key(&normalize_root(path)?);
    Ok(dsnap
        .list_projects()?
        .into_iter()
        .filter(|p| target.starts_with(key(&p.root)))
        .max_by_key(|p| p.root.as_os_str().len()))
}

/// Comparison key for a folder: case-folded on Windows.
fn key(p: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(p.to_string_lossy().to_lowercase())
    } else {
        p.to_path_buf()
    }
}
