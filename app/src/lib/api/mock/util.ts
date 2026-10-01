// Small deterministic helpers for the mock Api.

/** Fake 64-hex content hash (FNV-1a with 8 seeds). Stable, not cryptographic. */
export function fakeHash(input: string): string {
  let out = '';
  for (let seed = 0; seed < 8; seed++) {
    let h = (0x811c9dc5 ^ Math.imul(seed + 1, 0x9e3779b1)) >>> 0;
    for (let i = 0; i < input.length; i++) {
      h ^= input.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    out += h.toString(16).padStart(8, '0');
  }
  return out;
}

/** Seeded PRNG (mulberry32) returning floats in [0, 1). */
export function prng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** UTF-8 byte length of a string. */
export function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** Base64 of raw bytes. */
export function bytesToBase64(bytes: Uint8Array): string {
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

/** Base64 of a string's UTF-8 bytes. */
export function textToBase64(text: string): string {
  return bytesToBase64(new TextEncoder().encode(text));
}

/** MIME type for image paths the mock previews; `null` for anything else. */
export function imageMime(path: string): string | null {
  const ext = path.slice(path.lastIndexOf('.') + 1).toLowerCase();
  const map: Record<string, string> = {
    png: 'image/png',
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    gif: 'image/gif',
    webp: 'image/webp',
    svg: 'image/svg+xml',
  };
  return map[ext] ?? null;
}

/** Local `YYYY-MM-DD HH:mm:ss`, the default snapshot label (F4). */
export function timestampLabel(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
