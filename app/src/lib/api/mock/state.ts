// In-memory model behind the mock Api: blobs, projects, working trees and versions.
import type {
  BlobHash,
  ChangeCounts,
  Entry,
  FileChange,
  GlobalSettings,
  Project,
  ProjectId,
  RelPath,
  Version,
} from '../types';
import { DEFAULT_DIFF_OPTIONS } from '../types';
import { lineDiff } from './lines';
import { bytesToBase64, fakeHash, imageMime, isIgnored, utf8Length } from './util';

/** Stored content. Binary data is base64. `size` may exceed the data (simulated huge files). */
export interface BlobData {
  data: string;
  binary: boolean;
  size: number;
}

/** A file list keyed by path. */
export type Tree = Map<RelPath, Entry>;

export interface VersionRecord {
  version: Version;
  entries: Tree;
}

export interface ProjectRecord {
  project: Project;
  /** The folder "on disk". */
  tree: Tree;
  /** Oldest first. */
  versions: VersionRecord[];
}

/** Text diffs above this size come back as `tooLarge`. */
export const DIFF_SIZE_LIMIT = 5 * 1024 * 1024;

export class MockState {
  readonly blobs = new Map<BlobHash, BlobData>();
  readonly projects = new Map<ProjectId, ProjectRecord>();
  global: GlobalSettings = { sizeCapBytes: 50 * 1024 * 1024, retentionKeep: 200 };
  nextProjectId = 1;
  nextVersionId = 1;

  putText(text: string, size?: number): BlobHash {
    const hash = fakeHash(`t:${text}:${size ?? ''}`);
    this.blobs.set(hash, { data: text, binary: false, size: size ?? utf8Length(text) });
    return hash;
  }

  putBinary(bytes: Uint8Array, size?: number): BlobHash {
    const data = bytesToBase64(bytes);
    const hash = fakeHash(`b:${data}:${size ?? ''}`);
    this.blobs.set(hash, { data, binary: true, size: size ?? bytes.length });
    return hash;
  }

  blob(hash: BlobHash): BlobData | undefined {
    return this.blobs.get(hash);
  }

  /** Text of an entry for diffing. Symlinks diff as their target. */
  entryText(entry: Entry | null): string {
    if (!entry) return '';
    if (entry.kind.kind === 'symlink') return `symlink -> ${entry.kind.target}\n`;
    if (!entry.blob) return '';
    const b = this.blobs.get(entry.blob);
    return b && !b.binary ? b.data : '';
  }

  isBinary(entry: Entry | null): boolean {
    if (!entry?.blob) return false;
    return this.blobs.get(entry.blob)?.binary ?? false;
  }

  fileEntry(path: RelPath, hash: BlobHash, mtimeNs: number, readonly = false): Entry {
    return {
      path,
      kind: { kind: 'file' },
      blob: hash,
      size: this.blobs.get(hash)?.size ?? 0,
      mtimeNs,
      readonly,
    };
  }

  record(id: ProjectId): ProjectRecord | undefined {
    return this.projects.get(id);
  }

  findVersion(id: number): { rec: ProjectRecord; vr: VersionRecord; index: number } | undefined {
    for (const rec of this.projects.values()) {
      const index = rec.versions.findIndex((v) => v.version.id === id);
      const vr = rec.versions[index];
      if (vr) return { rec, vr, index };
    }
    return undefined;
  }

  /** Whether a path is ignored by the project's current rules. */
  ignored(rec: ProjectRecord, path: RelPath): boolean {
    return isIgnored(path, rec.project.settings.extraIgnore);
  }

  /**
   * Splits the project folder into what a snapshot would capture (`kept`) and
   * what it would not: ignored paths and files over the size cap.
   */
  capturable(rec: ProjectRecord): { kept: Tree; tooLarge: Entry[]; ignored: Entry[] } {
    const kept: Tree = new Map();
    const tooLarge: Entry[] = [];
    const ignored: Entry[] = [];
    for (const [p, e] of rec.tree) {
      if (this.ignored(rec, p)) ignored.push(e);
      else if (e.kind.kind === 'file' && e.size > this.global.sizeCapBytes) tooLarge.push(e);
      else kept.set(p, e);
    }
    return { kept, tooLarge, ignored };
  }

  /** Recomputes every version's counts against the version before it. */
  recount(rec: ProjectRecord): void {
    let prev: Tree | null = null;
    for (const vr of rec.versions) {
      vr.version.counts = countChanges(compareTrees(this, prev, vr.entries, false));
      prev = vr.entries;
    }
  }
}

/** Same file content (or same symlink target). */
export function sameContent(a: Entry, b: Entry): boolean {
  if (a.kind.kind !== b.kind.kind) return false;
  if (a.kind.kind === 'symlink' && b.kind.kind === 'symlink') {
    return a.kind.target === b.kind.target;
  }
  return a.blob === b.blob;
}

/** Changed files between two trees (directories excluded), sorted by path. */
export function compareTrees(
  state: MockState,
  oldTree: Tree | null,
  newTree: Tree,
  withLines: boolean,
): FileChange[] {
  const older: Tree = oldTree ?? new Map();
  const added: Entry[] = [];
  const deleted: Entry[] = [];
  const out: FileChange[] = [];

  for (const [path, n] of newTree) {
    if (n.kind.kind === 'dir') continue;
    const o = older.get(path);
    if (!o || o.kind.kind === 'dir') added.push(n);
    else if (!sameContent(o, n))
      out.push(change(state, path, { kind: 'modified' }, o, n, withLines));
  }
  for (const [path, o] of older) {
    if (o.kind.kind === 'dir') continue;
    const n = newTree.get(path);
    if (!n || n.kind.kind === 'dir') deleted.push(o);
  }

  // Rename = a deleted and an added file with the same content hash.
  for (const a of added) {
    const i = a.blob ? deleted.findIndex((d) => d.blob === a.blob) : -1;
    const d = i >= 0 ? deleted.splice(i, 1)[0] : undefined;
    if (d) out.push(change(state, a.path, { kind: 'renamed', from: d.path }, d, a, withLines));
    else out.push(change(state, a.path, { kind: 'added' }, null, a, withLines));
  }
  for (const d of deleted) out.push(change(state, d.path, { kind: 'deleted' }, d, null, withLines));

  return out.sort((x, y) => (x.path < y.path ? -1 : x.path > y.path ? 1 : 0));
}

function change(
  state: MockState,
  path: RelPath,
  status: FileChange['status'],
  old: Entry | null,
  nu: Entry | null,
  withLines: boolean,
): FileChange {
  let linesAdded: number | null = null;
  let linesRemoved: number | null = null;
  const textual =
    !state.isBinary(old) &&
    !state.isBinary(nu) &&
    imageMime(path) === null &&
    (old?.size ?? 0) <= DIFF_SIZE_LIMIT &&
    (nu?.size ?? 0) <= DIFF_SIZE_LIMIT;
  if (withLines && textual) {
    const d = lineDiff(state.entryText(old), state.entryText(nu), DEFAULT_DIFF_OPTIONS);
    linesAdded = d.added;
    linesRemoved = d.removed;
  }
  return { path, status, old, new: nu, linesAdded, linesRemoved };
}

export function countChanges(changes: FileChange[]): ChangeCounts {
  const counts: ChangeCounts = { added: 0, modified: 0, deleted: 0 };
  for (const c of changes) {
    if (c.status.kind === 'added') counts.added++;
    else if (c.status.kind === 'deleted') counts.deleted++;
    else counts.modified++; // modified and renamed
  }
  return counts;
}
