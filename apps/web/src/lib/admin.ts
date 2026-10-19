import { authFetch } from "./auth";

// ---------------------------------------------------------------------------
// Admin + image-update typed clients (Phase 7, Task 3).
// Key names match the frozen backend contracts exactly. The backend lands in
// parallel, so every reader must tolerate missing/extra keys and every caller
// must handle loading/error states defensively.
// ---------------------------------------------------------------------------

export class AdminApiError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "AdminApiError";
    this.status = status;
  }
}

interface ErrorBody {
  error?: string;
  error_code?: string;
  message?: string;
}

async function parseJson<T>(res: Response, label: string): Promise<T> {
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as ErrorBody;
      detail = body.error_code ?? body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new AdminApiError(`${label} failed (${res.status}): ${detail}`, res.status);
  }
  return res.json() as Promise<T>;
}

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : "Request failed.";
}

// Roles known to the backend (`auth::Role::from_str`): owner, admin, viewer.
export function isPrivilegedRole(role: string | null | undefined): boolean {
  return role === "owner" || role === "admin";
}

// ---------------------------------------------------------------------------
// Users — GET /users (exists), POST /users, PATCH /users/:id, DELETE /users/:id
// ---------------------------------------------------------------------------

export interface AdminUser {
  id: string;
  username: string;
  role: string;
  is_active: boolean;
  created_at: string;
  updated_at?: string | null;
  last_login_at?: string | null;
}

export interface UserListResponse {
  users: AdminUser[];
}

export interface CreateUserInput {
  username: string;
  password: string;
  role: string;
}

export interface CreateUserResponse {
  id: string;
  username: string;
  role: string;
}

function normalizeUser(raw: Partial<AdminUser> & { id: string }): AdminUser {
  return {
    id: raw.id,
    username: typeof raw.username === "string" ? raw.username : raw.id,
    role: typeof raw.role === "string" ? raw.role : "viewer",
    is_active: typeof raw.is_active === "boolean" ? raw.is_active : true,
    created_at: typeof raw.created_at === "string" ? raw.created_at : "",
    updated_at: typeof raw.updated_at === "string" ? raw.updated_at : null,
    last_login_at:
      typeof raw.last_login_at === "string" ? raw.last_login_at : null,
  };
}

export async function listUsers(): Promise<UserListResponse> {
  const res = await authFetch("/api/v1/users");
  const raw = await parseJson<{ users?: Array<Partial<AdminUser> & { id: string }> }>(
    res,
    "list users",
  );
  const users = Array.isArray(raw.users) ? raw.users.map(normalizeUser) : [];
  return { users };
}

