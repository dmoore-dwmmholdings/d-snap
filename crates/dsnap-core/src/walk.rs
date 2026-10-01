//! Folder walk producing unhashed entries. Owner: Chain E (DSNA-8).
//!
//! Never follows symlinks or junctions; records them as [`crate::EntryKind::Symlink`].
#![allow(unused_variables)] // stub signatures; remove when implemented

use std::path::Path;

use crate::error::Result;
use crate::ignore_rules::IgnoreRules;
use crate::types::{CancelToken, Entry, Progress, SkippedFile};

/// Options for [`walk`].
#[derive(Debug, Clone, Default)]
pub struct WalkOptions {
    /// Files above this size are skipped with [`crate::SkipReason::TooLarge`]; 0 = no cap.
    pub size_cap_bytes: u64,
    /// Optional progress callback ([`crate::Stage::Walk`]).
    pub progress: Option<Progress>,
    /// Optional cancellation token.
    pub cancel: Option<CancelToken>,
}

/// Result of [`walk`].
#[derive(Debug, Clone, Default)]
pub struct WalkOutput {
    /// Files, symlinks and directories, sorted by path, with `blob: None`.
    pub entries: Vec<Entry>,
    /// Paths left out (too large, non-UTF-8 name, unreadable).
    pub skipped: Vec<SkippedFile>,
}

/// Walk `root`, applying `rules`.
pub fn walk(root: &Path, rules: &IgnoreRules, opts: &WalkOptions) -> Result<WalkOutput> {
    todo!("DSNA-8")
}
