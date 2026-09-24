import { defineConfig, devices } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { dirname } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const chromiumPath = process.env.PLAYWRIGHT_CHROMIUM_PATH;
const port = process.env.MISTARR_E2E_PORT ?? '18420';

export default defineConfig({
  testDir: here,
  testMatch: /live\.spec\.ts/,
  outputDir: `${here}/out/artifacts`,
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  reporter: [['list']],
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    launchOptions: chromiumPath ? { executablePath: chromiumPath } : {}
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: `bash ${here}/live-server.sh`,
    url: `http://127.0.0.1:${port}/api/v1/system/status`,
    reuseExistingServer: false,
    timeout: 180_000
  }
});
