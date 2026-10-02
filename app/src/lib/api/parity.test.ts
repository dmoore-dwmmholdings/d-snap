// Type parity with the Rust side (DSNA-67). `generated/samples.json` holds a serialized
// sample of every type and enum variant that crosses the Tauri boundary; the Rust test
// `crates/dsnap-bridge/tests/samples.rs` keeps it current. Here every sample's keys must
// match the TypeScript type, and the key lists below must name every key of their type
// (the compiler rejects a missing one).
import { describe, expect, it } from 'vitest';
import type { ApiErrorCode } from './types';
import type * as T from './types';
import samples from './generated/samples.json';

/** `K` if it names every key of `O`, else a type error naming the missing ones. */
type Every<O, K extends readonly PropertyKey[]> = [Exclude<keyof O, K[number]>] extends [never]
  ? K
  : { missing: Exclude<keyof O, K[number]> };

/** Exhaustive key list of an object type. */
const keys =
  <O>() =>
  <const K extends readonly (keyof O)[]>(k: K & Every<O, K>): readonly string[] =>
    k as readonly string[];

/** Exhaustive list of a string union. */
const values =
  <U extends string>() =>
  <const K extends readonly U[]>(
    k: K & ([Exclude<U, K[number]>] extends [never] ? unknown : { missing: Exclude<U, K[number]> }),
  ): readonly string[] =>
    k;

type Variant<U, K> = Extract<U, { kind: K }>;

/** Keys per variant of a `kind`-tagged union; every variant must be listed. */
const tagged =
  <U extends { kind: string }>() =>
  (v: { [K in U['kind']]: readonly (keyof Variant<U, K>)[] }): Record<string, readonly string[]> =>
    v as Record<string, readonly string[]>;

type Shape =
  | { object: readonly string[] }
  | { tagged: Record<string, readonly string[]> }
  | { strings: readonly string[] };

const SHAPES: Record<string, Shape> = {
  Entry: { object: keys<T.Entry>()(['path', 'kind', 'blob', 'size', 'mtimeNs', 'readonly']) },
  EntryKind: {
    tagged: tagged<T.EntryKind>()({ file: ['kind'], symlink: ['kind', 'target'], dir: ['kind'] }),
  },
  VersionKind: { strings: values<T.VersionKind>()(['manual', 'auto', 'cli', 'safety']) },
  Version: {
    object: keys<T.Version>()([
      'id',
      'projectId',
      'label',
      'createdAtMs',
      'kind',
      'pinned',
      'unstable',
      'counts',
    ]),
  },
  ChangeCounts: { object: keys<T.ChangeCounts>()(['added', 'modified', 'deleted']) },
  AutoSnapshot: {
    tagged: tagged<T.AutoSnapshot>()({
      off: ['kind'],
      every: ['kind', 'secs'],
      afterIdle: ['kind', 'secs'],
    }),
  },
  ProjectSettings: {
    object: keys<T.ProjectSettings>()(['extraIgnore', 'respectGitignore', 'autoSnapshot']),
  },
  GlobalSettings: { object: keys<T.GlobalSettings>()(['sizeCapBytes', 'retentionKeep']) },
  Project: { object: keys<T.Project>()(['id', 'name', 'root', 'settings', 'missing']) },
  ChangeStatus: {
    tagged: tagged<T.ChangeStatus>()({
      added: ['kind'],
      modified: ['kind'],
      deleted: ['kind'],
      renamed: ['kind', 'from'],
    }),
  },
  FileChange: {
    object: keys<T.FileChange>()(['path', 'status', 'old', 'new', 'linesAdded', 'linesRemoved']),
  },
  DiffOptions: { object: keys<T.DiffOptions>()(['ignoreWhitespace', 'context']) },
  LineTag: { strings: values<T.LineTag>()(['equal', 'insert', 'delete']) },
  DiffLine: { object: keys<T.DiffLine>()(['tag', 'oldNo', 'newNo', 'text']) },
  Hunk: { object: keys<T.Hunk>()(['oldStart', 'oldLen', 'newStart', 'newLen', 'lines']) },
  DiffBody: {
    tagged: tagged<T.DiffBody>()({
      text: ['kind', 'hunks'],
      binary: ['kind', 'oldSize', 'newSize', 'oldHash', 'newHash'],
      image: ['kind', 'old', 'new', 'mime'],
      tooLarge: ['kind', 'size'],
    }),
  },
  FileDiff: { object: keys<T.FileDiff>()(['path', 'body']) },
  VersionRef: {
    tagged: tagged<T.VersionRef>()({ version: ['kind', 'id'], workingTree: ['kind'] }),
  },
  SkipReason: {
    tagged: tagged<T.SkipReason>()({
      tooLarge: ['kind', 'size'],
      locked: ['kind'],
      nonUtf8Name: ['kind'],
      unreadable: ['kind', 'msg'],
    }),
  },
  SkippedFile: { object: keys<T.SkippedFile>()(['path', 'reason']) },
  SnapshotReport: {
    object: keys<T.SnapshotReport>()(['version', 'skipped', 'unstablePaths']),
  },
  RestorePlan: {
    object: keys<T.RestorePlan>()(['target', 'write', 'delete', 'createDirs', 'uncaptured']),
  },
  RestoreReport: {
    object: keys<T.RestoreReport>()([
      'safetyVersion',
      'written',
      'deleted',
      'failed',
      'uncaptured',
    ]),
  },
  ProgressOp: { strings: values<T.ProgressOp>()(['snapshot', 'restore']) },
  ProgressPhase: {
    strings: values<T.ProgressPhase>()(['queued', 'walk', 'hash', 'restore', 'done']),
  },
  Progress: {
    object: keys<T.Progress>()(['opId', 'projectId', 'op', 'phase', 'done', 'total', 'path']),
  },
  ProjectChangedEvent: {
    object: keys<T.ProjectChangedEvent>()(['projectId', 'changedCount']),
  },
  VersionsChangedEvent: { object: keys<T.VersionsChangedEvent>()(['projectId']) },
  ApiError: { object: keys<{ code: ApiErrorCode; message: string }>()(['code', 'message']) },
};

const sorted = (a: readonly string[]) => [...a].sort();
const data = samples as Record<string, unknown[]>;

describe('type parity with Rust', () => {
  it('covers the same types', () => {
    expect(sorted(Object.keys(SHAPES))).toEqual(sorted(Object.keys(data)));
  });

  it.each(Object.entries(SHAPES))('%s', (name, shape) => {
    const list = data[name]!;
    if ('object' in shape) {
      for (const s of list) expect(sorted(Object.keys(s as object))).toEqual(sorted(shape.object));
    } else if ('strings' in shape) {
      expect(sorted(list as string[])).toEqual(sorted(shape.strings));
    } else {
      const kinds = list.map((s) => (s as { kind: string }).kind);
      expect(sorted(kinds)).toEqual(sorted(Object.keys(shape.tagged)));
      for (const s of list) {
        const v = shape.tagged[(s as { kind: string }).kind]!;
        expect(sorted(Object.keys(s as object))).toEqual(sorted(v));
      }
    }
  });
});
