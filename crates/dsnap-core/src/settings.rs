//! Project and global settings. Owner: Chain L (DSNA-15).

use std::path::Path;

use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::ignore_rules::IgnoreRules;
use crate::types::{AutoSnapshot, GlobalSettings, ProjectId, ProjectSettings};

/// Longest accepted auto-snapshot interval or idle period: 7 days.
pub const MAX_AUTO_SNAPSHOT_SECS: u64 = 7 * 24 * 60 * 60;

impl Dsnap {
    /// A project's settings.
    pub fn project_settings(&self, id: ProjectId) -> Result<ProjectSettings> {
        Ok(self.db.get_project(id)?.settings)
    }

    /// Replace a project's settings after [`validate_project_settings`].
    pub fn set_project_settings(&self, id: ProjectId, settings: &ProjectSettings) -> Result<()> {
        validate_project_settings(settings)?;
        self.db.set_project_settings(id, settings)
    }

    /// App-wide settings.
    pub fn global_settings(&self) -> Result<GlobalSettings> {
        self.db.global_settings()
    }

    /// Replace app-wide settings after [`validate_global_settings`].
    pub fn set_global_settings(&self, settings: &GlobalSettings) -> Result<()> {
        validate_global_settings(settings)?;
        self.db.set_global_settings(settings)
    }
}

/// Check project settings; [`Error::InvalidInput`] names the first problem.
///
/// - Every `extra_ignore` entry is one non-blank line that
///   [`IgnoreRules`] accepts.
/// - Auto-snapshot `secs` is between 1 and [`MAX_AUTO_SNAPSHOT_SECS`].
pub fn validate_project_settings(settings: &ProjectSettings) -> Result<()> {
    for pattern in &settings.extra_ignore {
        if pattern.trim().is_empty() {
            return Err(Error::InvalidInput("ignore pattern is blank".into()));
        }
        if pattern.contains(['\n', '\r']) {
            return Err(Error::InvalidInput(format!(
                "ignore pattern {pattern:?} spans several lines"
            )));
        }
    }
    // Same parser the walker uses; reads nothing from disk.
    IgnoreRules::new(Path::new(""), settings)?;
    match settings.auto_snapshot {
        AutoSnapshot::Off => {}
        AutoSnapshot::Every { secs } | AutoSnapshot::AfterIdle { secs } => {
            if !(1..=MAX_AUTO_SNAPSHOT_SECS).contains(&secs) {
                return Err(Error::InvalidInput(format!(
                    "auto-snapshot period must be 1 to {MAX_AUTO_SNAPSHOT_SECS} seconds, got {secs}"
                )));
            }
        }
    }
    Ok(())
}

/// Check global settings: `size_cap_bytes` and `retention_keep` are at least 1 (retention
/// always keeps the newest version), and `size_cap_bytes` fits the index (`i64`).
pub fn validate_global_settings(settings: &GlobalSettings) -> Result<()> {
    if settings.size_cap_bytes == 0 || i64::try_from(settings.size_cap_bytes).is_err() {
        return Err(Error::InvalidInput(format!(
            "size cap must be 1 to {} bytes, got {}",
            i64::MAX,
            settings.size_cap_bytes
        )));
    }
    if settings.retention_keep == 0 {
        return Err(Error::InvalidInput(
            "retention must keep at least 1 version".into(),
        ));
    }
    Ok(())
}
