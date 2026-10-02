// Syntax highlighting for the diff viewer (DSNA-45, F15) with highlight.js, bundled locally
// (the app is offline). Grammars load on first use. Only hunk lines are available, so each
// hunk side is highlighted as one block (multi-line tokens inside a hunk stay correct; one
// that starts above the hunk does not).
import type { HLJSApi, LanguageFn } from 'highlight.js';
import type { Hunk } from '../api';

type Loader = () => Promise<{ default: LanguageFn }>;

/** Grammar per highlight.js language name. */
const GRAMMARS: Record<string, Loader> = {
  bash: () => import('highlight.js/lib/languages/bash'),
  c: () => import('highlight.js/lib/languages/c'),
  cpp: () => import('highlight.js/lib/languages/cpp'),
  csharp: () => import('highlight.js/lib/languages/csharp'),
  css: () => import('highlight.js/lib/languages/css'),
  go: () => import('highlight.js/lib/languages/go'),
  ini: () => import('highlight.js/lib/languages/ini'),
  java: () => import('highlight.js/lib/languages/java'),
  javascript: () => import('highlight.js/lib/languages/javascript'),
  json: () => import('highlight.js/lib/languages/json'),
  kotlin: () => import('highlight.js/lib/languages/kotlin'),
  markdown: () => import('highlight.js/lib/languages/markdown'),
  php: () => import('highlight.js/lib/languages/php'),
  powershell: () => import('highlight.js/lib/languages/powershell'),
  python: () => import('highlight.js/lib/languages/python'),
  ruby: () => import('highlight.js/lib/languages/ruby'),
  rust: () => import('highlight.js/lib/languages/rust'),
  scss: () => import('highlight.js/lib/languages/scss'),
  sql: () => import('highlight.js/lib/languages/sql'),
  swift: () => import('highlight.js/lib/languages/swift'),
  typescript: () => import('highlight.js/lib/languages/typescript'),
  xml: () => import('highlight.js/lib/languages/xml'),
  yaml: () => import('highlight.js/lib/languages/yaml'),
};

const BY_EXTENSION: Record<string, string> = {
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  c: 'c',
  h: 'c',
  cc: 'cpp',
  cpp: 'cpp',
  cxx: 'cpp',
  hpp: 'cpp',
  cs: 'csharp',
  css: 'css',
  go: 'go',
  ini: 'ini',
  toml: 'ini',
  cfg: 'ini',
  java: 'java',
  js: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  jsx: 'javascript',
  json: 'json',
  kt: 'kotlin',
  kts: 'kotlin',
  md: 'markdown',
  markdown: 'markdown',
  php: 'php',
  ps1: 'powershell',
  psm1: 'powershell',
  py: 'python',
  rb: 'ruby',
  rs: 'rust',
  scss: 'scss',
  sql: 'sql',
  swift: 'swift',
  ts: 'typescript',
  tsx: 'typescript',
  mts: 'typescript',
  cts: 'typescript',
  html: 'xml',
  htm: 'xml',
  xml: 'xml',
  svg: 'xml',
  svelte: 'xml',
  vue: 'xml',
  yml: 'yaml',
  yaml: 'yaml',
};

/** highlight.js language for a path, or `null` for plain text. */
export function languageOf(path: string): string | null {
  const name = path.slice(path.lastIndexOf('/') + 1).toLowerCase();
  if (name === 'dockerfile' || name === 'makefile') return null;
  const dot = name.lastIndexOf('.');
  if (dot < 0) return null;
  return BY_EXTENSION[name.slice(dot + 1)] ?? null;
}

/** Diffs larger than this are not highlighted (it would cost more than it helps). */
export const MAX_HIGHLIGHT_LINES = 20_000;
/** Lines longer than this (minified files) turn highlighting off. */
export const MAX_HIGHLIGHT_LINE_CHARS = 2_000;

let core: Promise<HLJSApi> | null = null;
const loaded = new Set<string>();

async function engine(language: string): Promise<HLJSApi | null> {
  const loader = GRAMMARS[language];
  if (!loader) return null;
  core ??= import('highlight.js/lib/core').then((m) => m.default);
  const hljs = await core;
  if (!loaded.has(language)) {
    hljs.registerLanguage(language, (await loader()).default);
    loaded.add(language);
  }
  return hljs;
}

/**
 * Split highlight.js HTML into one HTML string per line, closing spans that are open at a
 * line end and reopening them on the next line.
 */
export function splitHtmlLines(html: string): string[] {
  const lines: string[] = [];
  const open: string[] = [];
  let cur = '';
  const re = /<span[^>]*>|<\/span>|\n|[^<\n]+/g;
  for (const m of html.matchAll(re)) {
    const t = m[0];
    if (t === '\n') {
      lines.push(cur + '</span>'.repeat(open.length));
      cur = open.join('');
    } else if (t === '</span>') {
      open.pop();
      cur += t;
    } else if (t.startsWith('<span')) {
      open.push(t);
      cur += t;
    } else {
      cur += t;
    }
  }
  lines.push(cur + '</span>'.repeat(open.length));
  return lines;
}

/** Highlighted HTML per line number, for each side. */
export interface Highlighted {
  old: Map<number, string>;
  new: Map<number, string>;
}

/**
 * Highlight the hunks' lines of `path`. `null` when the language is unknown or the diff is
 * too large; the viewer then shows plain text.
 */
export async function highlightHunks(path: string, hunks: Hunk[]): Promise<Highlighted | null> {
  const language = languageOf(path);
  if (!language) return null;
  let total = 0;
  for (const h of hunks) {
    total += h.lines.length;
    if (h.lines.some((l) => l.text.length > MAX_HIGHLIGHT_LINE_CHARS)) return null;
  }
  if (total > MAX_HIGHLIGHT_LINES) return null;
  const hljs = await engine(language);
  if (!hljs) return null;

  const out: Highlighted = { old: new Map(), new: new Map() };
  const side = (nos: number[], texts: string[], into: Map<number, string>) => {
    if (texts.length === 0) return;
    const html = hljs.highlight(texts.join('\n'), { language, ignoreIllegals: true }).value;
    splitHtmlLines(html).forEach((line, i) => into.set(nos[i]!, line));
  };
  for (const h of hunks) {
    const oldNos: number[] = [];
    const oldTexts: string[] = [];
    const newNos: number[] = [];
    const newTexts: string[] = [];
    for (const l of h.lines) {
      if (l.tag !== 'insert' && l.oldNo !== null) {
        oldNos.push(l.oldNo);
        oldTexts.push(l.text);
      }
      if (l.tag !== 'delete' && l.newNo !== null) {
        newNos.push(l.newNo);
        newTexts.push(l.text);
      }
    }
    side(oldNos, oldTexts, out.old);
    side(newNos, newTexts, out.new);
  }
  return out;
}

/** Escape text for `{@html}` (plain, unhighlighted lines). */
export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}
