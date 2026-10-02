// Small display helpers shared by the views.
import type { ChangeStatus, RelPath, VersionKind } from '../api';

/** "just now", "5 min ago", "3 h ago", "2 days ago", or a date for older times. */
export function relativeTime(ms: number, now: number = Date.now()): string {
  const s = Math.round((now - ms) / 1000);
  if (s < 45) return 'just now';
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h} h ago`;
  const d = Math.round(h / 24);
  if (d < 7) return d === 1 ? 'yesterday' : `${d} days ago`;
  return new Date(ms).toLocaleDateString();
}

/** Full local date and time, for tooltips. */
export function absoluteTime(ms: number): string {
  return new Date(ms).toLocaleString();
}

/** `dir/` and `name` of a path, for dimming the directory. */
export function splitPath(path: RelPath): { dir: string; name: string } {
  const i = path.lastIndexOf('/');
  return i < 0 ? { dir: '', name: path } : { dir: path.slice(0, i + 1), name: path.slice(i + 1) };
}

/** One-letter status badge. */
export function statusLetter(s: ChangeStatus): 'A' | 'M' | 'D' | 'R' {
  switch (s.kind) {
    case 'added':
      return 'A';
    case 'modified':
      return 'M';
    case 'deleted':
      return 'D';
    case 'renamed':
      return 'R';
  }
}

/** Short label of how a version was made. */
export const KIND_LABEL: Record<VersionKind, string> = {
  manual: 'Manual snapshot',
  auto: 'Auto snapshot',
  cli: 'Command line (dsnap)',
  safety: 'Safety snapshot before a restore',
};

/** Single-character icon per version kind. */
export const KIND_ICON: Record<VersionKind, string> = {
  manual: '●',
  auto: '⟳',
  cli: '›_',
  safety: '⛨',
};

/** Human-readable byte size. */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
}

/** "1 file" / "3 files". */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}
