// Entry point for the frontend API. UI code imports `api` from here and nothing else.
import type { Api } from './api';
import { createMockApi } from './mock';
import { TauriApi } from './real';

export * from './api';
export * from './types';
export { COMMANDS, EVENTS } from './commands';
export { MockApi, createMockApi, type MockOptions } from './mock';
export { TauriApi } from './real';

/** Which implementation `VITE_DSNAP_API` selects. */
export type ApiKind = 'mock' | 'tauri';

/** Whether the page runs inside the Tauri app (not a plain browser or a test). */
export function inTauri(): boolean {
  return typeof globalThis === 'object' && '__TAURI_INTERNALS__' in globalThis;
}

/**
 * Default implementation: `tauri` inside the app (also `tauri dev`) and in production
 * builds, so a release never ships fake data; `mock` in a plain browser dev server and in
 * tests. `VITE_DSNAP_API=mock` forces the mock for UI work.
 */
export function defaultApiKind(
  dev: boolean = import.meta.env.DEV,
  tauri: boolean = inTauri(),
): ApiKind {
  return dev && !tauri ? 'mock' : 'tauri';
}

/** Picks the implementation for `VITE_DSNAP_API` (default from `defaultApiKind`). */
export function selectApi(kind: string | undefined = import.meta.env.VITE_DSNAP_API): Api {
  switch (kind || defaultApiKind()) {
    case 'mock':
      return createMockApi({ latencyMs: import.meta.env.MODE === 'test' ? 0 : 150 });
    case 'tauri':
      return new TauriApi();
    default:
      throw new Error(`Unknown VITE_DSNAP_API value "${kind}". Use "mock" or "tauri".`);
  }
}

/** The app-wide Api instance. */
export const api: Api = selectApi();
