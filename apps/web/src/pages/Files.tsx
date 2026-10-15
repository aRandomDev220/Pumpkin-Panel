import { useState } from "react";
import type { ChangeEvent, FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import {
  createDirectory,
  deleteEntry,
  downloadFile,
  exportFiles,
  importArchive,
  isDirectory,
  joinRelative,
  listFiles,
  parentOf,
  renameEntry,
  saveBlob,
  uploadFile,
} from "../lib/files";
import { getServer } from "../lib/servers";

function displayPath(path: string): string {
  return path === "" ? "/" : `/${path}`;
}

function crumbs(path: string): Array<{ label: string; path: string }> {
  if (!path) return [];
  const parts = path.split("/").filter((p) => p.length > 0);
  const out: Array<{ label: string; path: string }> = [];
  let acc = "";
  for (const part of parts) {
    acc = acc === "" ? part : `${acc}/${part}`;
    out.push({ label: part, path: acc });
  }
  return out;
}

function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "n/a";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = bytes / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u += 1;
  }
  return `${v.toFixed(1)} ${units[u]}`;
}

export default function Files() {
  const { id } = useParams<{ id: string }>();
  const qc = useQueryClient();
  const [path, setPath] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [uploading, setUploading] = useState(false);
  const [importing, setImporting] = useState(false);
  const [mkdirName, setMkdirName] = useState("");
  const [renameTarget, setRenameTarget] = useState("");
  const [renameSource, setRenameSource] = useState<string | null>(null);

  const serverId = id ?? "";

  const serverQuery = useQuery({
    queryKey: ["servers", id],
    queryFn: () => getServer(serverId),
    enabled: !!id,
    retry: 1,
  });

  const filesQuery = useQuery({
    queryKey: ["servers", id, "files", path],
    queryFn: () => listFiles(serverId, path),
    enabled: !!id,
    retry: 1,
  });

  function invalidate() {
    qc.invalidateQueries({ queryKey: ["servers", id, "files"] });
  }

  const mkdirMutation = useMutation({
    mutationFn: (name: string) => createDirectory(serverId, joinRelative(path, name)),
    onSuccess: (_v, name) => {
      setNotice(`Created directory ${joinRelative(path, name)}.`);
      setError(null);
      setMkdirName("");
      invalidate();
    },
    onError: (err) => setError(err instanceof Error ? err.message : "mkdir failed."),
  });

  const renameMutation = useMutation({
    mutationFn: ({ from, to }: { from: string; to: string }) => renameEntry(serverId, from, to),
    onSuccess: () => {
      setNotice("Renamed.");
      setError(null);
      setRenameSource(null);
      setRenameTarget("");
      invalidate();
    },
    onError: (err) => setError(err instanceof Error ? err.message : "rename failed."),
  });

  const deleteMutation = useMutation({
    mutationFn: (target: string) => deleteEntry(serverId, target),
    onSuccess: () => {
      setNotice("Deleted.");
      setError(null);
      invalidate();
    },
    onError: (err) => setError(err instanceof Error ? err.message : "delete failed."),
  });

  if (!id) return <p className="err">Missing server id.</p>;

  const desiredState = serverQuery.data?.desired_state;
  const importBlocked = desiredState !== undefined && desiredState !== "stopped";

  async function onDownload(entryPath: string, filename: string) {
    setError(null);
    setNotice(null);
    try {
      const blob = await downloadFile(serverId, entryPath);
      saveBlob(blob, filename);
    } catch (e) {
      setError(e instanceof Error ? e.message : "download failed.");
    }
  }

  async function onExport() {
    setError(null);
    setNotice(null);
    try {
      const blob = await exportFiles(serverId, path);
      const filename = path === "" ? "export.tar.gz" : `${path.split("/").pop() ?? "export"}.tar.gz`;
      saveBlob(blob, filename);
      setNotice("Export downloaded.");
    } catch (e) {
      setError(e instanceof Error ? e.message : "export failed.");
    }
  }

  async function onUpload(e: ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setUploading(true);
    setError(null);
    try {
      await uploadFile(serverId, joinRelative(path, file.name), file);
      setNotice(`Uploaded ${file.name}.`);
      invalidate();
    } catch (err) {
      setError(err instanceof Error ? err.message : "upload failed.");
    } finally {
      setUploading(false);
    }
  }

  async function onImport(e: ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setImporting(true);
    setError(null);
    try {
      await importArchive(serverId, path, file);
      setNotice(`Imported ${file.name}.`);
      invalidate();
    } catch (err) {
      setError(err instanceof Error ? err.message : "import failed.");
    } finally {
      setImporting(false);
    }
  }

  function onMkdir(e: FormEvent) {
    e.preventDefault();
    const name = mkdirName.trim().replace(/\//g, "");
    if (!name) return;
    mkdirMutation.mutate(name);
  }

  function onRename(e: FormEvent) {
    e.preventDefault();
    if (!renameSource) return;
    const to = renameTarget.trim();
    if (!to) return;
    renameMutation.mutate({ from: renameSource, to: joinRelative(path, to.replace(/\//g, "")) });
  }

  return (
    <section>
      <p className="muted">
        <Link to={`/servers/${encodeURIComponent(id)}`}>← Server</Link>
        {" · "}
        <Link to={`/servers/${encodeURIComponent(id)}/config`}>Config</Link>
      </p>
      <h2>Files</h2>
      <p className="muted" title="Relative path">
        path: <code>{displayPath(path)}</code>
      </p>

      <nav aria-label="Breadcrumb" className="row">
        <button type="button" onClick={() => setPath("")}>
          /
        </button>
        {crumbs(path).map((c) => (
          <span key={c.path}>
            <span className="muted"> / </span>
            <button type="button" onClick={() => setPath(c.path)}>
              {c.label}
            </button>
          </span>
        ))}
        {path !== "" && (
          <button type="button" onClick={() => setPath(parentOf(path))}>
            ↑ Up
          </button>
        )}
      </nav>

      {notice && (
        <p className="muted" role="status">
          {notice}
        </p>
      )}
      {error && (
        <p className="err" role="alert">
          {error}
        </p>
      )}

      {filesQuery.isLoading && <p>Loading files…</p>}
      {filesQuery.error && <p className="err">Failed to load files.</p>}
      {filesQuery.data && filesQuery.data.entries.length === 0 && (
        <p className="muted">Empty directory.</p>
      )}
      {filesQuery.data && filesQuery.data.entries.length > 0 && (
        <table className="table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Type</th>
              <th>Size</th>
              <th>Modified</th>
              <th>Actions</th>
            </tr>
          </thead>
          <tbody>
            {filesQuery.data.entries.map((entry) => {
              const entryPath = joinRelative(path, entry.name);
              const dir = isDirectory(entry);
              return (
                <tr key={entry.name}>
                  <td>
                    {dir ? (
                      <button type="button" onClick={() => setPath(entryPath)}>
                        {entry.name}/
                      </button>
                    ) : (
                      entry.name
                    )}
                  </td>
                  <td>{entry.type}</td>
                  <td>{dir ? "—" : formatSize(entry.size)}</td>
                  <td>{entry.mtime}</td>
                  <td>
                    <div className="row">
                      {!dir && (
                        <button type="button" onClick={() => void onDownload(entryPath, entry.name)}>
                          Download
                        </button>
                      )}
                      <button
                        type="button"
                        onClick={() => {
                          setRenameSource(entryPath);
                          setRenameTarget(entry.name);
                        }}
                      >
                        Rename
                      </button>
                      <button
                        type="button"
                        disabled={deleteMutation.isPending}
                        onClick={() => {
                          if (window.confirm(`Delete ${entryPath}?`)) {
                            deleteMutation.mutate(entryPath);
                          }
                        }}
                      >
                        Delete
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}

      <h3>Upload</h3>
      <div className="row">
        <label>
          Upload file{" "}
          <input type="file" onChange={(e) => void onUpload(e)} disabled={uploading} />
        </label>
        {uploading && <span className="muted">Uploading…</span>}
      </div>

      <h3>New directory</h3>
      <form onSubmit={onMkdir}>
        <label>
          Name <input value={mkdirName} onChange={(e) => setMkdirName(e.target.value)} placeholder="dirname" />
        </label>{" "}
        <button type="submit" disabled={mkdirMutation.isPending}>
          {mkdirMutation.isPending ? "Creating…" : "Create"}
        </button>
      </form>

      {renameSource && (
        <>
          <h3>Rename</h3>
          <form onSubmit={onRename}>
            <p className="muted">
              from <code>{renameSource}</code>
            </p>
            <label>
              New name{" "}
              <input value={renameTarget} onChange={(e) => setRenameTarget(e.target.value)} />
            </label>{" "}
            <button type="submit" disabled={renameMutation.isPending}>
              {renameMutation.isPending ? "Renaming…" : "Rename"}
            </button>{" "}
            <button
              type="button"
              onClick={() => {
                setRenameSource(null);
                setRenameTarget("");
              }}
            >
              Cancel
            </button>
          </form>
        </>
      )}

      <h3>Import / Export</h3>
      <p className="muted">
        server desired_state:{" "}
        {serverQuery.isLoading ? "loading…" : (desiredState ?? "unknown")}
      </p>
      <div className="row">
        <button type="button" onClick={() => void onExport()}>
          Export (tar.gz)
        </button>
        <label title={importBlocked ? "Stop the server before importing" : "Import a tar.gz archive"}>
          Import tar.gz{" "}
          <input
            type="file"
            accept=".tar.gz,.tgz,application/gzip"
            onChange={(e) => void onImport(e)}
            disabled={importBlocked || importing}
          />
        </label>
        {importing && <span className="muted">Importing…</span>}
      </div>
      {importBlocked && (
        <p className="warn" role="note">
          Import is disabled while the server desired_state is “{desiredState}”. Stop the
          server before importing an archive.
        </p>
      )}
    </section>
  );
}
