import { defineConfig, devices } from '@playwright/test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));

// E2E runs against the real server binary with a throwaway data directory.
// Build it first: `npm run build` (UI) then `cargo build -p server`.
const exe = process.platform === 'win32' ? 'vyrtel.exe' : 'vyrtel';
const bin = process.env.VYRTEL_BIN ?? path.resolve(here, '..', 'target', 'debug', exe);
const port = Number(process.env.VYRTEL_E2E_PORT ?? 18765);
const dataDir = process.env.VYRTEL_E2E_DATA ?? fs.mkdtempSync(path.join(os.tmpdir(), 'vyrtel-e2e-'));
// Workers load this file again; share the directory so specs can reach it.
process.env.VYRTEL_E2E_DATA = dataDir;
/** Admin password the server starts with; tests/e2e/global-setup.ts signs in with it. */
export const E2E_PASSWORD = 'e2e admin';
export const STORAGE_STATE = path.join(dataDir, '..', `${path.basename(dataDir)}-session.json`);

export default defineConfig({
  testDir: 'tests/e2e',
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  globalSetup: './tests/e2e/global-setup.ts',
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    storageState: STORAGE_STATE,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: `"${bin}" --data-dir "${dataDir}" --bind 127.0.0.1:${port} --log-level warn`,
    // The specs ingest without a key (the default) and read with the admin session.
    env: { VYRTEL_AUTH_ADMIN_PASSWORD: E2E_PASSWORD },
    url: `http://127.0.0.1:${port}/health`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
