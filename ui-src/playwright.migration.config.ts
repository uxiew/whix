import { defineConfig } from '@playwright/test';
import base from './playwright.config';

export default defineConfig({
  ...base,
  testMatch: 'config-migration.spec.ts',
  fullyParallel: false,
  workers: 1,
  outputDir: './test-results/migration',
  use: { ...base.use, baseURL: 'http://127.0.0.1:25198' },
  webServer: {
    command: 'node e2e/proxy-server.mjs',
    url: 'http://127.0.0.1:25198/api/status',
    reuseExistingServer: false,
    gracefulShutdown: { signal: 'SIGTERM', timeout: 3000 },
  },
});
