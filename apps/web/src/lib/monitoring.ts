import { authFetch } from "./auth";

// ---------------------------------------------------------------------------
// Monitoring typed clients (Phase 6, Task 4).
// Key names match the frozen backend contracts exactly. The backend lands in
// parallel, so every caller must handle loading/error states defensively.
// Null metric values mean "unavailable/stale" — callers must render them as
// gaps and MUST NOT 0-fill them.
// ---------------------------------------------------------------------------

export type MetricsRange = "1h" | "24h" | "7d";

export const METRICS_RANGES: readonly MetricsRange[] = ["1h", "24h", "7d"];

export function isMetricsRange(value: string): value is MetricsRange {
  return value === "1h" || value === "24h" || value === "7d";
}

export interface MetricSample {
  sampled_at: string;
  cpu_percent: number | null;
  memory_bytes: number | null;
  players_online: number | null;
  latency_ms: number | null;
  state: string;
}

export interface MetricsHistoryResponse {
  samples: MetricSample[];
  total: number;
}

export interface BackendAlert {
  severity: string;
  kind: string;
  server_id?: string | null;
  message: string;
}

export interface AlertsResponse {
  alerts: BackendAlert[];
}

export class MonitoringApiError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "MonitoringApiError";
    this.status = status;
  }
}

export function isMonitoringApiError(
  err: unknown,
  status?: number,
): err is MonitoringApiError {
  if (!(err instanceof MonitoringApiError)) return false;
  return status === undefined || err.status === status;
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
    throw new MonitoringApiError(
      `${label} failed (${res.status}): ${detail}`,
      res.status,
    );
  }
  return res.json() as Promise<T>;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function toNumberOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function normalizeSample(raw: unknown): MetricSample | null {
  if (!isRecord(raw)) return null;
  if (typeof raw.sampled_at !== "string" || typeof raw.state !== "string") {
    return null;
  }
  return {
    sampled_at: raw.sampled_at,
    cpu_percent: toNumberOrNull(raw.cpu_percent),
    memory_bytes: toNumberOrNull(raw.memory_bytes),
    players_online: toNumberOrNull(raw.players_online),
    latency_ms: toNumberOrNull(raw.latency_ms),
    state: raw.state,
  };
}

function normalizeAlert(raw: unknown): BackendAlert | null {
  if (!isRecord(raw)) return null;
  if (
    typeof raw.severity !== "string" ||
    typeof raw.kind !== "string" ||
    typeof raw.message !== "string"
  ) {
    return null;
  }
  const serverId = raw.server_id;
  return {
    severity: raw.severity,
    kind: raw.kind,
    server_id: typeof serverId === "string" ? serverId : null,
    message: raw.message,
  };
}

export async function fetchMetricsHistory(
  serverId: string,
  range: MetricsRange,
): Promise<MetricsHistoryResponse> {
  const qs = `?range=${encodeURIComponent(range)}`;
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(serverId)}/metrics${qs}`,
  );
  const raw = await parseJson<unknown>(res, "get metrics history");
  if (!isRecord(raw) || !Array.isArray(raw.samples)) {
    throw new MonitoringApiError(
      "get metrics history failed: malformed response envelope",
      0,
    );
  }
  const samples = raw.samples
    .map(normalizeSample)
    .filter((s): s is MetricSample => s !== null);
  const total =
    typeof raw.total === "number" && Number.isFinite(raw.total)
      ? raw.total
      : samples.length;
  return { samples, total };
}

export async function fetchAlerts(): Promise<AlertsResponse> {
  const res = await authFetch("/api/v1/alerts");
  const raw = await parseJson<unknown>(res, "list alerts");
  if (!isRecord(raw) || !Array.isArray(raw.alerts)) {
    throw new MonitoringApiError(
      "list alerts failed: malformed response envelope",
      0,
    );
  }
  const alerts = raw.alerts
    .map(normalizeAlert)
    .filter((a): a is BackendAlert => a !== null);
  return { alerts };
}

export async function fetchDiagnostics(): Promise<Blob> {
  const res = await authFetch("/api/v1/admin/diagnostics");
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as { error?: string; message?: string };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new MonitoringApiError(
      `get diagnostics failed (${res.status}): ${detail}`,
      res.status,
    );
  }
  return res.blob();
}

export { formatBytes as formatBytesShort } from "./format";
