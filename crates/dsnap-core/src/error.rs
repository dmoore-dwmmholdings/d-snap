//! The core error type.

use std::path::PathBuf;

/// Result alias using [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every error the core library returns.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Filesystem error on a specific path.
    #[error("{}: {source}", path.display())]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// SQLite error.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    /// A project, version, path or blob does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The project folder no longer exists at its recorded path.
    #[error("project folder is missing: {}", root.display())]
    ProjectMissing {
        /// Recorded project root.
        root: PathBuf,
    },
    /// The operation was cancelled through a [`crate::CancelToken`].
    #[error("cancelled")]
    Cancelled,
    /// The safety snapshot before a restore failed, so nothing was written.
    #[error("safety snapshot failed; restore not started: {source}")]
    SafetySnapshotFailed {
        /// Why the safety snapshot failed.
        #[source]
        source: Box<Error>,
    },
    /// A new version would reference a blob missing from the store (pruned after the snapshot
    /// deduplicated against it). Nothing was committed; store the blob again and retry.
    /// See [`crate::db::Db::insert_version`].
    #[error("blob {0} is missing from the store")]
    BlobMissing(crate::types::BlobHash),
    /// Stored data is inconsistent (bad blob, hash mismatch, malformed row).
    #[error("corrupt data: {0}")]
    Corrupt(String),
    /// Caller passed an invalid argument.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl Error {
    /// Build an [`Error::Io`] for `path`.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Attach a path to an `io::Result`.
pub trait IoResultExt<T> {
    /// Convert the error into [`Error::Io`] naming `path`.
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoResultExt<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| Error::io(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_path_and_cause() {
        let e: Result<()> = Err(std::io::Error::other("boom")).at("some/file");
        let msg = e.unwrap_err().to_string();
        assert!(msg.contains("file") && msg.contains("boom"), "{msg}");
        let s = Error::SafetySnapshotFailed {
            source: Box::new(Error::Cancelled),
        };
        assert!(s.to_string().contains("cancelled"));
    }
}
