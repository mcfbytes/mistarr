import { defineConfig, devices } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { dirname } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  testDir: here,
  outputDir: `${here}/out/artifacts`,
  fullyParallel: false,
  reporter: [['list']],
  use: {
    baseURL: 'http://localhost:4173',
    launchOptions: {
      executablePath: '/opt/pw-browsers/chromium'
    }
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'npm run build && npm run preview',
    cwd: `${here}/..`,
    port: 4173,
    reuseExistingServer: false,
    env: { VITE_MOCK: '1' },
    timeout: 120_000
  }
});
