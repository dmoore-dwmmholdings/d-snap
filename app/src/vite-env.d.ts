/// <reference types="svelte" />
/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Selects the Api implementation: `mock` (default) or `tauri`. */
  readonly VITE_DSNAP_API?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
