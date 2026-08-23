import { defineConfig } from 'vite';
import { resolve } from 'node:path';

// The shell is the Tauri entry. It is emitted as `shell.html` so it never
// collides with the wpp-admin `index.html` that the copy step drops at the
// dist root (the iframe loads `/index.html`). Asset filenames are hashed, so
// the shell's `assets/` merges with the admin's without clashing.
export default defineConfig({
  root: resolve(import.meta.dirname, 'shell'),
  base: './',
  build: {
    outDir: resolve(import.meta.dirname, 'dist'),
    emptyOutDir: true, // safe: build:shell runs BEFORE copy-admin.mjs
    rollupOptions: {
      input: resolve(import.meta.dirname, 'shell/shell.html'),
    },
  },
});
