import { defineConfig, devices } from "@playwright/test";

const CI = Boolean(process.env.CI);

export default defineConfig({
  // Resolved relative to this file, i.e. apps/web/e2e.
  testDir: "./e2e",
  timeout: 180_000,
  retries: CI ? 2 : 0,
  workers: 1,
  webServer: {
    // Serves the prebuilt UI (CI and `test:e2e` build first); preview
    // proxies /api to the E2E backend on :18091 (see preview.proxy).
    command: "npm run preview -- --host 127.0.0.1 --port 5193",
    url: "http://127.0.0.1:5193",
    reuseExistingServer: !CI,
    timeout: 180_000,
  },
  reporter: CI
    ? [
        ["github"],
        ["junit", { outputFile: "test-results/junit.xml" }],
      ]
    : "list",
  use: {
    baseURL: "http://127.0.0.1:5193",
    trace: CI ? "on-first-retry" : "off",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
