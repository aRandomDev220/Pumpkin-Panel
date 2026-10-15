import { authFetch } from "./auth";

export type FileEntryType = "file" | "dir";

export interface FileEntry {
  name: string;
  type: string;
  size: number;
  mtime: string;
}

export interface FileListResponse {
  entries: FileEntry[];
}

interface ApiErrorBody {
  error?: string;
  message?: string;
}

async function throwForStatus(res: Response, label: string): Promise<ApiErrorBody> {
  const body: ApiErrorBody = await res
    .json()
    .then((b) => b as ApiErrorBody)
    .catch(() => ({}));
  throw new Error(`${label} failed (${res.status}): ${body.error ?? body.message ?? res.statusText}`);
}

function filesBase(id: string): string {
  return `/api/v1/servers/${encodeURIComponent(id)}/files`;
}

function withPath(url: string, path: string): string {
  return `${url}?path=${encodeURIComponent(path)}`;
}

export function isDirectory(entry: FileEntry): boolean {
  return entry.type === "dir";
}

export function joinRelative(parent: string, name: string): string {
  if (!parent) return name;
  return `${parent.replace(/\/+$/, "")}/${name}`;
}

export function parentOf(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  if (idx < 0) return "";
  return trimmed.slice(0, idx);
}

export async function listFiles(id: string, path: string): Promise<FileListResponse> {
  const res = await authFetch(withPath(filesBase(id), path));
  if (!res.ok) await throwForStatus(res, "list files");
  return (await res.json()) as FileListResponse;
}

export async function downloadFile(id: string, path: string): Promise<Blob> {
  const res = await authFetch(withPath(`${filesBase(id)}/download`, path));
  if (!res.ok) await throwForStatus(res, "download file");
  return res.blob();
}

export async function exportFiles(id: string, path: string): Promise<Blob> {
  const res = await authFetch(withPath(`${filesBase(id)}/export`, path));
  if (!res.ok) await throwForStatus(res, "export files");
  return res.blob();
}

export async function uploadFile(id: string, path: string, file: File): Promise<void> {
  const form = new FormData();
  form.append("file", file, file.name);
  const res = await authFetch(withPath(`${filesBase(id)}/upload`, path), {
    method: "POST",
    body: form,
  });
  if (!res.ok) await throwForStatus(res, "upload file");
}

export async function importArchive(id: string, path: string, file: File): Promise<void> {
  const form = new FormData();
  form.append("file", file, file.name);
  const res = await authFetch(withPath(`${filesBase(id)}/import`, path), {
    method: "POST",
    body: form,
  });
  if (!res.ok) await throwForStatus(res, "import archive");
}

export async function createDirectory(id: string, path: string): Promise<void> {
  const res = await authFetch(`${filesBase(id)}/directory`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path }),
  });
  if (!res.ok) await throwForStatus(res, "create directory");
}

export async function renameEntry(id: string, from: string, to: string): Promise<void> {
  const res = await authFetch(`${filesBase(id)}/rename`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ from, to }),
  });
  if (!res.ok) await throwForStatus(res, "rename");
}

export async function deleteEntry(id: string, path: string): Promise<void> {
  const res = await authFetch(withPath(filesBase(id), path), { method: "DELETE" });
  if (!res.ok) await throwForStatus(res, "delete");
}

export function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}
