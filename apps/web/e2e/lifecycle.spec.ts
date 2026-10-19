import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import { join } from "node:path";
import {
  E2E_BASE_URL,
  api,
  chownLeaf,
  dumpContainerLogs,
  removeContainer,
  removeScratchDir,
  startBackend,
  stopBackend,
  waitForReadiness,
  waitForTaskOk,
} from "./helpers";
import type { BackendHandle } from "./helpers";

const ADMIN = "e2e-admin";
const PASSWORD = "password12345";
const SERVER_NAME = "e2e-journey";
const GAME_PORT = 26565;
const CONTAINER = "pumpkin-e2e-journey";

let backend: BackendHandle | undefined;
let session = "";
let serverId = "";

async function readSession(page: Page): Promise<string> {
  const cookies = await page.context().cookies(E2E_BASE_URL);
  const sessionCookies = cookies.filter(
    (cookie) => cookie.name === "pm_session" || cookie.name === "pm_csrf",
  );
  expect(sessionCookies.length).toBe(2);
  return sessionCookies.map((cookie) => `${cookie.name}=${cookie.value}`).join("; ");
}

// Re-run `fn`; on failure append the test container's logs so a task or
// readiness timeout carries the server-side reason.
async function withContainerLogs<T>(fn: () => Promise<T>): Promise<T> {
  try {
    return await fn();
  } catch (err) {
    const logs = await dumpContainerLogs(CONTAINER);
    throw new Error(`${err}\n--- docker logs ${CONTAINER} ---\n${logs}`);
  }
}

// Each test gets a fresh browser context, so seed the HttpOnly session and
// readable CSRF cookies before navigating.
async function seedSession(page: Page, value: string): Promise<void> {
  expect(value.length).toBeGreaterThan(0);
  await page.context().addCookies(
    value.split("; ").map((part) => {
      const [name, ...rest] = part.split("=");
      return { name, value: rest.join("="), url: E2E_BASE_URL };
    }),
  );
}

