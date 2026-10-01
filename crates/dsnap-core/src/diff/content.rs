//! Per-file diff: text, binary, image or too large. Owner: Chain G (DSNA-10).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{DiffOptions, FileDiff, ProjectId, RelPath, VersionId, VersionRef};

/// Files above this size are reported as [`crate::DiffBody::TooLarge`].
pub const MAX_TEXT_DIFF_BYTES: u64 = 8 * 1024 * 1024;

/// How a file's content is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentClass {
    /// Valid text: line diff.
    Text,
    /// Image with this MIME type: side-by-side preview.
    Image(String),
    /// Anything else: size and hash only.
    Binary,
}

/// Classify content by its path and bytes.
pub fn classify(path: &RelPath, bytes: &[u8]) -> ContentClass {
    todo!("DSNA-10")
}

impl Dsnap {
    /// Diff one file between `from` (default: version before `to`) and `to`.
    pub fn file_diff(
        &self,
        project: ProjectId,
        from: Option<VersionId>,
        to: VersionRef,
        path: &RelPath,
        opts: &DiffOptions,
    ) -> Result<FileDiff> {
        todo!("DSNA-10")
    }
}
