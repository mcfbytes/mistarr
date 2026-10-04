import { defineConfig, devices } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { dirname } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const chromiumPath = process.env.PLAYWRIGHT_CHROMIUM_PATH;
// E2E_PORT moves the preview server off 4173 when several checkouts run the suite at once.
const port = Number(process.env.E2E_PORT ?? 4173);

export default defineConfig({
  testDir: here,
  testIgnore: /live\.spec\.ts/,
  outputDir: `${here}/out/artifacts`,
  fullyParallel: false,
  reporter: [['list']],
  use: {
    baseURL: `http://localhost:${port}`,
    launchOptions: chromiumPath ? { executablePath: chromiumPath } : {}
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: `npm run build && npx vite preview --port ${port} --strictPort`,
    cwd: `${here}/..`,
    port,
    reuseExistingServer: false,
    env: { VITE_MOCK: '1' },
    timeout: 120_000
  }
});
