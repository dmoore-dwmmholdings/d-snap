// Entry point for the frontend API. UI code imports `api` from here and nothing else.
import type { Api } from './api';
import { createMockApi } from './mock';

export * from './api';
export * from './types';
export { COMMANDS, EVENTS } from './commands';
export { MockApi, createMockApi, type MockOptions } from './mock';

/** Which implementation `VITE_DSNAP_API` selects. Only `mock` exists until Chain O (DSNA-18). */
export type ApiKind = 'mock' | 'tauri';

/**
 * Default implementation: `mock` in dev and tests, `tauri` in production builds,
 * so a release never ships fake data by accident.
 */
export function defaultApiKind(dev: boolean = import.meta.env.DEV): ApiKind {
  return dev ? 'mock' : 'tauri';
}

/** Picks the implementation for `VITE_DSNAP_API` (default from `defaultApiKind`). */
export function selectApi(kind: string | undefined = import.meta.env.VITE_DSNAP_API): Api {
  switch (kind || defaultApiKind()) {
    case 'mock':
      return createMockApi({ latencyMs: import.meta.env.MODE === 'test' ? 0 : 150 });
    case 'tauri':
      // Chain O replaces this with the invoke-based implementation.
      throw new Error('VITE_DSNAP_API=tauri is not implemented yet; use mock.');
    default:
      throw new Error(`Unknown VITE_DSNAP_API value "${kind}". Use "mock" or "tauri".`);
  }
}

/** The app-wide Api instance. */
export const api: Api = selectApi();
