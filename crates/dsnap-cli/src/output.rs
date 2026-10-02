//! Text output of the `dsnap` commands. `--json` bypasses all of this.

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::Path;

use dsnap_core::{
    ChangeCounts, ChangeStatus, DiffBody, FileChange, FileDiff, LineTag, Project, RestorePlan,
    RestoreReport, SkipReason, SkippedFile, Version, VersionKind,
};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

/// Whether to color the output.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    color: bool,
}

impl Style {
    /// Color when stdout is a terminal and `NO_COLOR` is not set.
    pub fn detect() -> Self {
        Self {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }

    fn red(self, s: &str) -> String {
        self.paint("31", s)
    }

    fn green(self, s: &str) -> String {
        self.paint("32", s)
    }

    fn yellow(self, s: &str) -> String {
        self.paint("33", s)
    }

    fn cyan(self, s: &str) -> String {
        self.paint("36", s)
    }
}

/// `+added ~modified -deleted`.
pub fn counts(c: &ChangeCounts) -> String {
    format!("+{} ~{} -{}", c.added, c.modified, c.deleted)
}

/// Local time `YYYY-MM-DD HH:MM:SS` of a Unix time in milliseconds.
fn local_time(ms: i64) -> String {
    let t = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let offset = UtcOffset::local_offset_at(t).unwrap_or(UtcOffset::UTC);
    let fmt = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    t.to_offset(offset)
        .format(&fmt)
        .unwrap_or_else(|_| ms.to_string())
}

fn kind(k: VersionKind) -> &'static str {
    match k {
        VersionKind::Manual => "manual",
        VersionKind::Auto => "auto",
        VersionKind::Cli => "cli",
        VersionKind::Safety => "safety",
    }
}

/// One line per version: id, time, kind, pin, counts, label.
pub fn versions(list: &[Version]) -> String {
    if list.is_empty() {
        return "No versions yet.\n".to_owned();
    }
    let width = list
        .iter()
        .map(|v| v.id.to_string().len())
        .max()
        .unwrap_or(1);
    let mut s = String::new();
    for v in list {
        let _ = writeln!(
            s,
            "{:>width$}  {}  {:<6} {} {:<16} {}",
            v.id.to_string(),
            local_time(v.created_at_ms),
            kind(v.kind),
            if v.pinned { "*" } else { " " },
            counts(&v.counts),
            v.label,
        );
    }
    s
}

/// `A path`, `M path +3 -1`, `D path`, `R old -> new` per change.
pub fn changes(list: &[FileChange], style: Style) -> String {
    let mut s = String::new();
    for c in list {
        let (tag, name) = match &c.status {
            ChangeStatus::Added => (style.green("A"), c.path.to_string()),
            ChangeStatus::Modified => (style.yellow("M"), c.path.to_string()),
            ChangeStatus::Deleted => (style.red("D"), c.path.to_string()),
            ChangeStatus::Renamed { from } => (style.cyan("R"), format!("{from} -> {}", c.path)),
        };
        let lines = match (c.lines_added, c.lines_removed) {
            (Some(a), Some(r)) if a + r > 0 => format!("  +{a} -{r}"),
            _ => String::new(),
        };
        let _ = writeln!(s, "{tag} {name}{lines}");
    }
    s
}

/// `warning: skipped …` lines for files a capture left out.
pub fn skipped(list: &[SkippedFile]) -> String {
    let mut s = String::new();
    for f in list {
        let why = match &f.reason {
            SkipReason::TooLarge { size } => format!("larger than the size cap ({size} bytes)"),
            SkipReason::Locked => "locked by another program".to_owned(),
            SkipReason::NonUtf8Name => "name is not valid UTF-8".to_owned(),
            SkipReason::Unreadable { msg } => format!("unreadable: {msg}"),
        };
        let _ = writeln!(s, "warning: skipped {}: {why}", f.path);
    }
    s
}

/// Unified diff of one file.
pub fn file_diff(d: &FileDiff, style: Style) -> String {
    let mut s = String::new();
    match &d.body {
        DiffBody::Text { hunks } => {
            if hunks.is_empty() {
                return format!("{}: no line changes\n", d.path);
            }
            let _ = writeln!(s, "--- a/{}\n+++ b/{}", d.path, d.path);
            for h in hunks {
                let head = format!(
                    "@@ -{},{} +{},{} @@",
                    h.old_start, h.old_len, h.new_start, h.new_len
                );
                let _ = writeln!(s, "{}", style.cyan(&head));
                for l in &h.lines {
                    let line = match l.tag {
                        LineTag::Equal => format!(" {}", l.text),
                        LineTag::Insert => style.green(&format!("+{}", l.text)),
                        LineTag::Delete => style.red(&format!("-{}", l.text)),
                    };
                    let _ = writeln!(s, "{line}");
                }
            }
        }
        DiffBody::Binary {
            old_size, new_size, ..
        } => {
            let size = |n: &Option<u64>| n.map_or("absent".to_owned(), |n| format!("{n} bytes"));
            let _ = writeln!(
                s,
                "Binary file {} differs ({} -> {})",
                d.path,
                size(old_size),
                size(new_size)
            );
        }
        DiffBody::Image { mime, .. } => {
            let _ = writeln!(s, "Image {} ({mime}) differs", d.path);
        }
        DiffBody::TooLarge { size } => {
            let _ = writeln!(s, "{} is too large to diff ({size} bytes)", d.path);
        }
    }
    s
}

/// Whether a plan changes nothing.
pub fn plan_is_empty(p: &RestorePlan) -> bool {
    p.write.is_empty() && p.delete.is_empty() && p.create_dirs.is_empty()
}

/// What a whole-project restore will do.
pub fn plan(p: &RestorePlan, v: &Version, style: Style) -> String {
    let mut s = format!("Restore to version {} \"{}\":\n", v.id, v.label);
    for w in &p.write {
        let _ = writeln!(s, "  {} {w}", style.yellow("write "));
    }
    for d in &p.create_dirs {
        let _ = writeln!(s, "  {} {d}/", style.green("mkdir "));
    }
    for d in &p.delete {
        let _ = writeln!(s, "  {} {d}", style.red("delete"));
    }
    for u in &p.uncaptured {
        let _ = writeln!(
            s,
            "  {} {u} (its current content is in no version)",
            style.cyan("keep  ")
        );
    }
    if plan_is_empty(p) {
        s.push_str("Nothing to change.\n");
    }
    s
}

/// Outcome of a restore, with the undo command.
pub fn restore_report(r: &RestoreReport, root: &Path) -> String {
    let mut s = format!(
        "Restored: {} written, {} deleted.\n",
        r.written.len(),
        r.deleted.len()
    );
    for u in &r.uncaptured {
        let _ = writeln!(s, "Left alone: {u} (its current content is in no version)");
    }
    for (p, e) in &r.failed {
        let _ = writeln!(s, "Not restored: {p}: {e}");
    }
    let _ = writeln!(
        s,
        "Undo with: dsnap restore \"{}\" {}",
        root.display(),
        r.safety_version
    );
    s
}

/// One line per project.
pub fn projects(list: &[Project]) -> String {
    if list.is_empty() {
        return "No projects tracked.\n".to_owned();
    }
    let mut s = String::new();
    for p in list {
        let missing = if p.missing { "  (folder missing)" } else { "" };
        let _ = writeln!(
            s,
            "{:>4}  {}  {}{missing}",
            p.id.to_string(),
            p.name,
            p.root.display()
        );
    }
    s
}
