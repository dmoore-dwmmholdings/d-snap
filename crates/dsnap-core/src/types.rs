//! Shared data types used across the core API, the CLI and the app.
//!
//! Every serializable type uses camelCase field names. Enums with data are tagged with a
//! `"kind"` field so the TypeScript frontend can switch on it.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// Database id of a tracked project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(pub i64);

/// Database id of a version (snapshot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VersionId(pub i64);

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for VersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// BLAKE3 hash of a file's bytes; names a blob in the object store. Serialized as 64 hex chars.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobHash(pub [u8; 32]);

impl BlobHash {
    /// Hash `bytes` with BLAKE3.
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Lowercase hex form (64 chars).
    pub fn to_hex(&self) -> String {
        self.to_string()
    }
}

impl From<blake3::Hash> for BlobHash {
    fn from(h: blake3::Hash) -> Self {
        Self(*h.as_bytes())
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobHash({self})")
    }
}

impl FromStr for BlobHash {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return Err(Error::InvalidInput(format!(
                "blob hash must be 64 hex chars, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            let hi = hex_val(pair[0]);
            let lo = hex_val(pair[1]);
            match (hi, lo) {
                (Some(hi), Some(lo)) => out[i] = (hi << 4) | lo,
                _ => {
                    return Err(Error::InvalidInput(format!(
                        "invalid hex in blob hash: {s}"
                    )));
                }
            }
        }
        Ok(Self(out))
    }
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl Serialize for BlobHash {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BlobHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Project-relative, `/`-separated UTF-8 path, e.g. `src/main.rs`.
///
/// Never empty, never absolute, and has no `.`, `..` or empty components, so
/// [`RelPath::to_path`] always stays under the root it is joined to.
///
/// On Windows each component must also be a valid Win32 file name: no `<>:"|?*` or control
/// characters (`:` would allow drive-relative paths like `C:x` and alternate data streams), no
/// trailing `.` or space (Win32 strips them, so `.. ` would become `..`), and no reserved
/// device name (`CON`, `NUL`, `COM1`, ...).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

/// Whether one path component is acceptable on this platform.
fn valid_component(c: &str) -> bool {
    if c.is_empty() || c == "." || c == ".." || c.contains(['\\', '\0']) {
        return false;
    }
    if cfg!(windows) {
        if c.chars()
            .any(|ch| ch < ' ' || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return false;
        }
        if c.ends_with(['.', ' ']) {
            return false;
        }
        let stem = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
        let device = |p: &[u8]| p.eq_ignore_ascii_case(b"COM") || p.eq_ignore_ascii_case(b"LPT");
        let reserved = ["CON", "PRN", "AUX", "NUL"]
            .iter()
            .any(|r| stem.eq_ignore_ascii_case(r))
            || matches!(stem.as_bytes(), [p @ .., b'1'..=b'9'] if device(p));
        if reserved {
            return false;
        }
    }
    true
}

impl RelPath {
    /// Validate and wrap a `/`-separated relative path.
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        if s.is_empty() {
            return Err(Error::InvalidInput("relative path is empty".into()));
        }
        if s.starts_with('/') {
            return Err(Error::InvalidInput(format!(
                "relative path is absolute: {s}"
            )));
        }
        if let Some(bad) = s.split('/').find(|c| !valid_component(c)) {
            return Err(Error::InvalidInput(format!(
                "invalid component {bad:?} in relative path {s:?}"
            )));
        }
        Ok(Self(s))
    }

    /// Build from a relative filesystem path (as produced by a walk).
    ///
    /// Returns [`Error::InvalidInput`] for a non-UTF-8 component, an absolute path, or `..`.
    pub fn from_path(rel: &Path) -> Result<Self> {
        let mut parts: Vec<&str> = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(os) => match os.to_str() {
                    Some(s) => parts.push(s),
                    None => {
                        return Err(Error::InvalidInput(format!(
                            "non-UTF-8 path: {}",
                            rel.display()
                        )));
                    }
                },
                Component::CurDir => {}
                _ => {
                    return Err(Error::InvalidInput(format!(
                        "path is not project-relative: {}",
                        rel.display()
                    )));
                }
            }
        }
        Self::new(parts.join("/"))
    }

