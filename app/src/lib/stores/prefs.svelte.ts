// Diff view preferences, kept in localStorage so they survive restarts.

export type DiffMode = 'split' | 'inline';

const KEY = 'dsnap.diffPrefs';

interface Saved {
  mode: DiffMode;
  ignoreWhitespace: boolean;
  context: number;
}

function load(): Saved {
  const fallback: Saved = { mode: 'split', ignoreWhitespace: false, context: 3 };
  try {
    const raw = globalThis.localStorage?.getItem(KEY);
    if (!raw) return fallback;
    const v = JSON.parse(raw) as Partial<Saved>;
    return {
      mode: v.mode === 'inline' ? 'inline' : 'split',
      ignoreWhitespace: v.ignoreWhitespace === true,
      context: typeof v.context === 'number' && v.context >= 0 ? v.context : 3,
    };
  } catch {
    return fallback;
  }
}

class DiffPrefs {
  mode = $state<DiffMode>('split');
  ignoreWhitespace = $state(false);
  context = $state(3);

  constructor() {
    const s = load();
    this.mode = s.mode;
    this.ignoreWhitespace = s.ignoreWhitespace;
    this.context = s.context;
  }

  save(): void {
    try {
      globalThis.localStorage?.setItem(
        KEY,
        JSON.stringify({
          mode: this.mode,
          ignoreWhitespace: this.ignoreWhitespace,
          context: this.context,
        }),
      );
    } catch {
      // Storage may be unavailable; the preference just does not persist.
    }
  }
}

/** Shared diff preferences. */
export const diffPrefs = new DiffPrefs();

$effect.root(() => {
  $effect(() => {
    void [diffPrefs.mode, diffPrefs.ignoreWhitespace, diffPrefs.context];
    diffPrefs.save();
  });
});
