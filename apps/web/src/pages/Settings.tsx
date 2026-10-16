import { useState } from "react";
import type { FormEvent } from "react";
import { useMutation } from "@tanstack/react-query";
import {
  downloadDatabaseBackup,
  errMsg,
  restoreDatabase,
} from "../lib/admin";
import { saveBlob } from "../lib/files";

export default function Settings() {
  const [downloading, setDownloading] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const [file, setFile] = useState<File | null>(null);
  const [staged, setStaged] = useState(false);
  const [restoreError, setRestoreError] = useState<string | null>(null);

  async function onDownload() {
    setDownloading(true);
    setDownloadError(null);
    try {
      const blob = await downloadDatabaseBackup();
      const stamp = new Date().toISOString().replace(/[:.]/g, "-");
      saveBlob(blob, `pumpkin-manager-backup-${stamp}.db`);
    } catch (err) {
      setDownloadError(errMsg(err));
    } finally {
      setDownloading(false);
    }
  }

  const restoreMutation = useMutation({
    mutationFn: (f: File) => restoreDatabase(f),
    onSuccess: (res) => {
      setRestoreError(null);
      setStaged(res.staged);
      setFile(null);
    },
    onError: (err) => {
      setRestoreError(errMsg(err));
      setStaged(false);
    },
  });

  function onStage(e: FormEvent) {
    e.preventDefault();
    setRestoreError(null);
    if (!file) {
      setRestoreError("Choose a .db file to stage.");
      return;
    }
    if (
      !window.confirm(
        "Stage this database file for restore? It takes effect on the next " +
          "manager restart, replacing the live database. The current database " +
          "is kept aside as a .pre-restore copy.",
      )
    ) {
      return;
    }
    restoreMutation.mutate(file);
  }

  return (
    <section>
      <h2>Settings</h2>
      <p className="muted">
        Manager database — the panel&apos;s own data (users, servers,
        schedules), not server worlds. For world backups, see a server&apos;s
        Backups section.
      </p>

      <div className="card">
        <h3 style={{ marginTop: 0 }}>Manager database</h3>
        <p className="muted">
          Procedure: download a snapshot → stage a restore file → restart the
          manager to apply it. A pre-restore copy of the live database is kept
          automatically. Full procedure:{" "}
          <code>docs/backup-restore.md</code>.
        </p>
        {downloadError && (
          <p className="err" role="alert">
            {downloadError}
          </p>
        )}
        <div className="row">
          <button type="button" disabled={downloading} onClick={onDownload}>
            {downloading ? "Preparing…" : "Download backup"}
          </button>
          <span className="muted">
            Consistent snapshot (.db) of the live database.
          </span>
        </div>
      </div>

      <div className="card" style={{ marginTop: 12 }}>
        <h3 style={{ marginTop: 0 }}>Staged restore</h3>
        <p className="muted">
          Upload a database file to stage it. Nothing changes until the manager
          restarts — the live data is untouched while staged. The upload is
          validated (SQLite format, supported schema, required tables) before
          it is accepted.
        </p>
        {restoreError && (
          <p className="err" role="alert">
            {restoreError}
          </p>
        )}
        {staged && (
          <p className="warn" role="status">
            Restore staged — restart the manager to apply it. Live data is
            untouched until restart.
          </p>
        )}
        <form onSubmit={onStage}>
          <div className="row">
            <label>
              Database file{" "}
              <input
                type="file"
                accept=".db,.sqlite,.sqlite3,application/octet-stream"
                aria-label="Database file to stage"
                onChange={(e) => setFile(e.target.files?.[0] ?? null)}
              />
            </label>
            <button type="submit" disabled={restoreMutation.isPending || !file}>
              {restoreMutation.isPending ? "Staging…" : "Stage restore"}
            </button>
          </div>
        </form>
      </div>

      <div
        style={{
          background: "#1c1214",
          border: "1px solid #8a2f2f",
          borderRadius: 8,
          padding: "12px 16px",
          marginTop: 12,
        }}
      >
        <h3 style={{ marginTop: 0 }}>Danger zone</h3>
        <p className="muted">
          Restoring replaces the live manager database on the next restart.
          Download a backup first and keep it somewhere safe. If the manager
          fails to start after a restore, the automatic pre-restore copy next
          to the database file can be swapped back — see{" "}
          <code>docs/backup-restore.md</code>.
        </p>
      </div>
    </section>
  );
}
