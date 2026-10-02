import type { Api } from './api';

/** `Api` methods that are Tauri commands (everything except event subscriptions). */
export type CommandMethod = Exclude<
  keyof Api,
  'onProjectChanged' | 'onProjectsChanged' | 'onVersionsChanged' | 'onProgress'
>;

/**
 * Tauri command name for each `Api` method. Chain O implements one
 * `#[tauri::command]` per entry with exactly this name. Arguments are passed as
 * an object keyed by the parameter names in `Api` (Tauri maps camelCase keys to
 * snake_case Rust parameters).
 */
export const COMMANDS = {
  listProjects: 'list_projects',
  addProject: 'add_project',
  renameProject: 'rename_project',
  removeProject: 'remove_project',
  relocateProject: 'relocate_project',
  revealInExplorer: 'reveal_in_explorer',
  pickFolder: 'pick_folder',
  getDataDir: 'get_data_dir',
  getProjectSettings: 'get_project_settings',
  setProjectSettings: 'set_project_settings',
  getGlobalSettings: 'get_global_settings',
  setGlobalSettings: 'set_global_settings',
  snapshot: 'snapshot',
  status: 'status',
  cancelOperation: 'cancel_operation',
  listVersions: 'list_versions',
  setLabel: 'set_label',
  setPinned: 'set_pinned',
  deleteVersion: 'delete_version',
  changes: 'changes',
  fileDiff: 'file_diff',
  readBlobAsDataUrl: 'read_blob_as_data_url',
  restorePlan: 'restore_plan',
  restoreProject: 'restore_project',
  restoreFile: 'restore_file',
  revertHunk: 'revert_hunk',
} as const satisfies Record<CommandMethod, string>;

/** Tauri event names for the `Api` subscriptions. */
export const EVENTS = {
  onProjectChanged: 'dsnap://project-changed',
  onProjectsChanged: 'dsnap://projects-changed',
  onVersionsChanged: 'dsnap://versions-changed',
  onProgress: 'dsnap://progress',
} as const satisfies Record<Exclude<keyof Api, CommandMethod>, string>;
