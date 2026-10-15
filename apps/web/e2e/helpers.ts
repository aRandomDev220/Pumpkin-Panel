import { execFile, spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const E2E_HTTP_PORT = 18091;
export const E2E_BASE_URL = `http://127.0.0.1:${E2E_HTTP_PORT}`;

// apps/web/e2e -> repo root.
const REPO_ROOT = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
);

export interface BackendHandle {
  proc: ChildProcess;
  baseUrl: string;
  /** mkdtemp scratch root holding dataDir, backups and the sqlite file. */
  scratchDir: string;
  dataDir: string;
  dbFile: string;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Spawn `cargo run -p pumpkin-manager-backend` on :18091 with temp dirs + sqlite file. */
export async function startBackend(): Promise<BackendHandle> {
  const scratch = mkdtempSync(join(tmpdir(), "pm-e2e-"));
  const dataDir = join(scratch, "servers");
  const backupDir = join(scratch, "backups");
  const dbFile = join(scratch, "e2e.db");
  const proc = spawn("cargo", ["run", "-p", "pumpkin-manager-backend"], {
    cwd: REPO_ROOT,
    // New process group so stopBackend can SIGTERM/SIGKILL the cargo
    // wrapper *and* the backend binary it spawns (killing cargo alone
    // orphans the server and wedges the port on re-runs).
    detached: true,
    env: {
      ...process.env,
      PUMPKIN_MANAGER_HTTP_PORT: String(E2E_HTTP_PORT),
      PUMPKIN_MANAGER_DATA_DIR: dataDir,
      PUMPKIN_MANAGER_BACKUP_DIR: backupDir,
      PUMPKIN_MANAGER_DATABASE_URL: `sqlite:${dbFile}?mode=rwc`,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let earlyExit = "";
  proc.stderr?.on("data", (chunk: Buffer) => {
    earlyExit += chunk.toString();
  });
  const deadline = Date.now() + 180_000;
  let lastErr = "no attempt yet";
  for (;;) {
    if (proc.exitCode !== null) {
      await removeScratchDir(scratch);
      throw new Error(
        `backend exited early (code ${proc.exitCode}): ${earlyExit.slice(-2000)}`,
      );
    }
    try {
      const res = await fetch(`${E2E_BASE_URL}/api/v1/health/live`);
      if (res.ok) break;
      lastErr = `live probe status ${res.status}`;
    } catch (err) {
      lastErr = err instanceof Error ? err.message : String(err);
    }
    if (Date.now() > deadline) {
      killGroup(proc);
      await removeScratchDir(scratch);
      throw new Error(
        `backend never became live on ${E2E_BASE_URL}: ${lastErr}\nstderr tail: ${earlyExit.slice(-2000)}`,
      );
    }
    await sleep(1000);
  }
  return { proc, baseUrl: E2E_BASE_URL, scratchDir: scratch, dataDir, dbFile };
}

function killGroup(proc: ChildProcess, signal: NodeJS.Signals = "SIGTERM"): void {
  if (proc.pid === undefined) return;
  try {
    process.kill(-proc.pid, signal);
  } catch {
    /* already gone */
  }
}

export async function stopBackend(handle: BackendHandle): Promise<void> {
  killGroup(handle.proc, "SIGTERM");
  const deadline = Date.now() + 10_000;
  while (handle.proc.exitCode === null && handle.proc.signalCode === null) {
    if (Date.now() > deadline) break;
    await sleep(250);
  }
  if (handle.proc.exitCode === null && handle.proc.signalCode === null) {
    killGroup(handle.proc, "SIGKILL");
  }
}

/**
 * Best-effort removal of the mkdtemp scratch dir. Never throws.
 * Files Pumpkin wrote are owned by UID 2613 (see chownLeaf), which the
 * manager user cannot unlink — so wipe the contents as root inside a
 * disposable alpine container first, then remove the (now empty) dir.
 */
export async function removeScratchDir(dir: string): Promise<void> {
  await new Promise<void>((resolve) => {
    execFile(
      "docker",
      ["run", "--rm", "-v", `${dir}:/s`, "alpine", "rm", "-rf", "/s"],
      { timeout: 60_000 },
      () => resolve(),
    );
  });
  try {
    await rm(dir, { recursive: true, force: true });
  } catch {
    /* best-effort teardown */
  }
}

/**
 * Best-effort `docker rm -f` of a test container. Never throws; returns a
 * short outcome line for teardown logging.
 */
export function removeContainer(name: string): Promise<string> {
  return new Promise((resolve) => {
    execFile(
      "docker",
      ["rm", "-f", name],
      { timeout: 30_000 },
      (err, stdout, stderr) => {
        if (err) {
          const detail = (stderr || err.message).trim().split("\n").pop() ?? "";
          resolve(`removeContainer(${name}): skipped (${detail})`);
        } else {
          resolve(`removeContainer(${name}): ${(stdout || "").trim() || "removed"}`);
        }
      },
    );
  });
}

/** Minimal JSON API client against the backend. `token` adds a Bearer header. */
export async function api<T>(
  baseUrl: string,
  method: string,
  path: string,
  body?: unknown,
  token?: string,
): Promise<{ status: number; json: T }> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (token) headers["Authorization"] = `Bearer ${token}`;
  const res = await fetch(`${baseUrl}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json: unknown = null;
  try {
    json = text === "" ? null : (JSON.parse(text) as unknown);
  } catch {
    json = text;
  }
  return { status: res.status, json: json as T };
}

export interface TaskView {
  id: string;
  kind: string;
  state: string;
  error?: string | null;
}

/** Poll `GET /api/v1/tasks/:id` until `state === "succeeded"`. */
export async function waitForTaskOk(
  baseUrl: string,
  taskId: string,
  token: string,
  timeoutMs = 120_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let last = "unknown";
  for (;;) {
    const { status, json } = await api<TaskView>(
      baseUrl,
      "GET",
      `/api/v1/tasks/${encodeURIComponent(taskId)}`,
      undefined,
      token,
    );
    if (status !== 200) {
      throw new Error(`task poll for ${taskId} failed: HTTP ${status}`);
    }
    last = json.state;
    if (json.state === "succeeded") return;
    if (json.state === "failed" || json.state === "cancelled") {
      throw new Error(
        `task ${taskId} ended with state "${json.state}": ${json.error ?? "no detail"}`,
      );
    }
    if (Date.now() > deadline) {
      throw new Error(
        `task ${taskId} not succeeded after ${timeoutMs}ms (last state "${last}")`,
      );
    }
    await sleep(2000);
  }
}

export interface StatusView {
  readiness: { state: string; reason: string } | null;
  desired_state: string;
  observed_state: string;
}

/** Poll `GET /api/v1/servers/:id/status` until `readiness.state === want`. */
export async function waitForReadiness(
  baseUrl: string,
  serverId: string,
  token: string,
  want: string,
  timeoutMs = 60_000,
): Promise<StatusView> {
  const deadline = Date.now() + timeoutMs;
  let last: StatusView | null = null;
  for (;;) {
    const { status, json } = await api<StatusView>(
      baseUrl,
      "GET",
      `/api/v1/servers/${encodeURIComponent(serverId)}/status`,
      undefined,
      token,
    );
    if (status !== 200) {
      throw new Error(`status poll for ${serverId} failed: HTTP ${status}`);
    }
    last = json;
    if (json.readiness?.state === want) return json;
    if (Date.now() > deadline) {
      throw new Error(
        `readiness never became "${want}" after ${timeoutMs}ms (last ${JSON.stringify(last?.readiness ?? null)})`,
      );
    }
    await sleep(2000);
  }
}

/**
 * Chown the leaf dir only via a disposable alpine container.
 * `dir` is the full leaf path; its parent is bind-mounted, never chowned
 * itself (the manager must keep write access to the servers root).
 */
export function chownLeaf(dir: string): Promise<void> {
  const root = dirname(dir);
  const leaf = basename(dir);
  return new Promise((resolve, reject) => {
    execFile(
      "docker",
      [
        "run",
        "--rm",
        "-v",
        `${root}:/s`,
        "alpine",
        "chown",
        "-R",
        "2613:2613",
        `/s/${leaf}`,
      ],
      (err, _stdout, stderr) => {
        if (err) {
          reject(new Error(`chownLeaf(${dir}) failed: ${stderr || err.message}`));
        } else {
          resolve();
        }
      },
    );
  });
}

/** Best-effort `docker logs` for failure output. Never throws. */
export function dumpContainerLogs(container: string): Promise<string> {
  return new Promise((resolve) => {
    execFile(
      "docker",
      ["logs", "--tail", "100", container],
      { timeout: 15_000 },
      (err, stdout, stderr) => {
        if (err) resolve(`(docker logs ${container} failed: ${err.message})`);
        else resolve(stdout || stderr || "(empty container logs)");
      },
    );
  });
}