    /// Build from an absolute `path` under project `root`.
    pub fn from_abs(root: &Path, path: &Path) -> Result<Self> {
        let rel = path.strip_prefix(root).map_err(|_| {
            Error::InvalidInput(format!(
                "{} is not under {}",
                path.display(),
                root.display()
            ))
        })?;
        Self::from_path(rel)
    }

    /// Resolve against project `root` into a native filesystem path.
    pub fn to_path(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        p.extend(self.0.split('/'));
        p
    }

    /// The path as a `/`-separated string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Last component.
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// Parent path, or `None` for a top-level entry.
    pub fn parent(&self) -> Option<RelPath> {
        self.0.rsplit_once('/').map(|(p, _)| RelPath(p.to_owned()))
    }

    /// Append one or more `/`-separated components.
    pub fn join(&self, child: &str) -> Result<RelPath> {
        Self::new(format!("{}/{}", self.0, child))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for RelPath {
    type Error = Error;

    fn try_from(s: String) -> Result<Self> {
        Self::new(s)
    }
}

impl From<RelPath> for String {
    fn from(p: RelPath) -> Self {
        p.0
    }
}

impl AsRef<str> for RelPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A blob as recorded in the store and the `blobs` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobInfo {
    /// Content hash.
    pub hash: BlobHash,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Compressed size on disk in bytes.
    pub stored_size: u64,
}

/// What kind of filesystem object an entry is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EntryKind {
    /// Regular file; content is in the entry's blob.
    File,
    /// Symlink or junction, stored as the link itself (never followed).
    Symlink {
        /// Link target exactly as read from the link.
        target: String,
    },
    /// Directory (recorded so empty directories can be restored).
    Dir,
}

/// One path in a version's file list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Project-relative path.
    pub path: RelPath,
    /// File, symlink or directory.
    pub kind: EntryKind,
    /// Content hash; `Some` for files, `None` for symlinks, dirs and not-yet-hashed walk results.
    pub blob: Option<BlobHash>,
    /// Size in bytes (0 for dirs).
    pub size: u64,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// Read-only attribute.
    pub readonly: bool,
}

/// How a version was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VersionKind {
    /// User pressed the snapshot button.
    #[default]
    Manual,
    /// Auto-snapshot timer or idle trigger.
    Auto,
    /// `dsnap snap` from the command line.
    Cli,
    /// Taken automatically before a restore.
    Safety,
}

/// Added/modified/deleted counts relative to the previous version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeCounts {
    /// Paths present now but not before.
    pub added: u32,
    /// Paths present in both with different content.
    pub modified: u32,
    /// Paths present before but not now.
    pub deleted: u32,
}

/// A snapshot of a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    /// Version id.
    pub id: VersionId,
    /// Owning project.
    pub project_id: ProjectId,
    /// User label (defaults to a timestamp).
    pub label: String,
    /// Creation time, milliseconds since the Unix epoch.
    pub created_at_ms: i64,
    /// How the version was created.
    pub kind: VersionKind,
    /// Pinned versions are never pruned by retention.
    pub pinned: bool,
    /// Some files kept changing while they were captured.
    pub unstable: bool,
    /// Change counts versus the previous version.
    pub counts: ChangeCounts,
}

/// Auto-snapshot mode for a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AutoSnapshot {
    /// No automatic snapshots.
    #[default]
    Off,
    /// Snapshot every `secs` seconds (if something changed).
    Every {
        /// Interval in seconds.
        secs: u64,
    },
    /// Snapshot after `secs` seconds with no file changes.
    AfterIdle {
        /// Idle period in seconds.
        secs: u64,
    },
}

