// Deterministic demo data for the mock Api: 3 projects with ~15 versions each.
import type { ProjectRecord, Tree } from './state';
import { MockState, compareTrees, countChanges } from './state';
import type { ProjectSettings, VersionKind } from '../types';
import { prng, timestampLabel } from './util';

const HOUR = 3_600_000;
/** First seeded version: 2026-09-01 09:00 UTC. */
const SEED_START_MS = Date.UTC(2026, 8, 1, 9, 0, 0);

const defaultSettings = (): ProjectSettings => ({
  extraIgnore: [],
  respectGitignore: true,
  autoSnapshot: { kind: 'off' },
});

class Seeder {
  readonly rand: () => number;
  now = SEED_START_MS;

  constructor(
    readonly state: MockState,
    seed: number,
  ) {
    this.rand = prng(seed);
  }

  project(name: string, root: string, missing = false): ProjectRecord {
    const id = this.state.nextProjectId++;
    const rec: ProjectRecord = {
      project: { id, name, root, settings: defaultSettings(), missing },
      tree: new Map(),
      versions: [],
    };
    this.state.projects.set(id, rec);
    return rec;
  }

  private mtime(): number {
    return this.now * 1_000_000;
  }

  put(rec: ProjectRecord, path: string, text: string, size?: number): void {
    const hash = this.state.putText(text, size);
    rec.tree.set(path, this.state.fileEntry(path, hash, this.mtime()));
  }

  putBin(rec: ProjectRecord, path: string, bytes: Uint8Array): void {
    const hash = this.state.putBinary(bytes);
    rec.tree.set(path, this.state.fileEntry(path, hash, this.mtime()));
  }

  symlink(rec: ProjectRecord, path: string, target: string): void {
    rec.tree.set(path, {
      path,
      kind: { kind: 'symlink', target },
      blob: null,
      size: target.length,
      mtimeNs: this.mtime(),
      readonly: false,
    });
  }

  dir(rec: ProjectRecord, path: string): void {
    rec.tree.set(path, {
      path,
      kind: { kind: 'dir' },
      blob: null,
      size: 0,
      mtimeNs: this.mtime(),
      readonly: false,
    });
  }

  del(rec: ProjectRecord, path: string): void {
    rec.tree.delete(path);
  }

  mv(rec: ProjectRecord, from: string, to: string): void {
    const e = rec.tree.get(from);
    if (!e) return;
    rec.tree.delete(from);
    rec.tree.set(to, { ...e, path: to });
  }

  text(rec: ProjectRecord, path: string): string {
    return this.state.entryText(rec.tree.get(path) ?? null);
  }

  edit(rec: ProjectRecord, path: string, fn: (text: string) => string): void {
    this.put(rec, path, fn(this.text(rec, path)));
  }

  /** Random line edits: replace, insert or delete `count` lines. */
  tweak(rec: ProjectRecord, path: string, count = 2): void {
    this.edit(rec, path, (text) => {
      const lines = text.split('\n');
      for (let i = 0; i < count; i++) {
        const at = Math.floor(this.rand() * Math.max(1, lines.length - 1));
        const r = this.rand();
        const line = `  // edit ${Math.floor(this.rand() * 10_000)}`;
        if (r < 0.5) lines[at] = line;
        else if (r < 0.8) lines.splice(at, 0, line);
        else if (lines.length > 2) lines.splice(at, 1);
      }
      return lines.join('\n');
    });
  }

  snap(
    rec: ProjectRecord,
    kind: VersionKind,
    label: string | null,
    flags: { pinned?: boolean; unstable?: boolean } = {},
  ): void {
    this.now += HOUR + Math.floor(this.rand() * 3 * HOUR);
    const entries: Tree = new Map(this.state.capturable(rec).kept);
    const prev = rec.versions[rec.versions.length - 1]?.entries ?? null;
    const counts = countChanges(compareTrees(this.state, prev, entries, false));
    rec.versions.push({
      version: {
        id: this.state.nextVersionId++,
        projectId: rec.project.id,
        label: label ?? timestampLabel(this.now),
        createdAtMs: this.now,
        kind,
        pinned: flags.pinned ?? false,
        unstable: flags.unstable ?? false,
        counts,
      },
      entries,
    });
  }
}

