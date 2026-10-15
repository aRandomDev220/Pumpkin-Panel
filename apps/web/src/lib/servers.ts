import { authFetch } from "./auth";

export interface ServerSummary {
  id: string;
  name: string;
  game_port: number;
  image_reference: string;
  desired_state: string;
  observed_state: string;
  status?: string;
  created_at?: string;
  updated_at?: string;
}

export interface ServerDetail extends ServerSummary {
  container_id?: string | null;
  last_error?: string | null;
}

export interface TaskStatus {
  id: string;
  kind: string;
  state: string;
  server_id?: string | null;
  error?: string | null;
  created_at?: string;
  updated_at?: string;
}

async function parseJson<T>(res: Response, label: string): Promise<T> {
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as { error?: string; message?: string };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new Error(`${label} failed (${res.status}): ${detail}`);
  }
  return res.json() as Promise<T>;
}

function hasDigest(imageReference: string): boolean {
  return imageReference.includes("@sha256:");
}

export function digestWarning(imageReference: string): string | null {
  const ref = imageReference.trim();
  if (!ref) return null;
  if (hasDigest(ref)) return null;
  return "No digest pinned (missing @sha256:…). Unpinned tags can drift between pulls; pin a digest for reproducible deploys.";
}

export interface CreateServerInput {
  name: string;
  game_port: number;
  image_reference: string;
}

export async function listServers(): Promise<{ servers: ServerSummary[] }> {
  const res = await authFetch("/api/v1/servers");
  return parseJson<{ servers: ServerSummary[] }>(res, "list servers");
}

export async function getServer(id: string): Promise<ServerDetail> {
  const res = await authFetch(`/api/v1/servers/${encodeURIComponent(id)}`);
  return parseJson<ServerDetail>(res, "get server");
}

export async function createServer(input: CreateServerInput): Promise<ServerDetail> {
  const res = await authFetch("/api/v1/servers", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  return parseJson<ServerDetail>(res, "create server");
}

export async function deleteServer(
  id: string,
  retainData: boolean,
): Promise<void> {
  const qs = retainData ? "?retain_data=true" : "?retain_data=false";
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(id)}${qs}`,
    { method: "DELETE" },
  );
  if (!res.ok && res.status !== 204) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as { error?: string; message?: string };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* ignore */
    }
    throw new Error(`delete server failed (${res.status}): ${detail}`);
  }
}

export async function fetchTask(id: string): Promise<TaskStatus> {
  const res = await authFetch(`/api/v1/tasks/${encodeURIComponent(id)}`);
  return parseJson<TaskStatus>(res, "get task");
}

export function validateCreateServer(input: CreateServerInput): string | null {
  if (!input.name.trim()) return "Name is required.";
  if (!/^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(input.name.trim())) {
    return "Name must start alphanumeric and contain only letters, numbers, - and _ .";
  }
  if (!Number.isInteger(input.game_port) || input.game_port < 1 || input.game_port > 65535) {
    return "game_port must be an integer 1–65535.";
  }
  if (!input.image_reference.trim()) return "image_reference is required.";
  return null;
}

// ---------------------------------------------------------------------------
// Phase 3: dashboard + lifecycle contracts.
// Key names below match the frozen backend contracts exactly. `readiness` is
// optional: older backends omit it, in which case callers fall back to
// desired/observed state.
// ---------------------------------------------------------------------------

export type ReadinessState =
  | "online"
  | "starting"
  | "stopped"
  | "unhealthy"
  | "unknown";

export interface ServerStatusPing {
  players_online: number;
  players_max: number;
  version: string;
  motd: string;
  latency_ms: number;
}

export interface ReadinessInfo {
  state: ReadinessState;
  reason: string;
  ping: ServerStatusPing | null;
}

export interface ServerContainerView {
  container_id?: string | null;
  status?: string | null;
  exit_code?: number | null;
  health?: string | null;
}

export interface ServerStatus {
  id: string;
  desired_state: string;
  observed_state: string;
  readiness?: ReadinessInfo | null;
  docker_available: boolean;
  container: ServerContainerView | null;
}

export interface ServerTask {
  id: string;
  kind: string;
  type: string;
  state: string;
  server_id: string | null;
  progress_percent: number | null;
  message: string | null;
  error: string | null;
  error_code: string | null;
}

export interface ServerTasksResponse {
  tasks: ServerTask[];
  total: number;
}

export interface HostStats {
  cpu_percent: number;
  memory_total_bytes: number;
  memory_used_bytes: number;
  disk_total_bytes: number;
  disk_free_bytes: number;
}

export interface ServerMetric {
  server_id: string;
  name: string;
  cpu_percent: number | null;
  memory_bytes: number | null;
  memory_limit: number | null;
  state: string;
}

export interface MetricsSummary {
  host: HostStats;
  servers: ServerMetric[];
}

export interface ServerLogsResponse {
  logs: string[];
}

export type LifecycleOp = "start" | "stop" | "restart";

export interface LifecycleAccepted {
  task_id: string;
}

export interface LifecycleAlready {
  status: "already";
  desired_state: string;
}

export type LifecycleResult = LifecycleAccepted | LifecycleAlready;

export function isLifecycleAccepted(
  result: LifecycleResult,
): result is LifecycleAccepted {
  return "task_id" in result;
}

const TERMINAL_TASK_STATES: ReadonlySet<string> = new Set([
  "succeeded",
  "failed",
  "cancelled",
]);

export function isTerminalTaskState(state: string): boolean {
  return TERMINAL_TASK_STATES.has(state);
}

export async function fetchStatus(id: string): Promise<ServerStatus> {
  const res = await authFetch(`/api/v1/servers/${encodeURIComponent(id)}/status`);
  return parseJson<ServerStatus>(res, "get server status");
}

export async function fetchServerTasks(
  id: string,
  limit = 10,
  offset = 0,
): Promise<ServerTasksResponse> {
  const qs = `?limit=${encodeURIComponent(String(limit))}&offset=${encodeURIComponent(String(offset))}`;
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(id)}/tasks${qs}`,
  );
  return parseJson<ServerTasksResponse>(res, "list server tasks");
}

export async function fetchMetricsSummary(): Promise<MetricsSummary> {
  const res = await authFetch("/api/v1/metrics/summary");
  return parseJson<MetricsSummary>(res, "get metrics summary");
}

export async function fetchServerLogs(
  id: string,
  tail = 200,
): Promise<ServerLogsResponse> {
  const qs = `?tail=${encodeURIComponent(String(tail))}`;
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(id)}/logs${qs}`,
  );
  return parseJson<ServerLogsResponse>(res, "get server logs");
}

export async function postLifecycle(
  id: string,
  op: LifecycleOp,
): Promise<LifecycleResult> {
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(id)}/${op}`,
    { method: "POST" },
  );
  return parseJson<LifecycleResult>(res, `${op} server`);
}