test.describe.serial("full user journey", () => {
  test.beforeAll(async () => {
    backend = await startBackend();
  });

  test.afterAll(async () => {
    // Teardown is best-effort and order-independent of which test failed:
    // force-remove the container (frees the game port even if (f) never
    // ran), stop the backend, then remove the scratch dirs.
    console.log(await removeContainer(CONTAINER));
    if (backend !== undefined) {
      await stopBackend(backend);
      await removeScratchDir(backend.scratchDir);
    }
  });

  test("(a) fresh DB redirects / to /setup; setup creates owner", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page).toHaveURL(/\/setup$/);
    await page.getByLabel("Admin username", { exact: true }).fill(ADMIN);
    await page.getByLabel("Password (min 8 chars)", { exact: true }).fill(PASSWORD);
    await page.getByLabel("Confirm password", { exact: true }).fill(PASSWORD);
    const [setupResp] = await Promise.all([
      page.waitForResponse(
        (r) => r.request().method() === "POST" && r.url().endsWith("/api/v1/auth/setup"),
      ),
      page.getByRole("button", { name: "Create owner account", exact: true }).click(),
    ]);
    expect(setupResp.ok()).toBe(true);
    await expect(page).toHaveURL(/\/servers$/);
    session = await readSession(page);
    const { status, json } = await api<{ needs_setup: boolean }>(
      E2E_BASE_URL,
      "GET",
      "/api/v1/auth/setup-status",
    );
    expect(status).toBe(200);
    expect(json.needs_setup).toBe(false);
  });

  test("(b) no session redirects to login; bad login errors; login works", async ({
    page,
  }) => {
    // No session is seeded in this test's fresh context: the guard must bounce
    // /servers to /login.
    await page.goto("/servers");
    await expect(page).toHaveURL(/\/login$/);

    await page.getByLabel("Username", { exact: true }).fill(ADMIN);
    await page.getByLabel("Password", { exact: true }).fill("wrong-password");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(page.getByText(/invalid username or password/i)).toBeVisible();

    await page.getByLabel("Password", { exact: true }).fill(PASSWORD);
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(page).toHaveURL(/\/servers$/);
    session = await readSession(page);
  });

  test("(c) create server appears in list", async ({ page }) => {
    await seedSession(page, session);
    await page.goto("/servers");
    await expect(
      page.getByRole("heading", { level: 1, name: "Servers" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "New server" }).click();
    const dialog = page.getByRole("dialog", { name: "New server" });
    await expect(dialog).toBeVisible();
    await dialog.getByLabel("Name", { exact: true }).fill(SERVER_NAME);
    await dialog
      .getByLabel("Game port", { exact: true })
      .fill(String(GAME_PORT));
    const [createResp] = await Promise.all([
      page.waitForResponse(
        (r) => r.request().method() === "POST" && r.url().endsWith("/api/v1/servers"),
      ),
      dialog
        .getByRole("button", { name: "Create server", exact: true })
        .click(),
    ]);
    expect(createResp.status()).toBe(201);
    await expect(dialog).toBeHidden();
    await expect(page.getByRole("link", { name: SERVER_NAME })).toBeVisible();
    await expect(
      page.getByRole("searchbox", { name: "Search servers" }),
    ).toBeVisible();

    const { status, json } = await api<{ servers: Array<{ id: string; name: string }> }>(
      E2E_BASE_URL,
      "GET",
      "/api/v1/servers",
      undefined,
      session,
    );
    expect(status).toBe(200);
    const match = json.servers.filter((s) => s.name === SERVER_NAME);
    expect(match.length).toBe(1);
    serverId = match[0].id;

    // Pumpkin runs as UID 2613; the manager-owned data dir must be handed over.
    await chownLeaf(join(backend.dataDir, serverId));
  });

  test("(c2) dashboard shows the fleet summary", async ({ page }) => {
    await seedSession(page, session);
    await page.goto("/");
    await expect(
      page.getByRole("heading", { level: 1, name: "Dashboard" }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Attention" }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Host capacity" }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Your servers" }),
    ).toBeVisible();
    await expect(page.getByRole("link", { name: SERVER_NAME })).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Start", exact: true }),
    ).toBeVisible();
  });

  test("(d) start reaches online", async ({ page }) => {
    await seedSession(page, session);
    await page.goto(`/servers/${serverId}`);
    await expect(
      page.getByRole("heading", { level: 1, name: SERVER_NAME }),
    ).toBeVisible();
    await expect(
      page.getByRole("link", { name: "Console", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("link", { name: "Backups", exact: true }),
    ).toBeVisible();
    const [startResp] = await Promise.all([
      page.waitForResponse(
        (r) =>
          r.request().method() === "POST" &&
          r.url().endsWith(`/api/v1/servers/${serverId}/start`),
      ),
      page.getByRole("button", { name: "Start", exact: true }).click(),
    ]);
    expect(startResp.status()).toBe(202);
    const startBody = (await startResp.json()) as { task_id: string };
    expect(typeof startBody.task_id).toBe("string");
    await withContainerLogs(async () => {
      await waitForTaskOk(E2E_BASE_URL, startBody.task_id, session, 120_000);
      const status = await waitForReadiness(E2E_BASE_URL, serverId, session, "online", 60_000);
      expect(status.readiness?.state).toBe("online");
    });
    await expect(page.getByText(/Start complete/)).toBeVisible({ timeout: 30_000 });
  });

  test("(e) stop reaches stopped", async ({ page }) => {
    await seedSession(page, session);
    await page.goto(`/servers/${serverId}`);
    await page.getByRole("button", { name: "Stop", exact: true }).click();
    const stopDialog = page.getByRole("dialog", {
      name: `Stop ${SERVER_NAME}?`,
    });
    await expect(stopDialog).toBeVisible();
    const [stopResp] = await Promise.all([
      page.waitForResponse(
        (r) =>
          r.request().method() === "POST" &&
          r.url().endsWith(`/api/v1/servers/${serverId}/stop`),
      ),
      stopDialog
        .getByRole("button", { name: "Stop server", exact: true })
        .click(),
    ]);
    expect(stopResp.status()).toBe(202);
    const stopBody = (await stopResp.json()) as { task_id: string };
    expect(typeof stopBody.task_id).toBe("string");
    await withContainerLogs(async () => {
      await waitForTaskOk(E2E_BASE_URL, stopBody.task_id, session, 120_000);
      const status = await waitForReadiness(E2E_BASE_URL, serverId, session, "stopped", 60_000);
      expect(status.readiness?.state).toBe("stopped");
    });
    await expect(page.getByText(/Stop complete/)).toBeVisible({ timeout: 30_000 });
  });

  test("(e2) server workspace tabs route correctly", async ({ page }) => {
    await seedSession(page, session);
    await page.goto(`/servers/${serverId}`);
    await expect(
      page.getByRole("heading", { level: 1, name: SERVER_NAME }),
    ).toBeVisible();
    const tabs = page.getByRole("navigation", {
      name: "Server sections",
    });
    for (const tab of ["Console", "Files", "Config", "Backups", "Settings"]) {
      await expect(
        tabs.getByRole("link", { name: tab, exact: true }),
      ).toBeVisible();
    }

    await tabs.getByRole("link", { name: "Backups", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Backups", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Run full backup", exact: true }),
    ).toBeVisible();

    await tabs.getByRole("link", { name: "Settings", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Update image", exact: true }),
    ).toBeVisible();

    await tabs.getByRole("link", { name: "Console", exact: true }).click();
    await expect(page.getByRole("log")).toBeVisible();
  });

  test("(f) delete with retain removes it from the list", async ({ page }) => {
    await seedSession(page, session);
    await page.goto("/servers");
    const card = page.locator("article.server-card", { hasText: SERVER_NAME });
    await expect(card).toBeVisible();
    await card
      .getByRole("button", { name: `Actions for ${SERVER_NAME}` })
      .click();
    await page.getByRole("menuitem", { name: "Delete server" }).click();
    const dialog = page.getByRole("dialog", { name: "Delete server" });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByLabel("Keep world data")).toBeChecked();
    const [delResp] = await Promise.all([
      page.waitForResponse(
        (r) =>
          r.request().method() === "DELETE" && r.url().includes("/api/v1/servers/"),
      ),
      dialog.getByRole("button", { name: "Delete server" }).click(),
    ]);
    expect(delResp.status()).toBe(204);
    await expect(page.getByRole("link", { name: SERVER_NAME })).toHaveCount(0);
    const { status } = await api<unknown>(
      E2E_BASE_URL,
      "GET",
      `/api/v1/servers/${encodeURIComponent(serverId)}`,
      undefined,
      session,
    );
    expect(status).toBe(404);
  });
});
