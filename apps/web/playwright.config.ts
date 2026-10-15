import { defineConfig, devices } from "@playwright/test";

const CI = Boolean(process.env.CI);

export default defineConfig({
  // Resolved relative to this file, i.e. apps/web/e2e.
  testDir: "./e2e",
  timeout: 180_000,
  retries: CI ? 2 : 0,
  workers: 1,
  use: {
    baseURL: "http://127.0.0.1:5193",
  },
  webServer: {
    // Build once so preview serves the current UI; preview proxies /api to
    // the E2E backend on :18091 (see preview.proxy in vite.config.ts).
    command: "npm run build && npm run preview -- --port 5193",
    url: "http://127.0.0.1:5193",
    reuseExistingServer: !CI,
    timeout: 180_000,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