function tsModule(name: string, fns: number): string {
  const out = [`// ${name}`, `import { log } from './log';`, ''];
  for (let i = 0; i < fns; i++) {
    out.push(
      `export function ${name}${i}(input: number): number {`,
      `  const scaled = input * ${i + 2};`,
      `  log('${name}${i}', scaled);`,
      `  return scaled + ${i};`,
      `}`,
      '',
    );
  }
  return out.join('\n');
}

function rustModule(name: string, fns: number): string {
  const out = [`//! ${name}`, 'use std::io;', ''];
  for (let i = 0; i < fns; i++) {
    out.push(
      `pub fn ${name}_${i}(x: u64) -> io::Result<u64> {`,
      `    let y = x.checked_mul(${i + 3}).unwrap_or(0);`,
      `    Ok(y + ${i})`,
      `}`,
      '',
    );
  }
  return out.join('\n');
}

function largeFile(lines: number, variant: number): string {
  const out: string[] = ['// Generated schema. Do not edit.'];
  for (let i = 1; i < lines; i++) {
    const v = variant > 0 && i % 25 === 0 ? `v${variant}-${i}` : `${i}`;
    out.push(`export const field${i}: string = 'value-${v}';`);
  }
  return out.join('\n') + '\n';
}

function logoSvg(color: string, shape: 'circle' | 'square'): string {
  const body =
    shape === 'circle'
      ? `<circle cx="32" cy="32" r="24" fill="${color}"/>`
      : `<rect x="8" y="8" width="48" height="48" rx="8" fill="${color}"/>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">${body}</svg>\n`;
}

function bytes(rand: () => number, n: number): Uint8Array {
  const b = new Uint8Array(n);
  for (let i = 0; i < n; i++) b[i] = Math.floor(rand() * 256);
  return b;
}

/** Builds the full demo state. Same `seed` gives the same data. */
export function seedState(seed = 42): MockState {
  const state = new MockState();
  const s = new Seeder(state, seed);
  seedWebShop(s);
  seedRustTool(s);
  seedNotes(s);
  return state;
}

/** Project 1: every change type, a 5k-line diff, rename, binary, image, unsaved changes. */
function seedWebShop(s: Seeder): void {
  const p = s.project('web-shop', 'C:\\Users\\demo\\Projects\\web-shop');
  s.put(p, '.gitignore', 'node_modules/\ndist/\n.env\n');
  s.put(p, 'package.json', '{\n  "name": "web-shop",\n  "version": "0.1.0"\n}\n');
  s.put(p, 'README.md', '# Web shop\n\nDemo project.\n');
  s.put(p, 'src/index.ts', tsModule('index', 4));
  s.put(p, 'src/cart.ts', tsModule('cart', 6));
  s.put(p, 'src/util.ts', tsModule('util', 3));
  s.put(p, 'src/styles.css', 'body {\n  margin: 0;\n  font-family: sans-serif;\n}\n');
  s.put(p, 'docs/old-notes.md', '# Notes\n\n- first idea\n- second idea\n');
  s.put(p, 'scripts/build.sh', '#!/bin/sh\r\nset -e\r\nnpm run build\r\n');
  s.put(p, 'assets/logo.svg', logoSvg('#2463eb', 'circle'));
  s.putBin(p, 'bin/model.bin', bytes(s.rand, 512));
  s.dir(p, 'logs/archive');
  s.snap(p, 'manual', 'Initial import'); // 1

  s.tweak(p, 'src/cart.ts', 3);
  s.snap(p, 'cli', 'before agent turn'); // 2

  s.tweak(p, 'src/index.ts', 2);
  s.put(p, 'src/checkout.ts', tsModule('checkout', 5));
  s.snap(p, 'cli', 'after agent turn'); // 3

  s.put(p, 'logs/huge.log', 'simulated 12 MB log\n', 12 * 1024 * 1024);
  s.snap(p, 'auto', null); // 4

  s.tweak(p, 'src/checkout.ts', 4);
  s.edit(p, 'README.md', (t) => t + '\n## Checkout\n\nWorks end to end.\n');
  s.snap(p, 'manual', 'Working checkout', { pinned: true }); // 5

  s.put(p, 'src/generated/schema.ts', largeFile(5000, 0));
  s.snap(p, 'cli', 'after agent turn'); // 6 (5k-line added file)

  s.mv(p, 'src/util.ts', 'src/helpers.ts');
  s.snap(p, 'manual', 'Rename util to helpers'); // 7

  s.putBin(p, 'bin/model.bin', bytes(s.rand, 640));
  s.snap(p, 'auto', null); // 8

  s.put(p, 'src/generated/schema.ts', largeFile(5000, 1));
  s.snap(p, 'cli', 'after agent turn'); // 9 (200 changed lines across 5k)

  s.put(p, 'assets/logo.svg', logoSvg('#cf222e', 'square'));
  s.snap(p, 'manual', 'New logo'); // 10

  s.del(p, 'docs/old-notes.md');
  s.del(p, 'logs/archive');
  s.snap(p, 'cli', 'after agent turn'); // 11

  s.tweak(p, 'src/cart.ts', 2);
  s.snap(p, 'safety', 'Before restore to Working checkout'); // 12

  s.tweak(p, 'src/index.ts', 1);
  s.edit(p, 'src/styles.css', (t) => t.replace('margin: 0;', 'margin: 0;\n  padding: 0;'));
  s.snap(p, 'auto', null, { unstable: true }); // 13

  s.put(p, 'logs/huge.log', 'simulated 12 MB log, rotated\n', 12 * 1024 * 1024);
  s.edit(p, 'scripts/build.sh', (t) => t.replace('npm run build', 'npm ci\r\nnpm run build'));
  s.snap(p, 'cli', 'before agent turn'); // 14

  s.tweak(p, 'src/checkout.ts', 2);
  s.put(p, 'src/payment.ts', tsModule('payment', 3));
  s.snap(p, 'cli', 'after agent turn'); // 15

  // Unsaved changes in the folder.
  s.tweak(p, 'src/cart.ts', 2);
  s.put(p, 'src/config.ts', "export const API_URL = 'http://localhost:3000';\n");
  s.del(p, 'src/payment.ts');
  s.put(p, 'videos/demo.mp4', 'simulated 80 MB video', 80 * 1024 * 1024);
}

