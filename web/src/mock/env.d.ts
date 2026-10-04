interface ImportMetaEnv {
  /** `1` builds the app against `src/mock` instead of the server; only main.ts reads it. */
  readonly VITE_MOCK?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
