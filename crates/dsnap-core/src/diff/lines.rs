//! Line diffs and hunks via `similar`. Owner: Chain G (DSNA-10).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::types::{DiffOptions, Hunk};

/// Line diff of two texts grouped into hunks with `opts.context` lines of context.
pub fn diff_text(old: &str, new: &str, opts: &DiffOptions) -> Vec<Hunk> {
    todo!("DSNA-10")
}

/// Lines added and removed between two texts.
pub fn line_counts(old: &str, new: &str, opts: &DiffOptions) -> (u32, u32) {
    todo!("DSNA-10")
}