/// Per-project settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectSettings {
    /// Extra gitignore-style patterns on top of the built-in defaults.
    pub extra_ignore: Vec<String>,
    /// Apply the project's `.gitignore` files (DSNA-3 default: yes).
    pub respect_gitignore: bool,
    /// Auto-snapshot mode.
    pub auto_snapshot: AutoSnapshot,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            extra_ignore: Vec::new(),
            respect_gitignore: true,
            auto_snapshot: AutoSnapshot::Off,
        }
    }
}

/// App-wide settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GlobalSettings {
    /// Files larger than this are skipped and reported (default 50 MiB).
    pub size_cap_bytes: u64,
    /// Unpinned versions kept per project (default 200).
    pub retention_keep: u32,
}

impl GlobalSettings {
    /// Default size cap: 50 MiB.
    pub const DEFAULT_SIZE_CAP_BYTES: u64 = 50 * 1024 * 1024;
    /// Default retention: 200 versions.
    pub const DEFAULT_RETENTION_KEEP: u32 = 200;
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            size_cap_bytes: Self::DEFAULT_SIZE_CAP_BYTES,
            retention_keep: Self::DEFAULT_RETENTION_KEEP,
        }
    }
}

/// A tracked folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// Project id.
    pub id: ProjectId,
    /// Display name.
    pub name: String,
    /// Absolute path of the project folder.
    pub root: PathBuf,
    /// Project settings.
    pub settings: ProjectSettings,
    /// The folder no longer exists at `root`.
    pub missing: bool,
}

/// How a path differs between two sides of a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChangeStatus {
    /// Only on the new side.
    Added,
    /// On both sides with different content or kind.
    Modified,
    /// Only on the old side.
    Deleted,
    /// Moved from `from`. Content matches, except for a case-only rename in case-insensitive
    /// mode, where `old` and `new` may also differ in content, kind or flags (DSNA-89).
    Renamed {
        /// Old path.
        from: RelPath,
    },
}

/// One changed path in a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// Path on the new side (old side for deletions).
    pub path: RelPath,
    /// Kind of change.
    pub status: ChangeStatus,
    /// Entry on the old side, if any.
    pub old: Option<Entry>,
    /// Entry on the new side, if any.
    pub new: Option<Entry>,
    /// Lines added, when computed for a text file.
    pub lines_added: Option<u32>,
    /// Lines removed, when computed for a text file.
    pub lines_removed: Option<u32>,
}

/// Options for a line diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiffOptions {
    /// Ignore whitespace and line-ending differences.
    pub ignore_whitespace: bool,
    /// Unchanged context lines around each hunk (default 3).
    pub context: u32,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            ignore_whitespace: false,
            context: 3,
        }
    }
}

/// Role of one line in a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineTag {
    /// Present on both sides.
    Equal,
    /// Only on the new side.
    Insert,
    /// Only on the old side.
    Delete,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    /// Equal, insert or delete.
    pub tag: LineTag,
    /// 1-based line number on the old side.
    pub old_no: Option<u32>,
    /// 1-based line number on the new side.
    pub new_no: Option<u32>,
    /// Line text without its line ending.
    pub text: String,
}

/// A contiguous group of changes with context.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    /// 1-based first old line (0 when the old range is empty).
    pub old_start: u32,
    /// Number of old lines.
    pub old_len: u32,
    /// 1-based first new line (0 when the new range is empty).
    pub new_start: u32,
    /// Number of new lines.
    pub new_len: u32,
    /// Lines in order.
    pub lines: Vec<DiffLine>,
}

/// Content of a single-file diff.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DiffBody {
    /// Line diff of text content.
    Text {
        /// Hunks in order.
        hunks: Vec<Hunk>,
    },
    /// Binary content: only size and hash are compared.
    Binary {
        /// Old size, if the old side exists.
        old_size: Option<u64>,
        /// New size, if the new side exists.
        new_size: Option<u64>,
        /// Old hash, if the old side exists.
        old_hash: Option<BlobHash>,
        /// New hash, if the new side exists.
        new_hash: Option<BlobHash>,
    },
    /// Image shown side by side; fetch bytes with `read_blob`.
    Image {
        /// Old image blob.
        old: Option<BlobHash>,
        /// New image blob.
        new: Option<BlobHash>,
        /// MIME type, e.g. `image/png`.
        mime: String,
    },
    /// Too large to diff in the UI.
    TooLarge {
        /// Larger of the two sizes.
        size: u64,
    },
}

