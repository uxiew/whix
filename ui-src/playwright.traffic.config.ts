import { defineConfig } from '@playwright/test';
import base from './playwright.migration.config';

export default defineConfig({
  ...base,
  testMatch: 'traffic-debug.spec.ts',
  outputDir: './test-results/traffic',
  webServer: [
    base.webServer as Exclude<typeof base.webServer, unknown[] | undefined>,
    {
      command: 'node e2e/traffic-origin.mjs',
      url: 'http://127.0.0.1:25197/health',
      reuseExistingServer: false,
      gracefulShutdown: { signal: 'SIGTERM', timeout: 3000 },
    },
  ],
});
