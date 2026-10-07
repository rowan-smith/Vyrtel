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

export default defineConfig({
  testDir: 'tests/e2e',
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: `"${bin}" --data-dir "${dataDir}" --bind 127.0.0.1:${port} --log-level warn`,
    url: `http://127.0.0.1:${port}/health`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
