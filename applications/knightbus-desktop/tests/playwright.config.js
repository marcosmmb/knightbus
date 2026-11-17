import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.', testMatch: '*.spec.js',
  outputDir: '../../../lab/m13/ui-results',
  reporter: 'list',
  use: { launchOptions: process.env.PLAYWRIGHT_BROWSER_PATH ? { executablePath: process.env.PLAYWRIGHT_BROWSER_PATH } : {}, baseURL: 'http://127.0.0.1:4178', viewport: { width: 1120, height: 900 } },
  webServer: { command: 'node server.js', port: 4178, reuseExistingServer: false }
});
