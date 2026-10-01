//! Ignore rules: built-in defaults, per-project patterns and (optionally) `.gitignore`.
//! Owner: Chain E (DSNA-8).
//!
//! Three sources are combined. For one path, the first source (in this order) with a matching
//! pattern decides, and inside a source the last matching pattern wins (git semantics):
//!
//! 1. `extra_ignore` patterns from [`ProjectSettings`], rooted at the project root.
//! 2. `.gitignore` files (only when [`ProjectSettings::respect_gitignore`]); a deeper file
//!    overrides a shallower one, like git.
//! 3. The built-in defaults in [`BUILTIN_IGNORES`].
//!
//! So `!node_modules/` in `extra_ignore` or a `.gitignore` re-includes a built-in default. The
//! exception is `.git`: any path with a `.git` component is always ignored.
//!
//! As in git, a path is ignored when any of its parent directories is ignored; a pattern cannot
//! re-include a path below an ignored directory.
//!
//! Global git excludes (`core.excludesFile`) and `.git/info/exclude` are never read, so the
//! result does not depend on the user's git configuration. On Windows matching is
//! case-insensitive (like `core.ignoreCase`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder, Glob};

use crate::error::{Error, Result};
use crate::types::{ProjectSettings, RelPath};

/// Paths always ignored, matched as directory names at any depth.
///
/// `.git` also matches a file (a worktree or submodule `.git` file) and cannot be re-included.
pub const BUILTIN_IGNORES: &[&str] = &[".git", "node_modules", "target", "dist", ".venv"];

/// Name of the per-directory ignore file.
const GITIGNORE: &str = ".gitignore";

const CASE_INSENSITIVE: bool = cfg!(windows);

/// Compiled ignore rules for one project.
///
/// `.gitignore` files are read lazily, the first time a path below their directory is checked,
/// and cached for the life of this value. Build a new `IgnoreRules` per operation to pick up
/// edits. Safe to share between threads.
pub struct IgnoreRules {
    root: PathBuf,
    builtin: Gitignore,
    extra: Gitignore,
    respect_gitignore: bool,
    /// Parsed `.gitignore` per directory (key: project-relative dir, `""` for the root).
    /// `None` when the directory has no usable `.gitignore`.
    gitignores: Mutex<HashMap<String, Option<Arc<Gitignore>>>>,
}

impl std::fmt::Debug for IgnoreRules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IgnoreRules")
            .field("root", &self.root)
            .field("extra_patterns", &self.extra.len())
            .field("respect_gitignore", &self.respect_gitignore)
            .finish_non_exhaustive()
    }
}

/// Outcome of checking one path against one source.
enum Verdict {
    Ignore,
    Keep,
    Undecided,
}

impl From<Match<&Glob>> for Verdict {
    fn from(m: Match<&Glob>) -> Self {
        match m {
            Match::Ignore(_) => Verdict::Ignore,
            Match::Whitelist(_) => Verdict::Keep,
            Match::None => Verdict::Undecided,
        }
    }
}

fn builder() -> GitignoreBuilder {
    // Matchers are rooted at "." and always given paths relative to their own directory, so
    // the crate never strips (or fails to strip) a prefix.
    let mut b = GitignoreBuilder::new(".");
    // Returns Result only for historical reasons; it cannot fail.
    let _ = b.case_insensitive(CASE_INSENSITIVE);
    b
}

fn build(b: &GitignoreBuilder, what: &str) -> Result<Gitignore> {
    b.build()
        .map_err(|e| Error::InvalidInput(format!("{what}: {e}")))
}

/// Parse `.gitignore` text, skipping lines that are not valid patterns (git skips them too).
fn parse_gitignore(text: &str) -> Option<Gitignore> {
    let mut b = builder();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for line in text.lines() {
        let _ = b.add_line(None, line);
    }
    b.build().ok().filter(|g| !g.is_empty())
}

fn is_dot_git(component: &str) -> bool {
    if CASE_INSENSITIVE {
        component.eq_ignore_ascii_case(".git")
    } else {
        component == ".git"
    }
}

impl IgnoreRules {
    /// Build the rules for the project at `root`.
    ///
    /// Returns [`Error::InvalidInput`] if an `extra_ignore` pattern is not valid gitignore
    /// syntax. Nothing is read from disk here; `.gitignore` files are read on demand.
    pub fn new(root: &Path, settings: &ProjectSettings) -> Result<Self> {
        let mut b = builder();
        for name in BUILTIN_IGNORES {
            // `.git` is handled separately (any kind, never re-included); the rest are
            // directory-only patterns matching at any depth.
            if *name != ".git" {
                b.add_line(None, &format!("{name}/"))
                    .map_err(|e| Error::InvalidInput(format!("built-in ignore {name}: {e}")))?;
            }
        }
        let builtin = build(&b, "built-in ignores")?;

        let mut b = builder();
        for pat in &settings.extra_ignore {
            b.add_line(None, pat)
                .map_err(|e| Error::InvalidInput(format!("ignore pattern {pat:?}: {e}")))?;
        }
        let extra = build(&b, "ignore patterns")?;

        Ok(Self {
            root: root.to_path_buf(),
            builtin,
            extra,
            respect_gitignore: settings.respect_gitignore,
            gitignores: Mutex::new(HashMap::new()),
        })
    }

