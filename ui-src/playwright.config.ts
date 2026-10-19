import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 2,
  timeout: 20_000,
  expect: { timeout: 3_000 },
  use: {
    baseURL: 'http://127.0.0.1:25199',
    browserName: 'chromium',
    // Local runs can reuse an installed Chrome, always with a fresh profile.
    channel: process.env.PLAYWRIGHT_CHANNEL,
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
  },
  webServer: {
    command: 'npm run dev -- --host 127.0.0.1 --port 25199 --strictPort',
    url: 'http://127.0.0.1:25199',
    // Never attach a regression run to somebody else's server or data.
    reuseExistingServer: false,
  },
});
