import { authFetch } from "./auth";

export interface GuidedConfig {
  seed?: string;
  default_difficulty?: string;
  default_gamemode?: string;
  default_level_name?: string;
  hardcore?: boolean;
  white_list?: boolean;
  enforce_whitelist?: boolean;
  tps?: number;
  motd?: string;
  max_players?: number;
  view_distance?: number;
  simulation_distance?: number;
  online_mode?: boolean;
  [key: string]: string | number | boolean | undefined | null;
}

export interface ConfigProfile {
  pumpkin_version: string;
  known: boolean;
}

export interface ServerConfig {
  revision: number;
  hash: string;
  content: string;
  guided: GuidedConfig;
  profile: ConfigProfile;
  diverged?: boolean;
  diverged_reason?: string;
  valid?: boolean;
  errors?: ConfigErrorItem[];
}

export interface SaveConfigResult {
  revision: number;
  hash: string;
  restart_required: boolean;
}

export type ConfigErrorItem = string | { message?: string; line?: number };

function errorMessage(item: ConfigErrorItem): string {
  if (typeof item === "string") return item;
  if (typeof item.message === "string" && item.message.length > 0) {
    return typeof item.line === "number" ? `line ${item.line}: ${item.message}` : item.message;
  }
  return JSON.stringify(item);
}

export function normalizeErrors(errors: ConfigErrorItem[] | undefined): string[] {
  if (!errors) return [];
  return errors.map(errorMessage);
}

export interface ValidateConfigResult {
  ok: boolean;
  errors: ConfigErrorItem[];
}

export interface ConfigRevision {
  revision: number;
  hash: string;
  content: string;
  created_at: string;
  created_by: string | null;
}

export interface ConfigRevisionList {
  revisions: ConfigRevision[];
  total: number;
}

interface ApiErrorBody {
  error?: string;
  message?: string;
  error_code?: string;
  errors?: ConfigErrorItem[];
}

export class ConfigConflictError extends Error {
  constructor(message = "Config revision conflict — reload and try again.") {
    super(message);
    this.name = "ConfigConflictError";
  }
}

export class ConfigValidationError extends Error {
  errorCode: string;
  errors: string[];
  constructor(message: string, errorCode = "validation_failed", errors: string[] = []) {
    super(message);
    this.name = "ConfigValidationError";
    this.errorCode = errorCode;
    this.errors = errors.length > 0 ? errors : [message];
  }
}

async function readErrorBody(res: Response): Promise<ApiErrorBody> {
  try {
    return (await res.json()) as ApiErrorBody;
  } catch {
    return {};
  }
}

function base(id: string): string {
  return `/api/v1/servers/${encodeURIComponent(id)}/config`;
}

export async function fetchConfig(id: string): Promise<ServerConfig> {
  const res = await authFetch(base(id));
  if (!res.ok) {
    const body = await readErrorBody(res);
    throw new Error(`get config failed (${res.status}): ${body.error ?? body.message ?? res.statusText}`);
  }
  return (await res.json()) as ServerConfig;
}

export async function saveConfig(id: string, revision: number, content: string): Promise<SaveConfigResult> {
  const res = await authFetch(base(id), {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ revision, content }),
  });
  if (res.status === 409) {
    throw new ConfigConflictError();
  }
  if (res.status === 422) {
    const body = await readErrorBody(res);
    const errors = normalizeErrors(body.errors);
    const message = body.message ?? body.error ?? "Config validation failed.";
    throw new ConfigValidationError(message, body.error_code ?? "validation_failed", errors.length > 0 ? errors : [message]);
  }
  if (!res.ok) {
    const body = await readErrorBody(res);
    throw new Error(`save config failed (${res.status}): ${body.error ?? body.message ?? res.statusText}`);
  }
  return (await res.json()) as SaveConfigResult;
}

export async function validateConfig(id: string, content: string): Promise<ValidateConfigResult> {
  const res = await authFetch(`${base(id)}/validate`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  if (!res.ok) {
    const body = await readErrorBody(res);
    throw new Error(`validate config failed (${res.status}): ${body.error ?? body.message ?? res.statusText}`);
  }
  return (await res.json()) as ValidateConfigResult;
}

export async function listConfigRevisions(id: string): Promise<ConfigRevisionList> {
  const res = await authFetch(`${base(id)}/revisions`);
  if (!res.ok) {
    const body = await readErrorBody(res);
    throw new Error(`list revisions failed (${res.status}): ${body.error ?? body.message ?? res.statusText}`);
  }
  return (await res.json()) as ConfigRevisionList;
}

export async function revertConfig(id: string, revision: number): Promise<void> {
  const res = await authFetch(`${base(id)}/revert`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ revision }),
  });
  if (!res.ok) {
    const body = await readErrorBody(res);
    throw new Error(`revert config failed (${res.status}): ${body.error ?? body.message ?? res.statusText}`);
  }
}