/// Diff of one file between two sides.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    /// Path being compared.
    pub path: RelPath,
    /// Diff content.
    pub body: DiffBody,
}

/// One side of a comparison: a stored version or the folder as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum VersionRef {
    /// A stored version.
    Version(VersionId),
    /// The project folder on disk now.
    WorkingTree,
}

/// Why a file was left out of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SkipReason {
    /// Larger than the size cap.
    TooLarge {
        /// File size in bytes.
        size: u64,
    },
    /// Locked by another process after retries.
    Locked,
    /// Name is not valid UTF-8.
    NonUtf8Name,
    /// Could not be read for another reason.
    Unreadable {
        /// Error message.
        msg: String,
    },
}

/// A file left out of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFile {
    /// Path (lossy for non-UTF-8 names, which cannot be a [`RelPath`]).
    pub path: String,
    /// Why it was skipped.
    pub reason: SkipReason,
}

/// Outcome of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotReport {
    /// The new version, or `None` when nothing changed.
    pub version: Option<Version>,
    /// Files left out.
    pub skipped: Vec<SkippedFile>,
    /// Files that kept changing while captured.
    pub unstable_paths: Vec<RelPath>,
}

/// What a whole-project restore will do.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    /// Version being restored.
    pub target: VersionId,
    /// Files to (re)write.
    pub write: Vec<RelPath>,
    /// Files added since the target, to be removed.
    pub delete: Vec<RelPath>,
    /// Directories to recreate.
    pub create_dirs: Vec<RelPath>,
}

/// Outcome of a restore.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    /// Safety snapshot taken before writing (restoring it undoes the restore).
    pub safety_version: VersionId,
    /// Paths written.
    pub written: Vec<RelPath>,
    /// Paths removed.
    pub deleted: Vec<RelPath>,
    /// Paths that could not be written or removed, with the error.
    pub failed: Vec<(RelPath, String)>,
}

/// Outcome of applying retention to a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionReport {
    /// Versions removed.
    pub versions_deleted: u32,
    /// Blobs removed from the store.
    pub blobs_pruned: u32,
    /// Stored bytes freed.
    pub bytes_freed: u64,
}

/// Phase of a long-running operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    /// Walking the folder.
    Walk,
    /// Hashing and storing changed files.
    Hash,
    /// Writing files during a restore.
    Restore,
}

/// Progress update from a long-running operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    /// Current phase.
    pub stage: Stage,
    /// Items done in this phase.
    pub done: u64,
    /// Total items, when known.
    pub total: Option<u64>,
    /// Item being processed, if any.
    pub path: Option<RelPath>,
}

/// Progress callback, cheap to clone and safe to call from any thread.
#[derive(Clone)]
pub struct Progress(Arc<dyn Fn(&ProgressEvent) + Send + Sync>);

impl Progress {
    /// Wrap a callback.
    pub fn new(f: impl Fn(&ProgressEvent) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Deliver one event.
    pub fn report(&self, event: &ProgressEvent) {
        (self.0)(event)
    }
}

impl fmt::Debug for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Progress(..)")
    }
}