    /// The project root these rules were built for.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `path` (a directory if `is_dir`) is ignored, including via an ignored parent.
    ///
    /// Usable on its own (no walk needed): restore uses it to never touch ignored paths.
    /// A symlink or junction counts as a file (`is_dir = false`), as in git.
    pub fn is_ignored(&self, path: &RelPath, is_dir: bool) -> bool {
        let s = path.as_str();
        // Every proper prefix is a directory; check from the top down.
        for (i, _) in s.match_indices('/') {
            if self.is_ignored_here(&s[..i], true) {
                return true;
            }
        }
        self.is_ignored_here(s, is_dir)
    }

    /// Whether `path` itself is ignored, assuming no parent directory is ignored.
    ///
    /// For a top-down walk that never descends into an ignored directory.
    #[allow(dead_code)] // used by walk (DSNA-34)
    pub(crate) fn is_ignored_entry(&self, path: &RelPath, is_dir: bool) -> bool {
        self.is_ignored_here(path.as_str(), is_dir)
    }

    fn is_ignored_here(&self, path: &str, is_dir: bool) -> bool {
        // Checking every component keeps `is_ignored_entry` safe on its own.
        if path.split('/').any(is_dot_git) {
            return true;
        }
        match Verdict::from(self.extra.matched(path, is_dir)) {
            Verdict::Ignore => return true,
            Verdict::Keep => return false,
            Verdict::Undecided => {}
        }
        if self.respect_gitignore {
            // `.gitignore` files in the path's ancestor directories, deepest first.
            let mut dir = path;
            loop {
                let parent = dir.rsplit_once('/').map_or("", |(p, _)| p);
                match self.gitignore_verdict(parent, path, is_dir) {
                    Verdict::Ignore => return true,
                    Verdict::Keep => return false,
                    Verdict::Undecided => {}
                }
                if parent.is_empty() {
                    break;
                }
                dir = parent;
            }
        }
        matches!(
            Verdict::from(self.builtin.matched(path, is_dir)),
            Verdict::Ignore
        )
    }

    /// Check `path` against the `.gitignore` in `dir` (project-relative, `""` = root).
    fn gitignore_verdict(&self, dir: &str, path: &str, is_dir: bool) -> Verdict {
        let Some(gi) = self.gitignore(dir) else {
            return Verdict::Undecided;
        };
        let rel = if dir.is_empty() {
            path
        } else {
            path.get(dir.len() + 1..).unwrap_or(path)
        };
        Verdict::from(gi.matched(rel, is_dir))
    }

    /// The parsed `.gitignore` of `dir`, read on first use.
    fn gitignore(&self, dir: &str) -> Option<Arc<Gitignore>> {
        {
            let cache = self.gitignores.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = cache.get(dir) {
                return hit.clone();
            }
        }
        // Read outside the lock so a parallel walk does not serialize on file reads. Two
        // threads may parse the same file; the first insert wins.
        let loaded = self.load_gitignore(dir).map(Arc::new);
        let mut cache = self.gitignores.lock().unwrap_or_else(|e| e.into_inner());
        cache.entry(dir.to_owned()).or_insert(loaded).clone()
    }

