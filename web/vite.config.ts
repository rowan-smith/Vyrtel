import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// During `npm run dev`, API calls are proxied to a running Observer server.
const target = process.env.OBSERVER_URL ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': target,
      '/v1': target,
      '/health': target,
      '/ready': target,
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: false,
  },
  test: {
    environment: 'jsdom',
    include: ['tests/unit/**/*.test.{ts,tsx}'],
    setupFiles: ['tests/unit/setup.ts'],
    restoreMocks: true,
  },
});
