// Settings form values (as typed) and their validation (DSNA-47).

export interface Form {
  sizeCapMb: string;
  retentionKeep: string;
  extraIgnore: string;
  respectGitignore: boolean;
  autoKind: 'off' | 'every' | 'afterIdle';
  everyMinutes: string;
  idleSeconds: string;
}

export type Errors = Partial<Record<keyof Form, string>>;

function int(v: string | number, min: number, max: number, what: string): string | undefined {
  const s = String(v).trim();
  if (!/^\d+$/.test(s)) return `${what} must be a whole number.`;
  const n = Number(s);
  if (n < min || n > max) return `${what} must be between ${min} and ${max}.`;
  return undefined;
}

/** Problems with the form, by field; empty when it can be saved. */
export function validate(f: Form): Errors {
  const e: Errors = {};
  const set = (k: keyof Form, msg: string | undefined) => {
    if (msg) e[k] = msg;
  };
  set('sizeCapMb', int(f.sizeCapMb, 1, 102_400, 'The size cap'));
  set('retentionKeep', int(f.retentionKeep, 1, 100_000, 'The number of snapshots'));
  if (f.autoKind === 'every') set('everyMinutes', int(f.everyMinutes, 1, 1440, 'The interval'));
  if (f.autoKind === 'afterIdle')
    set('idleSeconds', int(f.idleSeconds, 5, 86_400, 'The idle time'));
  return e;
}
