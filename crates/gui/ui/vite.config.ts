import path from 'node:path'

import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react-swc'
// `defineConfig` from `vitest/config` re-exports Vite's, extended with a `test` key — one
// config file serves both `vite build`/`vite dev` and `vitest` rather than a second file.
import { defineConfig } from 'vitest/config'

// Tauri's `devUrl`/`frontendDist` (crates/gui/tauri.conf.json) point at this dev server /
// its build output, per ADR-0008 Decision 4's "ui/ co-located under crates/gui" layout.
// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Tauri waits on `devUrl` (crates/gui/tauri.conf.json → localhost:1420); pin Vite to that
  // exact port and fail loudly rather than silently drifting to 5173, which would leave
  // `cargo tauri dev` hanging on an unreachable dev server.
  server: {
    port: 1420,
    strictPort: true,
  },
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
  },
})
