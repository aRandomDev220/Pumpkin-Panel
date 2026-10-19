import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuth } from "../lib/auth";
import {
  checksumShort,
  createBackup,
  createPolicy,
  deleteBackup,
  deletePolicy,
  downloadBackup,
  listBackups,
  listPolicies,
  previewPolicies,
  restoreBackup,
  updatePolicy,
  formatBytes,
  BackupApiError,
} from "../lib/backups";
import type {
  BackupConsistency,
  BackupPolicy,
  BackupPreviewResponse,
} from "../lib/backups";
import { cancelTask, fetchTask, isTerminalTaskState } from "../lib/servers";
import { saveBlob } from "../lib/files";

const UTC_NOTE = "All schedules run in UTC.";

interface BackupsSectionProps {
  serverId: string;
  observedState?: string;
}

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : "Request failed.";
}

export default function BackupsSection({
  serverId,
  observedState,
}: BackupsSectionProps) {
  const { can } = useAuth();
  const canBackup = can("server.backup.create");
  const canRestore = can("server.backup.restore");
  const qc = useQueryClient();
  const stopped = (observedState ?? "").toLowerCase() === "stopped";

  const [consistency, setConsistency] = useState<BackupConsistency>("stopped");
  const [backupTaskId, setBackupTaskId] = useState<string | null>(null);
  const [restoreTaskId, setRestoreTaskId] = useState<string | null>(null);
  const [banner, setBanner] = useState<string | null>(null);
  const [opError, setOpError] = useState<string | null>(null);
  const [restoreArm, setRestoreArm] = useState<string | null>(null);
  const [preRestore, setPreRestore] = useState(true);
  const [downloadingId, setDownloadingId] = useState<string | null>(null);

  // Policy form state.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [pName, setPName] = useState("");
  const [pSchedule, setPSchedule] = useState("");
  const [pTimezone, setPTimezone] = useState("UTC");
  const [pRetentionCount, setPRetentionCount] = useState("7");
  const [pMaxAgeDays, setPMaxAgeDays] = useState("30");
  const [pEnabled, setPEnabled] = useState(true);
  const [policyError, setPolicyError] = useState<string | null>(null);
  const [preview, setPreview] = useState<BackupPreviewResponse | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);

  const backupsQuery = useQuery({
    queryKey: ["servers", serverId, "backups"],
    queryFn: () => listBackups(serverId),
    retry: 1,
  });

  const policiesQuery = useQuery({
    queryKey: ["servers", serverId, "backup-policies"],
    queryFn: () => listPolicies(serverId),
    retry: 1,
  });

  function refetchBackups() {
    qc.invalidateQueries({ queryKey: ["servers", serverId, "backups"] });
  }

  const backupTaskQuery = useQuery({
    queryKey: ["tasks", backupTaskId],
    queryFn: () => fetchTask(backupTaskId ?? ""),
    enabled: backupTaskId !== null && backupTaskId !== "",
    refetchInterval: backupTaskId ? 2_000 : false,
    retry: false,
  });

  const restoreTaskQuery = useQuery({
    queryKey: ["tasks", restoreTaskId],
    queryFn: () => fetchTask(restoreTaskId ?? ""),
    enabled: restoreTaskId !== null && restoreTaskId !== "",
    refetchInterval: restoreTaskId ? 2_000 : false,
    retry: false,
  });

  useEffect(() => {
    const t = backupTaskQuery.data;
    if (!t || !isTerminalTaskState(t.state)) return;
    setBackupTaskId(null);
    refetchBackups();
    if (t.state !== "succeeded") {
      setOpError(t.error ?? `Backup task ended with state: ${t.state}.`);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backupTaskQuery.data]);

  useEffect(() => {
    const t = restoreTaskQuery.data;
    if (!t || !isTerminalTaskState(t.state)) return;
    setRestoreTaskId(null);
    refetchBackups();
    if (t.state !== "succeeded") {
      setOpError(t.error ?? `Restore task ended with state: ${t.state}.`);
    } else {
      setBanner("Restore succeeded.");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [restoreTaskQuery.data]);

  const backupMutation = useMutation({
    mutationFn: (c: BackupConsistency) => createBackup(serverId, c),
    onSuccess: (res) => {
      setOpError(null);
      setBackupTaskId(res.task_id);
    },
    onError: (err) => setOpError(errMsg(err)),
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => deleteBackup(id),
    onSuccess: () => {
      setBanner(null);
      setOpError(null);
      refetchBackups();
    },
    onError: (err) => {
      if (err instanceof BackupApiError && err.status === 409) {
        setBanner(err.message);
      } else {
        setOpError(errMsg(err));
      }
    },
  });

  const restoreMutation = useMutation({
    mutationFn: ({ id, pre }: { id: string; pre: boolean }) =>
      restoreBackup(id, pre),
    onSuccess: (res) => {
      setRestoreArm(null);
      setOpError(null);
      setRestoreTaskId(res.task_id);
    },
    onError: (err) => {
      setRestoreArm(null);
      setOpError(errMsg(err));
    },
  });

  async function onDownload(id: string) {
    setDownloadingId(id);
    setOpError(null);
    try {
      const blob = await downloadBackup(id);
      saveBlob(blob, `backup-${id}.tar.gz`);
    } catch (err) {
      setOpError(errMsg(err));
    } finally {
      setDownloadingId(null);
    }
  }

  function onDelete(id: string, isProtected: boolean) {
    if (
      !window.confirm(
        isProtected
          ? `Delete protected backup ${id}? Protected backups guard the retention floor; deleting may fail with 409.`
          : `Delete backup ${id}? This cannot be undone.`,
      )
    ) {
      return;
    }
    deleteMutation.mutate(id);
  }

  async function onCancelTask(id: string) {
    try {
      await cancelTask(id);
      setBanner("Cancellation requested.");
    } catch (err) {
      setOpError(errMsg(err));
    }
  }

  function onRestoreClick(id: string) {
    if (restoreArm !== id) {
      setRestoreArm(id);
      return;
    }
    if (
      !window.confirm(
        `Restore backup ${id}? Current server data will be replaced.${preRestore ? " A protected pre-restore backup will be created first." : " NO pre-restore backup will be created — current data may be lost."}`,
      )
    ) {
      return;
    }
    restoreMutation.mutate({ id, pre: preRestore });
  }

  function resetPolicyForm() {
    setEditingId(null);
    setPName("");
    setPSchedule("");
    setPTimezone("UTC");
    setPRetentionCount("7");
    setPMaxAgeDays("30");
    setPEnabled(true);
    setPolicyError(null);
    setPreview(null);
    setPreviewError(null);
  }

  function startEdit(p: BackupPolicy) {
    setEditingId(p.id);
    setPName(p.name);
    setPSchedule(p.schedule);
    setPTimezone(p.timezone);
    setPRetentionCount(String(p.retention_count));
    setPMaxAgeDays(String(p.max_age_days));
    setPEnabled(p.enabled);
    setPolicyError(null);
    setPreview(null);
    setPreviewError(null);
  }

  const policyMutation = useMutation({
    mutationFn: async () => {
      const retentionCount = Number(pRetentionCount);
      const maxAgeDays = Number(pMaxAgeDays);
      if (!pSchedule.trim()) throw new Error("Schedule (cron) is required.");
      if (!Number.isInteger(retentionCount) || retentionCount < 0) {
        throw new Error("Retention count must be an integer ≥ 0.");
      }
      if (!Number.isInteger(maxAgeDays) || maxAgeDays < 0) {
        throw new Error("Max age days must be an integer ≥ 0.");
      }
      const input = {
        name: pName.trim(),
        schedule: pSchedule.trim(),
        timezone: pTimezone.trim() === "" ? "UTC" : pTimezone.trim(),
        retention_count: retentionCount,
        max_age_days: maxAgeDays,
        enabled: pEnabled,
      };
      if (editingId) return updatePolicy(editingId, input);
      return createPolicy(serverId, input);
    },
    onSuccess: () => {
      qc.invalidateQueries({
        queryKey: ["servers", serverId, "backup-policies"],
      });
      resetPolicyForm();
    },
    onError: (err) => setPolicyError(errMsg(err)),
  });

  const policyDeleteMutation = useMutation({
    mutationFn: (id: string) => deletePolicy(id),
    onSuccess: () => {
      if (editingId) resetPolicyForm();
      qc.invalidateQueries({
        queryKey: ["servers", serverId, "backup-policies"],
      });
    },
    onError: (err) => setPolicyError(errMsg(err)),
  });

  async function onPreview(e: FormEvent) {
    e.preventDefault();
    setPreview(null);
    setPreviewError(null);
    if (!pSchedule.trim()) {
      setPreviewError("Enter a cron schedule to preview retention.");
      return;
    }
    const retentionCount = Number(pRetentionCount);
    const maxAgeDays = Number(pMaxAgeDays);
    setPreviewBusy(true);
    try {
      const res = await previewPolicies(serverId, {
        schedule: pSchedule.trim(),
        retention_count: Number.isInteger(retentionCount)
          ? retentionCount
          : undefined,
        max_age_days: Number.isInteger(maxAgeDays) ? maxAgeDays : undefined,
      });
      setPreview(res);
    } catch (err) {
      setPreviewError(errMsg(err));
    } finally {
      setPreviewBusy(false);
    }
  }

  const backups = backupsQuery.data?.backups ?? [];
  const policies = policiesQuery.data?.policies ?? [];
  const backupBusy = backupMutation.isPending || backupTaskId !== null;
  const restoreBusy = restoreMutation.isPending || restoreTaskId !== null;

  return (
    <section aria-label="Backups">
      <h3>Backups</h3>
      {banner && (
        <p className="warn" role="status">
          {banner}
        </p>
      )}
      {opError && (
        <p className="err" role="alert">
          {opError}
        </p>
      )}

      <div className="row">
        <label>
          Consistency{" "}
          <select
            value={consistency}
            onChange={(e) =>
              setConsistency(e.target.value as BackupConsistency)
            }
            aria-label="Backup consistency"
            disabled={backupBusy || !canBackup}
          >
            <option value="stopped">stopped</option>
            <option value="live">live</option>
          </select>
        </label>
        <button
          type="button"
          disabled={backupBusy || !canBackup}
          onClick={() => backupMutation.mutate(consistency)}
        >
          {backupTaskId ? "Backing up… polling" : "Run full backup"}
        </button>
      </div>
      {!canBackup && (
        <p className="muted" role="note">
          Your role can download backups but cannot create, delete, or restore
          them.
        </p>
      )}
      {consistency === "live" && (
        <p className="warn" role="note">
          Live backups capture a running server and may be inconsistent — files
          can change mid-snapshot. Prefer “stopped” for a guaranteed-consistent
          backup.
        </p>
      )}
      {backupTaskQuery.data && backupTaskId && (
        <p className="muted" role="status">
          Backup task {backupTaskQuery.data.id}:{" "}
          <span className="badge">{backupTaskQuery.data.state}</span>{" "}
          {!isTerminalTaskState(backupTaskQuery.data.state) &&
            backupTaskQuery.data.state !== "cancel_requested" && (
              <button
                type="button"
                onClick={() => void onCancelTask(backupTaskQuery.data.id)}
              >
                Cancel
              </button>
            )}
        </p>
      )}
      {backupTaskQuery.error && (
        <p className="err" role="alert">
          Failed to poll backup task status.
        </p>
      )}

      {backupsQuery.isLoading && <p>Loading backups…</p>}
      {backupsQuery.error && (
        <p className="err" role="alert">
          Failed to load backups.
        </p>
      )}
      {backupsQuery.data && backups.length === 0 && (
        <p className="muted">No backups yet.</p>
      )}
      {backups.length > 0 && (
        <div className="table-scroll">
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Created</th>
              <th scope="col">Size</th>
              <th scope="col">Checksum</th>
              <th scope="col">Consistency</th>
              <th scope="col">State</th>
              <th scope="col">Errors</th>
              <th scope="col">Actions</th>
            </tr>
          </thead>
          <tbody>
            {backups.map((b) => (
              <tr key={b.id}>
                <td title={b.completed_at ?? undefined}>{b.created_at}</td>
                <td>{formatBytes(b.size_bytes)}</td>
                <td title={b.checksum || undefined}>
                  {b.protected ? "🔒 " : ""}
                  {checksumShort(b.checksum)}
                </td>
                <td>{b.consistency_method}</td>
                <td>
                  <span className="badge">{b.state}</span>
                </td>
                <td>{b.error_summary ?? <span className="muted">—</span>}</td>
                <td>
                  <div className="row">
                    <button
                      type="button"
                      disabled={downloadingId === b.id}
                      onClick={() => onDownload(b.id)}
                    >
                      {downloadingId === b.id ? "…" : "Download"}
                    </button>
                    <button
                      type="button"
                      disabled={deleteMutation.isPending || !canBackup}
                      onClick={() => onDelete(b.id, b.protected)}
                    >
                      Delete
                    </button>
                    <button
                      type="button"
                      disabled={!stopped || restoreBusy || !canRestore}
                      title={
                        stopped
                          ? restoreArm === b.id
                            ? "Click again, then confirm the dialog, to restore"
                            : "Restore (double-confirmed)"
                          : `Restore requires a stopped server (observed: ${observedState ?? "unknown"}). Stop the server first.`
                      }
                      onClick={() => onRestoreClick(b.id)}
                    >
                      {restoreArm === b.id ? "Confirm restore" : "Restore"}
                    </button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      )}
      {!stopped && (
        <p className="muted" role="note">
          Restore is disabled while the server is not stopped (observed:{" "}
          {observedState ?? "unknown"}). Stop the server to enable restore.
        </p>
      )}
      <p className="muted" role="note">
        Restoring replaces current server data. A protected pre-restore backup
        is created automatically before restoring.
      </p>
      <label className="row">
        <input
          type="checkbox"
          checked={preRestore}
          onChange={(e) => setPreRestore(e.target.checked)}
        />
        Create protected pre-restore backup (recommended)
      </label>
      {restoreTaskQuery.data && restoreTaskId && (
        <p className="muted" role="status">
          Restore task {restoreTaskQuery.data.id}:{" "}
          <span className="badge">{restoreTaskQuery.data.state}</span>{" "}
          {!isTerminalTaskState(restoreTaskQuery.data.state) &&
            restoreTaskQuery.data.state !== "cancel_requested" && (
              <button
                type="button"
                onClick={() => void onCancelTask(restoreTaskQuery.data.id)}
              >
                Cancel
              </button>
            )}
        </p>
      )}
      {restoreTaskQuery.error && (
        <p className="err" role="alert">
          Failed to poll restore task status.
        </p>
      )}

      <h3>Backup policies</h3>
      <p className="muted" role="note">
        {UTC_NOTE}
      </p>
      {policiesQuery.isLoading && <p>Loading policies…</p>}
      {policiesQuery.error && (
        <p className="err" role="alert">
          Failed to load backup policies.
        </p>
      )}
      {policies.length > 0 && (
        <div className="table-scroll">
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Schedule</th>
              <th scope="col">Retention</th>
              <th scope="col">Enabled</th>
              <th scope="col">Next run</th>
              <th scope="col">Actions</th>
            </tr>
          </thead>
          <tbody>
            {policies.map((p) => (
              <tr key={p.id}>
                <td>{p.name || <span className="muted">—</span>}</td>
                <td title={p.timezone_note ?? undefined}>
                  <code>{p.schedule}</code>{" "}
                  <span className="muted">({p.timezone})</span>
                </td>
                <td>
                  keep {p.retention_count} / {p.max_age_days}d
                </td>
                <td>{p.enabled ? "on" : "off"}</td>
                <td>{p.next_run_at ?? <span className="muted">—</span>}</td>
                <td>
                  <div className="row">
                    <button type="button" onClick={() => startEdit(p)}>
                      Edit
                    </button>
                    <button
                      type="button"
                      disabled={policyDeleteMutation.isPending}
                      onClick={() => {
                        if (
                          window.confirm(
                            `Delete backup policy “${p.name || p.id}”?`,
                          )
                        ) {
                          policyDeleteMutation.mutate(p.id);
                        }
                      }}
                    >
                      Delete
                    </button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      )}
      {policiesQuery.data && policies.length === 0 && (
        <p className="muted">No backup policies yet.</p>
      )}

      <h4>{editingId ? "Edit policy" : "New policy"}</h4>
      {policyError && (
        <p className="err" role="alert">
          {policyError}
        </p>
      )}
      <form
        onSubmit={(e: FormEvent) => {
          e.preventDefault();
          setPolicyError(null);
          policyMutation.mutate();
        }}
      >
        <div className="row">
          <label>
            Name{" "}
            <input
              value={pName}
              onChange={(e) => setPName(e.target.value)}
              placeholder="nightly"
            />
          </label>
          <label>
            Schedule (cron){" "}
            <input
              value={pSchedule}
              onChange={(e) => setPSchedule(e.target.value)}
              placeholder="0 2 * * *"
              aria-label="Schedule (cron)"
            />
          </label>
          <label>
            Timezone{" "}
            <input
              value={pTimezone}
              onChange={(e) => setPTimezone(e.target.value)}
              placeholder="UTC"
            />
          </label>
        </div>
        <div className="row">
          <label>
            Retention count{" "}
            <input
              value={pRetentionCount}
              onChange={(e) => setPRetentionCount(e.target.value)}
              inputMode="numeric"
            />
          </label>
          <label>
            Max age (days){" "}
            <input
              value={pMaxAgeDays}
              onChange={(e) => setPMaxAgeDays(e.target.value)}
              inputMode="numeric"
            />
          </label>
          <label className="row">
            <input
              type="checkbox"
              checked={pEnabled}
              onChange={(e) => setPEnabled(e.target.checked)}
            />
            Enabled
          </label>
        </div>
        <div className="row">
          <button type="submit" disabled={policyMutation.isPending}>
            {editingId ? "Save policy" : "Create policy"}
          </button>
          {editingId && (
            <button type="button" onClick={resetPolicyForm}>
              Cancel
            </button>
          )}
          <button
            type="button"
            disabled={previewBusy}
            onClick={onPreview}
            title="Dry-run retention against current backups"
          >
            {previewBusy ? "Previewing…" : "Preview retention"}
          </button>
        </div>
      </form>
      {previewError && (
        <p className="err" role="alert">
          {previewError}
        </p>
      )}
      {preview && (
        <p className="muted" role="status">
          Preview: would delete {preview.would_delete.length} backup
          {preview.would_delete.length === 1 ? "" : "s"}
          {preview.would_delete.length > 0 &&
            ` (${preview.would_delete.join(", ")})`}
          ; {preview.retained} retained; {preview.protected_kept} protected
          kept.
        </p>
      )}
    </section>
  );
}