/// Cooperative cancellation flag shared between a caller and an operation.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// New, not-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// `Err(Error::Cancelled)` once cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Options for [`crate::Dsnap::snapshot`].
#[derive(Debug, Clone, Default)]
pub struct SnapshotOptions {
    /// Label; `None` uses a timestamp.
    pub label: Option<String>,
    /// How the snapshot was triggered.
    pub kind: VersionKind,
    /// Optional progress callback.
    pub progress: Option<Progress>,
    /// Optional cancellation token.
    pub cancel: Option<CancelToken>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use std::fmt::Debug;

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(v: &T) -> String {
        let json = serde_json::to_string(v).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, v, "round trip changed value; json = {json}");
        json
    }

    fn rp(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    fn entry(path: &str, kind: EntryKind) -> Entry {
        Entry {
            path: rp(path),
            kind,
            blob: Some(BlobHash::of(path.as_bytes())),
            size: 12,
            mtime_ns: 1_700_000_000_123_456_789,
            readonly: true,
        }
    }

    fn version() -> Version {
        Version {
            id: VersionId(7),
            project_id: ProjectId(3),
            label: "before agent turn".into(),
            created_at_ms: 1_700_000_000_000,
            kind: VersionKind::Cli,
            pinned: true,
            unstable: false,
            counts: ChangeCounts {
                added: 1,
                modified: 2,
                deleted: 3,
            },
        }
    }

    #[test]
    fn blob_hash_hex_round_trip() {
        let h = BlobHash::of(b"hello");
        let s = h.to_string();
        assert_eq!(s.len(), 64);
        assert_eq!(s, blake3::hash(b"hello").to_hex().as_str());
        assert_eq!(s.parse::<BlobHash>().unwrap(), h);
        assert_eq!(s.to_uppercase().parse::<BlobHash>().unwrap(), h);
        assert!("abc".parse::<BlobHash>().is_err());
        assert!("g".repeat(64).parse::<BlobHash>().is_err());
        assert_eq!(round_trip(&h), format!("\"{s}\""));
    }

    #[test]
    fn rel_path_validation() {
        for ok in ["a", "a/b.txt", "src/.hidden", "dir/with space/x"] {
            assert_eq!(rp(ok).as_str(), ok);
        }
        for bad in [
            "", "/a", "a/", "a//b", "./a", "a/../b", "..", "a\\b", "a\0b", "x/..",
        ] {
            assert!(RelPath::new(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(serde_json::from_str::<RelPath>("\"../x\"").is_err());
    }

    /// Inputs that would escape the root or alias another file on Windows.
    const WINDOWS_HOSTILE: &[&str] = &[
        "C:/x",
        "c:",
        "x/C:evil.txt",
        "x/c:",
        "ab:c",
        "a/b:stream:$DATA",
        "x/.. /y",
        "x/... ",
        "a.",
        "a ",
        "x/a?b",
        "x/a*b",
        "x/a<b",
        "x/a>b",
        "x/a|b",
        "x/a\"b",
        "x/a\u{1}b",
        "CON",
        "x/nul.txt",
        "Com1",
        "x/lpt9.log",
        "aux .txt",
    ];

    #[cfg(windows)]
    #[test]
    fn rel_path_rejects_windows_hostile_names() {
        for bad in WINDOWS_HOSTILE {
            assert!(RelPath::new(*bad).is_err(), "{bad:?} should be rejected");
            assert!(
                serde_json::to_string(bad)
                    .ok()
                    .and_then(|j| serde_json::from_str::<RelPath>(&j).ok())
                    .is_none()
            );
        }
        for ok in [
            "COM0",
            "com10",
            "console",
            "a.b",
            "x/.hidden",
            "LPT",
            "nul2",
        ] {
            assert!(RelPath::new(ok).is_ok(), "{ok:?} should be accepted");
        }
    }

    #[test]
    fn to_path_always_stays_under_root() {
        let root = std::env::temp_dir().join("dsnap-root");
        let candidates = [
            "a",
            "a/b.txt",
            "src/.hidden",
            "dir/with space/x",
            "ä/ö",
            "a..b",
            "...x",
            "x/..y",
        ];
        for s in candidates.iter().chain(WINDOWS_HOSTILE) {
            let Ok(p) = RelPath::new(*s) else { continue };
            let abs = p.to_path(&root);
            assert!(abs.starts_with(&root), "{s:?} -> {}", abs.display());
            assert_eq!(
                abs.components().count(),
                root.components().count() + s.split('/').count(),
                "{s:?} -> {}",
                abs.display()
            );
            assert_eq!(RelPath::from_abs(&root, &abs).unwrap(), p);
        }
    }

    #[test]
    fn rel_path_helpers() {
        let p = rp("src/lib/mod.rs");
        assert_eq!(p.file_name(), "mod.rs");
        assert_eq!(p.parent(), Some(rp("src/lib")));
        assert_eq!(rp("top").parent(), None);
        assert_eq!(rp("src").join("a/b").unwrap(), rp("src/a/b"));
    }

    #[test]
    fn rel_path_native_conversion() {
        let root = std::env::temp_dir().join("proj");
        let p = rp("src/main.rs");
        let abs = p.to_path(&root);
        assert_eq!(abs, root.join("src").join("main.rs"));
        assert_eq!(RelPath::from_abs(&root, &abs).unwrap(), p);
        assert_eq!(
            RelPath::from_path(&Path::new("a").join("b")).unwrap(),
            rp("a/b")
        );
        assert!(RelPath::from_abs(&root, &std::env::temp_dir().join("other")).is_err());
        assert!(RelPath::from_path(Path::new("../x")).is_err());
    }

    #[test]
    fn json_shapes_are_camel_case_and_tagged() {
        assert_eq!(
            round_trip(&EntryKind::Symlink { target: "x".into() }),
            r#"{"kind":"symlink","target":"x"}"#
        );
        assert_eq!(round_trip(&EntryKind::File), r#"{"kind":"file"}"#);
        assert_eq!(
            round_trip(&VersionRef::Version(VersionId(4))),
            r#"{"kind":"version","id":4}"#
        );
        assert_eq!(
            round_trip(&VersionRef::WorkingTree),
            r#"{"kind":"workingTree"}"#
        );
        assert_eq!(
            round_trip(&AutoSnapshot::AfterIdle { secs: 30 }),
            r#"{"kind":"afterIdle","secs":30}"#
        );
        assert_eq!(round_trip(&VersionKind::Safety), r#""safety""#);
        let e = round_trip(&entry("l", EntryKind::Symlink { target: "t".into() }));
        assert!(
            e.contains(r#""kind":{"kind":"symlink","target":"t"}"#) && e.contains(r#""mtimeNs":"#),
            "{e}"
        );
        let v = round_trip(&version());
        assert!(v.contains(r#""createdAtMs":1700000000000"#), "{v}");
        assert!(v.contains(r#""projectId":3"#), "{v}");
        let body = DiffBody::Binary {
            old_size: Some(1),
            new_size: None,
            old_hash: None,
            new_hash: None,
        };
        assert!(round_trip(&body).contains(r#""oldSize":1"#));
    }

    #[test]
    fn settings_defaults_and_partial_json() {
        let g: GlobalSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(g.size_cap_bytes, 50 * 1024 * 1024);
        assert_eq!(g.retention_keep, 200);
        let p: ProjectSettings = serde_json::from_str(r#"{"extraIgnore":["*.log"]}"#).unwrap();
        assert!(p.respect_gitignore);
        assert_eq!(p.auto_snapshot, AutoSnapshot::Off);
        assert_eq!(p.extra_ignore, vec!["*.log".to_string()]);
        assert_eq!(DiffOptions::default().context, 3);
    }

    #[test]
    fn all_types_round_trip() {
        let file = entry("src/a.rs", EntryKind::File);
        let link = entry(
            "link",
            EntryKind::Symlink {
                target: "../t".into(),
            },
        );
        let dir = entry("empty", EntryKind::Dir);
        round_trip(&file);
        round_trip(&link);
        round_trip(&dir);
        round_trip(&version());
        round_trip(&ChangeCounts::default());
        round_trip(&AutoSnapshot::Off);
        round_trip(&AutoSnapshot::Every { secs: 60 });
        round_trip(&ProjectSettings {
            extra_ignore: vec!["build/".into()],
            respect_gitignore: false,
            auto_snapshot: AutoSnapshot::Every { secs: 5 },
        });
        round_trip(&GlobalSettings::default());
        round_trip(&Project {
            id: ProjectId(1),
            name: "demo".into(),
            root: std::env::temp_dir().join("demo"),
            settings: ProjectSettings::default(),
            missing: true,
        });
        for status in [
            ChangeStatus::Added,
            ChangeStatus::Modified,
            ChangeStatus::Deleted,
            ChangeStatus::Renamed { from: rp("old.rs") },
        ] {
            round_trip(&FileChange {
                path: rp("src/a.rs"),
                status,
                old: Some(file.clone()),
                new: None,
                lines_added: Some(3),
                lines_removed: None,
            });
        }
        round_trip(&DiffOptions {
            ignore_whitespace: true,
            context: 0,
        });
        let hunk = Hunk {
            old_start: 1,
            old_len: 2,
            new_start: 1,
            new_len: 1,
            lines: vec![
                DiffLine {
                    tag: LineTag::Equal,
                    old_no: Some(1),
                    new_no: Some(1),
                    text: "a".into(),
                },
                DiffLine {
                    tag: LineTag::Delete,
                    old_no: Some(2),
                    new_no: None,
                    text: "b".into(),
                },
                DiffLine {
                    tag: LineTag::Insert,
                    old_no: None,
                    new_no: Some(2),
                    text: "c".into(),
                },
            ],
        };
        round_trip(&hunk);
        for body in [
            DiffBody::Text { hunks: vec![hunk] },
            DiffBody::Binary {
                old_size: Some(1),
                new_size: Some(2),
                old_hash: Some(BlobHash::of(b"1")),
                new_hash: Some(BlobHash::of(b"2")),
            },
            DiffBody::Image {
                old: None,
                new: Some(BlobHash::of(b"png")),
                mime: "image/png".into(),
            },
            DiffBody::TooLarge { size: 10 },
        ] {
            round_trip(&FileDiff {
                path: rp("x"),
                body,
            });
        }
        round_trip(&VersionRef::WorkingTree);
        for reason in [
            SkipReason::TooLarge { size: 99 },
            SkipReason::Locked,
            SkipReason::NonUtf8Name,
            SkipReason::Unreadable {
                msg: "denied".into(),
            },
        ] {
            round_trip(&SkippedFile {
                path: "big.bin".into(),
                reason,
            });
        }
        round_trip(&SnapshotReport {
            version: Some(version()),
            skipped: vec![],
            unstable_paths: vec![rp("log.txt")],
        });
        round_trip(&SnapshotReport::default());
        round_trip(&RestorePlan {
            target: VersionId(2),
            write: vec![rp("a")],
            delete: vec![rp("b")],
            create_dirs: vec![rp("c")],
        });
        round_trip(&RestoreReport {
            safety_version: VersionId(9),
            written: vec![rp("a")],
            deleted: vec![],
            failed: vec![(rp("locked.txt"), "in use".into())],
        });
        round_trip(&RetentionReport::default());
        round_trip(&BlobInfo {
            hash: BlobHash::of(b"x"),
            size: 10,
            stored_size: 4,
        });
        round_trip(&ProgressEvent {
            stage: Stage::Hash,
            done: 1,
            total: Some(2),
            path: Some(rp("a")),
        });
    }

    #[test]
    fn cancel_token_and_progress() {
        let t = CancelToken::new();
        let t2 = t.clone();
        assert!(t.check().is_ok());
        t2.cancel();
        assert!(matches!(t.check(), Err(Error::Cancelled)));

        let seen = Arc::new(std::sync::Mutex::new(0u64));
        let s = seen.clone();
        let p = Progress::new(move |e| *s.lock().unwrap() += e.done);
        p.clone().report(&ProgressEvent {
            stage: Stage::Walk,
            done: 5,
            total: None,
            path: None,
        });
        assert_eq!(*seen.lock().unwrap(), 5);
        let opts = SnapshotOptions::default();
        assert_eq!(opts.kind, VersionKind::Manual);
    }
}
