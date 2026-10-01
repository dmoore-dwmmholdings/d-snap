//! Ignore rules: built-in defaults, per-project patterns and (optionally) `.gitignore`.
//! Owner: Chain E (DSNA-8).
#![allow(unused_variables)] // stub signatures; remove when implemented

use std::path::Path;

use crate::error::Result;
use crate::types::{ProjectSettings, RelPath};

/// Paths always ignored, matched as directory names at any depth.
pub const BUILTIN_IGNORES: &[&str] = &[".git", "node_modules", "target", "dist", ".venv"];

/// Compiled ignore rules for one project.
#[derive(Debug)]
pub struct IgnoreRules {
    _todo: (),
}

impl IgnoreRules {
    /// Build the rules for the project at `root`.
    pub fn new(root: &Path, settings: &ProjectSettings) -> Result<Self> {
        todo!("DSNA-8")
    }

    /// Whether `path` (a directory if `is_dir`) is ignored, including via an ignored parent.
    pub fn is_ignored(&self, path: &RelPath, is_dir: bool) -> bool {
        todo!("DSNA-8")
    }
}