    fn load_gitignore(&self, dir: &str) -> Option<Gitignore> {
        let mut path = self.root.clone();
        if !dir.is_empty() {
            path.extend(dir.split('/'));
        }
        path.push(GITIGNORE);
        // Only a regular file counts; a symlinked `.gitignore` is not followed (git >= 2.32
        // refuses them too). An unreadable file is treated as absent.
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let bytes = fs::read(&path).ok()?;
        parse_gitignore(&String::from_utf8_lossy(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn rp(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    fn settings(extra: &[&str], respect_gitignore: bool) -> ProjectSettings {
        ProjectSettings {
            extra_ignore: extra.iter().map(|s| s.to_string()).collect(),
            respect_gitignore,
            ..ProjectSettings::default()
        }
    }

    /// Temp project with the given `(path, contents)` files.
    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let path = rp(p).to_path(dir.path());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, c).unwrap();
        }
        dir
    }

    fn rules(root: &Path, extra: &[&str], respect_gitignore: bool) -> IgnoreRules {
        IgnoreRules::new(root, &settings(extra, respect_gitignore)).unwrap()
    }

    #[test]
    fn builtins_alone() {
        let r = rules(Path::new("/nonexistent"), &[], true);
        for d in [
            "node_modules",
            "target",
            "dist",
            ".venv",
            "a/b/node_modules",
        ] {
            assert!(r.is_ignored(&rp(d), true), "{d}");
            assert!(
                r.is_ignored(&rp(&format!("{d}/x/y.js")), false),
                "{d}/x/y.js"
            );
        }
        // Directory-only: a file named like a built-in is kept.
        assert!(!r.is_ignored(&rp("dist"), false));
        assert!(!r.is_ignored(&rp("src/target"), false));
        assert!(!r.is_ignored(&rp("src/main.rs"), false));
        assert!(!r.is_ignored(&rp("targets/x"), false));
    }

    #[test]
    fn dot_git_is_always_ignored_as_file_or_dir() {
        let root = project(&[(".gitignore", "!.git\n!.git/\n!**/.git/**\n")]);
        let r = rules(root.path(), &["!.git", "!.git/", "!.git/**"], true);
        assert!(r.is_ignored(&rp(".git"), true));
        assert!(r.is_ignored(&rp(".git"), false));
        assert!(r.is_ignored(&rp(".git/config"), false));
        assert!(r.is_ignored(&rp("sub/.git"), false));
        assert!(r.is_ignored(&rp("sub/.git/HEAD"), false));
        assert!(r.is_ignored_entry(&rp("sub/.git/HEAD"), false));
        assert!(!r.is_ignored(&rp(".github/ci.yml"), false));
        assert!(!r.is_ignored(&rp(".gitignore"), false));
    }

    #[test]
    fn extra_patterns_alone() {
        let r = rules(
            Path::new("/nonexistent"),
            &["*.log", "/build/", "docs/*.tmp"],
            true,
        );
        assert!(r.is_ignored(&rp("a.log"), false));
        assert!(r.is_ignored(&rp("x/y/a.log"), false));
        assert!(r.is_ignored(&rp("build"), true));
        assert!(r.is_ignored(&rp("build/out.bin"), false));
        assert!(
            !r.is_ignored(&rp("src/build/out.bin"), false),
            "anchored to root"
        );
        assert!(!r.is_ignored(&rp("build"), false), "dir-only");
        assert!(r.is_ignored(&rp("docs/a.tmp"), false));
        assert!(!r.is_ignored(&rp("docs/sub/a.tmp"), false));
    }

    #[test]
    fn invalid_extra_pattern_is_rejected() {
        let err = IgnoreRules::new(Path::new("."), &settings(&["a{b"], true)).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    }

    #[test]
    fn gitignore_alone() {
        let root = project(&[(".gitignore", "# comment\n*.env\n/secret/\n\nlogs\n")]);
        let r = rules(root.path(), &[], true);
        assert!(r.is_ignored(&rp(".env"), false));
        assert!(r.is_ignored(&rp("prod.env"), false));
        assert!(r.is_ignored(&rp("deep/x.env"), false));
        assert!(r.is_ignored(&rp("secret/key"), false));
        assert!(!r.is_ignored(&rp("a/secret/key"), false));
        assert!(r.is_ignored(&rp("logs"), false));
        assert!(r.is_ignored(&rp("a/logs/x"), false));
        assert!(!r.is_ignored(&rp("src/main.rs"), false));
        assert!(!r.is_ignored(&rp(".gitignore"), false));
        assert!(!r.is_ignored(&rp("# comment"), false));
    }

    #[test]
    fn respect_gitignore_false_skips_gitignore_only() {
        let root = project(&[(".gitignore", "*.env\n")]);
        let r = rules(root.path(), &["*.log"], false);
        assert!(!r.is_ignored(&rp("prod.env"), false));
        assert!(r.is_ignored(&rp("a.log"), false));
        assert!(r.is_ignored(&rp("node_modules"), true));
        assert!(r.is_ignored(&rp(".git"), true));
    }

    #[test]
    fn negation_patterns() {
        let root = project(&[(".gitignore", "*.log\n!keep.log\nout/\n!out/\n")]);
        let r = rules(root.path(), &[], true);
        assert!(r.is_ignored(&rp("a.log"), false));
        assert!(!r.is_ignored(&rp("keep.log"), false));
        assert!(!r.is_ignored(&rp("x/keep.log"), false));
        assert!(!r.is_ignored(&rp("out/a"), false), "last match wins");
    }

    #[test]
    fn cannot_reinclude_below_an_ignored_dir() {
        let root = project(&[(".gitignore", "gen/\n!gen/keep.txt\n")]);
        let r = rules(root.path(), &[], true);
        assert!(r.is_ignored(&rp("gen/keep.txt"), false));
    }

    #[test]
    fn nested_gitignore_is_relative_and_overrides_parent() {
        let root = project(&[
            (".gitignore", "*.tmp\n/top.txt\n"),
            ("sub/.gitignore", "!keep.tmp\n/local.txt\n"),
            ("sub/deeper/.gitignore", "*.md\n"),
        ]);
        let r = rules(root.path(), &[], true);
        assert!(r.is_ignored(&rp("a.tmp"), false));
        assert!(r.is_ignored(&rp("sub/a.tmp"), false));
        assert!(
            !r.is_ignored(&rp("sub/keep.tmp"), false),
            "deeper file wins"
        );
        assert!(!r.is_ignored(&rp("sub/x/keep.tmp"), false));
        assert!(
            r.is_ignored(&rp("keep.tmp"), false),
            "nested file does not apply above"
        );
        assert!(r.is_ignored(&rp("top.txt"), false));
        assert!(
            !r.is_ignored(&rp("sub/top.txt"), false),
            "anchored to its own dir"
        );
        assert!(r.is_ignored(&rp("sub/local.txt"), false));
        assert!(!r.is_ignored(&rp("local.txt"), false));
        assert!(r.is_ignored(&rp("sub/deeper/x.md"), false));
        assert!(r.is_ignored(&rp("sub/deeper/y/x.md"), false));
        assert!(!r.is_ignored(&rp("sub/x.md"), false));
    }

    #[test]
    fn precedence_extra_over_gitignore_over_builtin() {
        let root = project(&[(".gitignore", "!node_modules/\n*.cfg\n!dist/\n")]);
        // .gitignore re-includes node_modules and dist; extra re-ignores dist and keeps a cfg.
        let r = rules(root.path(), &["dist/", "!local.cfg"], true);
        assert!(!r.is_ignored(&rp("node_modules/x.js"), false));
        assert!(r.is_ignored(&rp("dist/app.js"), false));
        assert!(r.is_ignored(&rp("a.cfg"), false));
        assert!(!r.is_ignored(&rp("local.cfg"), false));
        assert!(
            r.is_ignored(&rp("target/debug"), true),
            "untouched built-in"
        );
        // Without .gitignore the built-in applies again.
        let r = rules(root.path(), &[], false);
        assert!(r.is_ignored(&rp("node_modules/x.js"), false));
        // Extra can re-include a built-in too.
        let r = rules(root.path(), &["!target/"], false);
        assert!(!r.is_ignored(&rp("target/x"), false));
    }

    #[test]
    fn missing_or_non_file_gitignore_is_skipped() {
        let root = project(&[("a/.gitignore/x", "not a file")]);
        let r = rules(root.path(), &[], true);
        assert!(!r.is_ignored(&rp("a/b"), false));
        assert!(!r.is_ignored(&rp("zzz/b"), false));
    }

    #[test]
    fn gitignore_with_bom_and_bad_lines() {
        let root = project(&[(".gitignore", "\u{feff}*.bak\na[b\n*.o\n")]);
        let r = rules(root.path(), &[], true);
        assert!(r.is_ignored(&rp("x.bak"), false));
        assert!(r.is_ignored(&rp("x.o"), false));
    }

    #[test]
    fn info_exclude_is_not_read() {
        let root = project(&[(".git/info/exclude", "*.txt\n")]);
        let r = rules(root.path(), &[], true);
        assert!(!r.is_ignored(&rp("a.txt"), false));
    }

    #[test]
    fn native_separators_via_from_path() {
        let r = rules(Path::new("."), &["build/"], true);
        let native: PathBuf = ["build", "out", "a.o"].iter().collect();
        let p = RelPath::from_path(&native).unwrap();
        assert_eq!(p.as_str(), "build/out/a.o");
        assert!(r.is_ignored(&p, false));
        #[cfg(windows)]
        {
            let p = RelPath::from_path(Path::new(r"node_modules\x\y.js")).unwrap();
            assert_eq!(p.as_str(), "node_modules/x/y.js");
            assert!(r.is_ignored(&p, false));
            assert!(
                RelPath::new(r"build\a").is_err(),
                "backslash never reaches the rules"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn matching_is_case_insensitive_on_windows() {
        let root = project(&[(".gitignore", "*.LOG\n")]);
        let r = rules(root.path(), &["Secret/"], true);
        assert!(r.is_ignored(&rp("a.log"), false));
        assert!(r.is_ignored(&rp("secret/x"), false));
        assert!(r.is_ignored(&rp("Node_Modules/x"), false));
        assert!(r.is_ignored(&rp(".GIT/config"), false));
    }

    #[test]
    fn rules_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<IgnoreRules>();
    }
}
