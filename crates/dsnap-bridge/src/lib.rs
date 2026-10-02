//! The D-Snap app backend without Tauri (DSNA-66): what each command does, the error the
//! frontend receives, and the event payloads. `app/src-tauri` wraps [`backend::Backend`]
//! in `#[tauri::command]`s. Keeping Tauri out lets these tests run on every target (a test
//! binary that links Tauri cannot start on Windows without an app manifest).

pub mod backend;
pub mod error;

/// Every Tauri command name; must match `COMMANDS` in `app/src/lib/api/commands.ts`. The
/// app checks at compile time that it registers exactly these.
pub const COMMAND_NAMES: &[&str] = &[
    "list_projects",
    "add_project",
    "rename_project",
    "remove_project",
    "relocate_project",
    "reveal_in_explorer",
    "pick_folder",
    "get_data_dir",
    "get_project_settings",
    "set_project_settings",
    "get_global_settings",
    "set_global_settings",
    "snapshot",
    "status",
    "cancel_operation",
    "list_versions",
    "set_label",
    "set_pinned",
    "delete_version",
    "changes",
    "file_diff",
    "read_blob_as_data_url",
    "restore_plan",
    "restore_project",
    "restore_file",
    "revert_hunk",
];

/// Whether two name lists are equal, usable in a `const` assertion.
pub const fn same_names(a: &[&str], b: &[&str]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        let (x, y) = (a[i].as_bytes(), b[i].as_bytes());
        if x.len() != y.len() {
            return false;
        }
        let mut j = 0;
        while j < x.len() {
            if x[j] != y[j] {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_match_commands_ts() {
        let ts = include_str!("../../../app/src/lib/api/commands.ts");
        let block = ts
            .split("export const COMMANDS = {")
            .nth(1)
            .and_then(|s| s.split("} as const").next())
            .unwrap();
        let mut from_ts: Vec<&str> = block.lines().filter_map(|l| l.split('\'').nth(1)).collect();
        from_ts.sort_unstable();
        let mut ours = COMMAND_NAMES.to_vec();
        ours.sort_unstable();
        assert_eq!(ours, from_ts);
    }

    #[test]
    fn same_names_compares_in_order() {
        assert!(same_names(&["a", "bc"], &["a", "bc"]));
        assert!(!same_names(&["a", "bc"], &["a", "bd"]));
        assert!(!same_names(&["a"], &["a", "b"]));
    }
}
