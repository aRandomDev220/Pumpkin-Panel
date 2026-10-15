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
