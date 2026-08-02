import { defineConfig, devices } from '@playwright/test';

const BASE_URL = 'http://127.0.0.1:2283';

// 実 illumia-server がビルド済み SPA を配信する構成で E2E を回す。
// 目的は本番配信経路の CSP 回帰検出なので、dev server (vite) は使わない。
export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: process.env.CI ? [['github'], ['list']] : [['list']],
  globalSetup: './e2e/global-setup.ts',
  use: {
    baseURL: BASE_URL,
    storageState: 'e2e/.auth/state.json',
    trace: 'retain-on-failure'
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'node e2e/start-server.mjs',
    url: `${BASE_URL}/api/server/info`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000
  }
});