export async function createUser(input: CreateUserInput): Promise<CreateUserResponse> {
  const res = await authFetch("/api/v1/users", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  return parseJson<CreateUserResponse>(res, "create user");
}

export interface UpdateUserPatch {
  role?: string;
  is_active?: boolean;
}

export async function updateUser(id: string, patch: UpdateUserPatch): Promise<void> {
  const res = await authFetch(`/api/v1/users/${encodeURIComponent(id)}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
  });
  await parseJson<unknown>(res, "update user");
}

export async function resetUserPassword(
  id: string,
  password: string,
): Promise<void> {
  const res = await authFetch(
    `/api/v1/users/${encodeURIComponent(id)}/reset-password`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ password }),
    },
  );
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as ErrorBody;
      detail = body.error_code ?? body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new AdminApiError(
      `reset password failed (${res.status}): ${detail}`,
      res.status,
    );
  }
}

export async function deleteUser(id: string): Promise<void> {
  const res = await authFetch(`/api/v1/users/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  if (res.status === 204) return;
  await parseJson<unknown>(res, "delete user");
}

// ---------------------------------------------------------------------------
// Audit — GET /audit?limit=&offset=&action= → {events: [...]}
// Metadata is redacted server-side; display it as opaque text only.
// ---------------------------------------------------------------------------

export interface AuditEvent {
  id: string;
  actor_user_id: string | null;
  action: string;
  target_type: string;
  target_id: string;
  outcome: string;
  request_id?: string | null;
  created_at: string;
  metadata: string;
}

export interface AuditListResponse {
  events: AuditEvent[];
}

export interface AuditQuery {
  limit?: number;
  offset?: number;
  action?: string;
}

function normalizeEvent(raw: Partial<AuditEvent> & { id?: string }): AuditEvent {
  const meta = (raw as { metadata?: unknown }).metadata;
  return {
    id: typeof raw.id === "string" ? raw.id : "",
    actor_user_id:
      typeof raw.actor_user_id === "string" ? raw.actor_user_id : null,
    action: typeof raw.action === "string" ? raw.action : "unknown",
    target_type: typeof raw.target_type === "string" ? raw.target_type : "",
    target_id: typeof raw.target_id === "string" ? raw.target_id : "",
    outcome: typeof raw.outcome === "string" ? raw.outcome : "",
    request_id: typeof raw.request_id === "string" ? raw.request_id : null,
    created_at: typeof raw.created_at === "string" ? raw.created_at : "",
    metadata: typeof meta === "string" ? meta : meta == null ? "" : JSON.stringify(meta),
  };
}

export async function listAudit(query: AuditQuery = {}): Promise<AuditListResponse> {
  const params = new URLSearchParams();
  params.set("limit", String(query.limit ?? 50));
  params.set("offset", String(query.offset ?? 0));
  if (query.action && query.action.trim() !== "") {
    params.set("action", query.action.trim());
  }
  const res = await authFetch(`/api/v1/audit?${params.toString()}`);
  const raw = await parseJson<{ events?: Array<Partial<AuditEvent>> }>(
    res,
    "list audit events",
  );
  const events = Array.isArray(raw.events) ? raw.events.map(normalizeEvent) : [];
  return { events };
}

// ---------------------------------------------------------------------------
// Manager database — GET /admin/database/backup (blob),
// POST /admin/database/restore (multipart) → {staged, restart_required}
// ---------------------------------------------------------------------------

export interface RestoreDatabaseResponse {
  staged: boolean;
  restart_required: boolean;
}

export async function downloadDatabaseBackup(): Promise<Blob> {
  const res = await authFetch("/api/v1/admin/database/backup");
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as ErrorBody;
      detail = body.error_code ?? body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new AdminApiError(
      `download database backup failed (${res.status}): ${detail}`,
      res.status,
    );
  }
  return res.blob();
}

export async function restoreDatabase(file: File): Promise<RestoreDatabaseResponse> {
  const form = new FormData();
  form.append("file", file, file.name);
  const res = await authFetch("/api/v1/admin/database/restore", {
    method: "POST",
    body: form,
  });
  const raw = await parseJson<Partial<RestoreDatabaseResponse>>(
    res,
    "stage database restore",
  );
  return {
    staged: raw.staged === true,
    restart_required: raw.restart_required !== false,
  };
}

// ---------------------------------------------------------------------------
// Image update — POST /servers/:id/update/preview,
// POST /servers/:id/update → 202 {task_id},
// POST /servers/:id/rollback → 202 {task_id}
// ---------------------------------------------------------------------------

export interface UpdatePreview {
  warnings: string[];
  resolved_digest: string | null;
  current_reference: string;
}

export interface UpdateAccepted {
  task_id: string;
}

export async function previewUpdate(
  serverId: string,
  imageReference: string,
  imageDigest?: string,
): Promise<UpdatePreview> {
  const body: { image_reference: string; image_digest?: string } = {
    image_reference: imageReference,
  };
  if (imageDigest && imageDigest.trim() !== "") {
    body.image_digest = imageDigest.trim();
  }
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(serverId)}/update/preview`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
  );
  const raw = await parseJson<Partial<UpdatePreview>>(res, "preview image update");
  return {
    warnings: Array.isArray(raw.warnings)
      ? raw.warnings.filter((w): w is string => typeof w === "string")
      : [],
    resolved_digest:
      typeof raw.resolved_digest === "string" ? raw.resolved_digest : null,
    current_reference:
      typeof raw.current_reference === "string" ? raw.current_reference : "",
  };
}

export async function requestUpdate(
  serverId: string,
  imageReference: string,
  imageDigest?: string,
  allowWithoutBackup = false,
): Promise<UpdateAccepted> {
  const body: {
    image_reference: string;
    image_digest?: string;
    allow_without_backup?: boolean;
  } = { image_reference: imageReference };
  if (imageDigest && imageDigest.trim() !== "") {
    body.image_digest = imageDigest.trim();
  }
  if (allowWithoutBackup) body.allow_without_backup = true;
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(serverId)}/update`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
  );
  return parseJson<UpdateAccepted>(res, "update server image");
}

export async function requestRollback(serverId: string): Promise<UpdateAccepted> {
  const res = await authFetch(
    `/api/v1/servers/${encodeURIComponent(serverId)}/rollback`,
    { method: "POST" },
  );
  return parseJson<UpdateAccepted>(res, "roll back server image");
}

// Additive keys the controller guarantees on GET-server; optional here so the
// UI stays correct when talking to a backend that predates the wiring.
export interface ServerImageHistory {
  previous_image_reference?: string | null;
  previous_image_digest?: string | null;
  image_digest?: string | null;
}

export function previousImageOf(server: object): string | null {
  const ref = (server as ServerImageHistory).previous_image_reference;
  return typeof ref === "string" && ref !== "" ? ref : null;
}

export { errMsg };
