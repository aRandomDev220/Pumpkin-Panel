import { authFetch } from "./auth";

// ---------------------------------------------------------------------------
// Backups + backup-policy typed clients.
// Key names match the frozen backend contracts exactly. The backend lands in
// parallel, so every caller must handle loading/error states defensively.
// ---------------------------------------------------------------------------

export type BackupConsistency = "stopped" | "live";

export interface Backup {
  id: string;
  type: string;
  state: string;
  size_bytes: number;
  checksum: string;
  consistency_method: string;
  created_at: string;
  completed_at: string | null;
  protected: boolean;
  error_summary: string | null;
}

export interface BackupListResponse {
  backups: Backup[];
  total: number;
}

export interface CreateBackupResponse {
  task_id: string;
  backup: {
    id: string;
    state: string;
  };
}

export interface RestoreBackupResponse {
  task_id: string;
}

export interface BackupPolicy {
  id: string;
  server_id: string;
  name: string;
  schedule: string;
  timezone: string;
  retention_count: number;
  max_age_days: number;
  enabled: boolean;
  last_run_at: string | null;
  next_run_at: string | null;
  timezone_note?: string | null;
}

export interface BackupPolicyListResponse {
  policies: BackupPolicy[];
  total: number;
}

export interface BackupPolicyInput {
  name: string;
  schedule: string;
  timezone: string;
  retention_count: number;
  max_age_days: number;
  enabled: boolean;
}

export interface BackupPolicyPatch {
  name?: string;
  schedule?: string;
  timezone?: string;
  retention_count?: number;
  max_age_days?: number;
  enabled?: boolean;
}

export interface BackupPreviewResponse {
  would_delete: string[];
  retained: number;
  protected_kept: number;
}

export class BackupApiError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "BackupApiError";
    this.status = status;
  }
}

async function parseJson<T>(res: Response, label: string): Promise<T> {
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as {
        error?: string;
        message?: string;
      };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new BackupApiError(`${label} failed (${res.status}): ${detail}`, res.status);
  }
  return res.json() as Promise<T>;
}

function backupsBase(serverId: string): string {
  return `/api/v1/servers/${encodeURIComponent(serverId)}/backups`;
}

function policiesBase(serverId: string): string {
  return `/api/v1/servers/${encodeURIComponent(serverId)}/backup-policies`;
}

export async function createBackup(
  serverId: string,
  consistency: BackupConsistency,
): Promise<CreateBackupResponse> {
  const res = await authFetch(backupsBase(serverId), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ mode: "full", consistency }),
  });
  return parseJson<CreateBackupResponse>(res, "create backup");
}

export async function listBackups(serverId: string): Promise<BackupListResponse> {
  const res = await authFetch(backupsBase(serverId));
  return parseJson<BackupListResponse>(res, "list backups");
}

export async function getBackup(id: string): Promise<Backup> {
  const res = await authFetch(`/api/v1/backups/${encodeURIComponent(id)}`);
  return parseJson<Backup>(res, "get backup");
}

export async function downloadBackup(id: string): Promise<Blob> {
  const res = await authFetch(
    `/api/v1/backups/${encodeURIComponent(id)}/download`,
  );
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as {
        error?: string;
        message?: string;
      };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new BackupApiError(
      `download backup failed (${res.status}): ${detail}`,
      res.status,
    );
  }
  return res.blob();
}

export async function deleteBackup(id: string): Promise<void> {
  const res = await authFetch(`/api/v1/backups/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  if (!res.ok && res.status !== 204) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as {
        error?: string;
        message?: string;
      };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new BackupApiError(
      `delete backup failed (${res.status}): ${detail}`,
      res.status,
    );
  }
}

export async function restoreBackup(
  id: string,
  preRestoreBackup = true,
): Promise<RestoreBackupResponse> {
  const res = await authFetch(
    `/api/v1/backups/${encodeURIComponent(id)}/restore`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ pre_restore_backup: preRestoreBackup }),
    },
  );
  return parseJson<RestoreBackupResponse>(res, "restore backup");
}

export async function listPolicies(
  serverId: string,
): Promise<BackupPolicyListResponse> {
  const res = await authFetch(policiesBase(serverId));
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as {
        error?: string;
        message?: string;
      };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new BackupApiError(
      `list backup policies failed (${res.status}): ${detail}`,
      res.status,
    );
  }
  const raw = (await res.json()) as
    | BackupPolicyListResponse
    | BackupPolicy[];
  if (Array.isArray(raw)) {
    return { policies: raw, total: raw.length };
  }
  return {
    policies: Array.isArray(raw.policies) ? raw.policies : [],
    total:
      typeof raw.total === "number"
        ? raw.total
        : Array.isArray(raw.policies)
          ? raw.policies.length
          : 0,
  };
}

export async function createPolicy(
  serverId: string,
  input: BackupPolicyInput,
): Promise<BackupPolicy> {
  const res = await authFetch(policiesBase(serverId), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  return parseJson<BackupPolicy>(res, "create backup policy");
}

export async function updatePolicy(
  id: string,
  patch: BackupPolicyPatch,
): Promise<BackupPolicy> {
  const res = await authFetch(
    `/api/v1/backup-policies/${encodeURIComponent(id)}`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    },
  );
  return parseJson<BackupPolicy>(res, "update backup policy");
}

export async function deletePolicy(id: string): Promise<void> {
  const res = await authFetch(
    `/api/v1/backup-policies/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  );
  if (!res.ok && res.status !== 204) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as {
        error?: string;
        message?: string;
      };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new BackupApiError(
      `delete backup policy failed (${res.status}): ${detail}`,
      res.status,
    );
  }
}

export async function previewPolicies(
  serverId: string,
  draft: BackupPolicyPatch & { schedule: string },
): Promise<BackupPreviewResponse> {
  const res = await authFetch(`${policiesBase(serverId)}/preview`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(draft),
  });
  return parseJson<BackupPreviewResponse>(res, "preview backup policy");
}

export function checksumShort(checksum: string): string {
  if (!checksum) return "n/a";
  return checksum.slice(0, 12);
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "n/a";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}