/** Project 2: Rust CLI with no unsaved changes, includes a symlink. */
function seedRustTool(s: Seeder): void {
  const p = s.project('rust-tool', 'C:\\Users\\demo\\Projects\\rust-tool');
  s.put(p, 'Cargo.toml', '[package]\nname = "rust-tool"\nversion = "0.1.0"\nedition = "2024"\n');
  s.put(p, 'src/main.rs', 'fn main() {\n    println!("hello");\n}\n');
  s.put(p, 'src/lib.rs', rustModule('core', 4));
  s.put(p, 'src/parse.rs', rustModule('parse', 5));
  s.symlink(p, 'latest', 'target/release/rust-tool');
  s.snap(p, 'manual', 'Initial import');

  const files = ['src/lib.rs', 'src/parse.rs', 'src/main.rs'];
  const kinds: VersionKind[] = ['cli', 'cli', 'auto', 'manual'];
  for (let i = 0; i < 14; i++) {
    const kind = kinds[i % kinds.length] ?? 'cli';
    s.tweak(p, files[i % files.length] ?? 'src/lib.rs', 1 + (i % 3));
    if (i === 4) s.put(p, 'src/fmt.rs', rustModule('fmt', 3));
    if (i === 9) s.del(p, 'src/fmt.rs');
    const label =
      kind === 'cli'
        ? i % 2
          ? 'after agent turn'
          : 'before agent turn'
        : kind === 'manual'
          ? `Step ${i + 2}`
          : null;
    s.snap(p, kind, label, { pinned: i === 7 });
  }
  const last = p.versions[p.versions.length - 1];
  if (last) last.version.kind = 'safety';
  if (last) last.version.label = 'Before restore to Step 9';
}

/** Project 3: notes folder that was moved ("missing"). */
function seedNotes(s: Seeder): void {
  const p = s.project('notes', 'D:\\Archive\\notes', true);
  s.put(p, 'todo.md', '# Todo\n\n- [ ] one\n- [ ] two\n- [ ] three\n');
  s.put(p, 'journal.md', '# Journal\n\nDay 1.\n');
  s.snap(p, 'manual', 'Initial import');
  for (let i = 0; i < 14; i++) {
    s.edit(p, 'journal.md', (t) => `${t}\nDay ${i + 2}: wrote more notes.\n`);
    if (i % 4 === 0) s.tweak(p, 'todo.md', 1);
    const kind: VersionKind = i === 6 ? 'safety' : i % 3 === 0 ? 'manual' : 'auto';
    const label =
      kind === 'safety'
        ? 'Before restore to Initial import'
        : kind === 'manual'
          ? `Notes ${i + 2}`
          : null;
    s.snap(p, kind, label, { unstable: i === 10, pinned: i === 2 });
  }
}
