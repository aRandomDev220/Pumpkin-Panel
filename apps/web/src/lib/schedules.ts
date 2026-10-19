import { authFetch } from "./auth";

export interface ScheduledRestart {
  id: string;
  server_id: string;
  kind: string;
  schedule: string;
  timezone: string;
  timezone_note?: string;
  enabled: boolean;
  last_run_at?: string | null;
  next_run_at?: string | null;
  created_by?: string | null;
  created_at?: string;
  updated_at?: string;
}

interface ErrorBody {
  error_code?: string;
  message?: string;
}

async function parseJson<T>(res: Response, label: string): Promise<T> {
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as ErrorBody;
      detail = body.error_code ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new Error(`${label} failed (${res.status}): ${detail}`);
  }
  return res.json() as Promise<T>;
}

export async function listScheduledRestarts(
  serverId: string,
): Promise<ScheduledRestart[]> {
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(serverId)}/scheduled-tasks`,
  );
  const body = await parseJson<{ scheduled_tasks: ScheduledRestart[] }>(
    res,
    "list scheduled restarts",
  );
  return body.scheduled_tasks;
}

export async function createScheduledRestart(
  serverId: string,
  input: { schedule: string; timezone: string; enabled: boolean },
): Promise<ScheduledRestart> {
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(serverId)}/scheduled-tasks`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        kind: "restart",
        schedule: input.schedule,
        timezone: input.timezone,
        enabled: input.enabled,
      }),
    },
  );
  const body = await parseJson<{ scheduled_task: ScheduledRestart }>(
    res,
    "create scheduled restart",
  );
  return body.scheduled_task;
}

export async function deleteScheduledRestart(id: string): Promise<void> {
  const res = await authFetch(
    `/api/v1/scheduled-tasks/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  );
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as ErrorBody;
      detail = body.error_code ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new Error(`delete scheduled restart failed (${res.status}): ${detail}`);
  }
}
