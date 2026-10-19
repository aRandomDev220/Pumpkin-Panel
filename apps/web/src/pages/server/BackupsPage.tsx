import { useState } from "react";
import {
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useOutletContext } from "react-router-dom";
import type { ServerOutletContext } from "../../components/server/ServerLayout";
import TaskProgress from "../../components/server/TaskProgress";
import Badge from "../../components/ui/Badge";
import Button from "../../components/ui/Button";
import ConfirmDialog from "../../components/ui/ConfirmDialog";
import Dialog from "../../components/ui/Dialog";
import EmptyState from "../../components/ui/EmptyState";
import ErrorState from "../../components/ui/ErrorState";
import Field, {
  Checkbox,
  Input,
  Select,
} from "../../components/ui/Field";
import { SkeletonTable } from "../../components/ui/Skeleton";
import { useToast } from "../../components/ui/toast-context";
import {
  isTaskInFlight,
  useTaskPolling,
} from "../../hooks/useTaskPolling";
import type { LifecycleController } from "../../hooks/useLifecycle";
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
} from "../../lib/backups";
import type {
  Backup,
  BackupConsistency,
  BackupPolicy,
  BackupPreviewResponse,
} from "../../lib/backups";
import { humanizeError } from "../../lib/errors";
import { saveBlob } from "../../lib/files";
import { formatBytes, formatDateTime } from "../../lib/format";

const UTC_NOTE = "All schedules run in UTC.";

function StopServerButton({
  lifecycle,
  serverName,
}: {
  lifecycle: LifecycleController;
  serverName: string;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);

  return (
    <>
      <Button
        size="sm"
        variant="danger"
        disabled={lifecycle.isBusy}
        onClick={() => setOpen(true)}
      >
        Stop server
      </Button>
      <ConfirmDialog
        open={open}
        onClose={() => setOpen(false)}
        onConfirm={() => {
          lifecycle.run("stop");
          setOpen(false);
        }}
        title={`Stop ${serverName}?`}
        description="Connected players will be disconnected. The world is saved before the server stops."
        confirmLabel="Stop server"
        tone="danger"
        busy={lifecycle.isBusy}
      />
    </>
  );
}

export default function BackupsPage(): React.JSX.Element {
  const { server, status, lifecycle } =
    useOutletContext<ServerOutletContext>();
  const queryClient = useQueryClient();
  const toast = useToast();

  const [consistency, setConsistency] =
    useState<BackupConsistency>("stopped");
  const [backupTaskId, setBackupTaskId] = useState<string | null>(null);
  const [restoreTaskId, setRestoreTaskId] = useState<string | null>(null);
  const [opError, setOpError] = useState<string | null>(null);
  const [downloadingId, setDownloadingId] = useState<string | null>(null);

  const [restoreTarget, setRestoreTarget] = useState<Backup | null>(null);
  const [preRestore, setPreRestore] = useState(true);
  const [restoreError, setRestoreError] = useState<string | null>(null);

  const [deleteTarget, setDeleteTarget] = useState<Backup | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const [policyOpen, setPolicyOpen] = useState(false);
  const [editingPolicy, setEditingPolicy] =
    useState<BackupPolicy | null>(null);
  const [pName, setPName] = useState("");
  const [pSchedule, setPSchedule] = useState("");
  const [pTimezone, setPTimezone] = useState("UTC");
  const [pRetentionCount, setPRetentionCount] = useState("7");
  const [pMaxAgeDays, setPMaxAgeDays] = useState("30");
  const [pEnabled, setPEnabled] = useState(true);
  const [policyError, setPolicyError] = useState<string | null>(null);
  const [policyPreview, setPolicyPreview] =
    useState<BackupPreviewResponse | null>(null);
  const [policyPreviewError, setPolicyPreviewError] =
    useState<string | null>(null);
  const [policyPreviewBusy, setPolicyPreviewBusy] = useState(false);
  const [policyDeleteTarget, setPolicyDeleteTarget] =
    useState<BackupPolicy | null>(null);
  const [policyDeleteError, setPolicyDeleteError] =
    useState<string | null>(null);

  const backupsQuery = useQuery({
    queryKey: ["servers", server.id, "backups"],
    queryFn: () => listBackups(server.id),
    retry: 1,
  });
  const policiesQuery = useQuery({
    queryKey: ["servers", server.id, "backup-policies"],
    queryFn: () => listPolicies(server.id),
    retry: 1,
  });

  const backups = backupsQuery.data?.backups ?? [];
  const policies = policiesQuery.data?.policies ?? [];
  const observed = status?.readiness?.state ?? server.observed_state;
  const stopped = observed.toLowerCase() === "stopped";

  function refetchBackups(): void {
    void queryClient.invalidateQueries({
      queryKey: ["servers", server.id, "backups"],
    });
  }

  const backupTask = useTaskPolling(backupTaskId, (task) => {
    refetchBackups();
    if (task.state === "succeeded") {
      toast.success("Backup complete");
    } else {
      toast.error(task.error ?? `Backup failed (${task.state}).`);
    }
  });
  const restoreTask = useTaskPolling(restoreTaskId, (task) => {
    refetchBackups();
    if (task.state === "succeeded") {
      toast.success("Restore complete");
    } else {
      toast.error(task.error ?? `Restore failed (${task.state}).`);
    }
  });

  const backupBusy =
    backupTask.isLoading || isTaskInFlight(backupTask.task);
  const restoreBusy =
    isTaskInFlight(restoreTask.task) || restoreTask.isLoading;

  const backupMutation = useMutation({
    mutationFn: () => createBackup(server.id, consistency),
    onSuccess: (result) => {
      setOpError(null);
      setBackupTaskId(result.task_id);
      toast.info("Backup started");
    },
    onError: (error) => setOpError(humanizeError(error)),
  });

  const restoreMutation = useMutation({
    mutationFn: ({
      id,
      pre,
    }: {
      id: string;
      pre: boolean;
    }) => restoreBackup(id, pre),
    onSuccess: (result) => {
      setRestoreTarget(null);
      setRestoreError(null);
      setRestoreTaskId(result.task_id);
      setOpError(null);
      toast.info("Restore started");
    },
    onError: (error) => setRestoreError(humanizeError(error)),
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => deleteBackup(id),
    onSuccess: () => {
      refetchBackups();
      setDeleteTarget(null);
      setDeleteError(null);
      toast.success("Backup deleted");
    },
    onError: (error) => setDeleteError(humanizeError(error)),
  });

  async function onDownload(backup: Backup): Promise<void> {
    setDownloadingId(backup.id);
    setOpError(null);
    try {
      const blob = await downloadBackup(backup.id);
      saveBlob(blob, `backup-${backup.id}.tar.gz`);
      toast.success("Backup downloaded");
    } catch (error) {
      setOpError(humanizeError(error));
    } finally {
      setDownloadingId(null);
    }
  }

  function resetPolicyForm(): void {
    setEditingPolicy(null);
    setPName("");
    setPSchedule("");
    setPTimezone("UTC");
    setPRetentionCount("7");
    setPMaxAgeDays("30");
    setPEnabled(true);
    setPolicyError(null);
    setPolicyPreview(null);
    setPolicyPreviewError(null);
  }

  function openCreatePolicy(): void {
    resetPolicyForm();
    setPolicyOpen(true);
  }

  function openEditPolicy(policy: BackupPolicy): void {
    setEditingPolicy(policy);
    setPName(policy.name);
    setPSchedule(policy.schedule);
    setPTimezone(policy.timezone);
    setPRetentionCount(String(policy.retention_count));
    setPMaxAgeDays(String(policy.max_age_days));
    setPEnabled(policy.enabled);
    setPolicyError(null);
    setPolicyPreview(null);
    setPolicyPreviewError(null);
    setPolicyOpen(true);
  }

  const policyMutation = useMutation({
    mutationFn: async () => {
      const retentionCount = Number(pRetentionCount);
      const maxAgeDays = Number(pMaxAgeDays);
      if (pSchedule.trim() === "") {
        throw new Error("Schedule (cron) is required.");
      }
      if (
        !Number.isInteger(retentionCount) ||
        retentionCount < 0
      ) {
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
      if (editingPolicy !== null) {
        return updatePolicy(editingPolicy.id, input);
      }
      return createPolicy(server.id, input);
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: ["servers", server.id, "backup-policies"],
      });
      toast.success(
        editingPolicy !== null ? "Policy saved" : "Policy created",
      );
      setPolicyOpen(false);
      resetPolicyForm();
    },
    onError: (error) => setPolicyError(humanizeError(error)),
  });

  function closePolicy(): void {
    if (policyMutation.isPending) return;
    setPolicyOpen(false);
    resetPolicyForm();
  }

  const policyDeleteMutation = useMutation({
    mutationFn: (id: string) => deletePolicy(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({
        queryKey: ["servers", server.id, "backup-policies"],
      });
      toast.success("Policy deleted");
      setPolicyDeleteTarget(null);
      setPolicyDeleteError(null);
    },
    onError: (error) =>
      setPolicyDeleteError(humanizeError(error)),
  });

  async function onPreviewPolicy(): Promise<void> {
    setPolicyPreview(null);
    setPolicyPreviewError(null);
    if (pSchedule.trim() === "") {
      setPolicyPreviewError(
        "Enter a cron schedule to preview retention.",
      );
      return;
    }
    const retentionCount = Number(pRetentionCount);
    const maxAgeDays = Number(pMaxAgeDays);
    setPolicyPreviewBusy(true);
    try {
      setPolicyPreview(
        await previewPolicies(server.id, {
          schedule: pSchedule.trim(),
          retention_count: Number.isInteger(retentionCount)
            ? retentionCount
            : undefined,
          max_age_days: Number.isInteger(maxAgeDays)
            ? maxAgeDays
            : undefined,
        }),
      );
    } catch (error) {
      setPolicyPreviewError(humanizeError(error));
    } finally {
      setPolicyPreviewBusy(false);
    }
  }

  return (
    <section>
      <div className="section-heading">
        <h2>Backups</h2>
        <span className="muted">
          {backups.length} snapshot{backups.length === 1 ? "" : "s"}
        </span>
      </div>

      <div className="backups-toolbar">
        <Field
          label="Consistency"
          hint="Stop the server for a guaranteed-consistent snapshot."
        >
          <Select
            value={consistency}
            onChange={(event) =>
              setConsistency(
                event.target.value as BackupConsistency,
              )
            }
            disabled={backupBusy}
          >
            <option value="stopped">Stopped server</option>
            <option value="live">
              Live server (may be inconsistent)
            </option>
          </Select>
        </Field>
        <Button
          onClick={() => backupMutation.mutate()}
          loading={backupMutation.isPending || backupBusy}
        >
          Run full backup
        </Button>
      </div>

      {consistency === "live" && (
        <p className="warn" role="note">
          Live backups may capture files while they are changing.
          Prefer a stopped server for a consistent snapshot.
        </p>
      )}
      {opError !== null && (
        <p className="err" role="alert">
          {opError}
        </p>
      )}
      <TaskProgress task={backupTask.task} label="Backup" />
      <TaskProgress task={restoreTask.task} label="Restore" />

      {!stopped && (
        <div className="notice-warning">
          <p>
            Restore requires a stopped server. Current state:{" "}
            <strong>{observed}</strong>.
          </p>
          <StopServerButton
            lifecycle={lifecycle}
            serverName={server.name}
          />
        </div>
      )}

      {backupsQuery.isLoading && <SkeletonTable />}
      {backupsQuery.error && (
        <ErrorState
          message="Could not load backups."
          technicalDetails={
            backupsQuery.error instanceof Error
              ? backupsQuery.error.message
              : undefined
          }
          onRetry={() => void backupsQuery.refetch()}
        />
      )}
      {!backupsQuery.isLoading &&
        !backupsQuery.error &&
        backups.length === 0 && (
          <EmptyState
            title="No backups yet"
            description="Run a full backup before updating the image or making a risky configuration change."
          />
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
                <th scope="col">Protection</th>
                <th scope="col">Actions</th>
              </tr>
            </thead>
            <tbody>
              {backups.map((backup) => (
                <tr key={backup.id}>
                  <td title={backup.created_at}>
                    {formatDateTime(backup.created_at)}
                  </td>
                  <td>{formatBytes(backup.size_bytes)}</td>
                  <td title={backup.checksum || undefined}>
                    <code>{checksumShort(backup.checksum)}</code>
                  </td>
                  <td>{backup.consistency_method}</td>
                  <td>
                    <Badge
                      tone={
                        backup.state === "succeeded"
                          ? "success"
                          : backup.state === "failed"
                            ? "danger"
                            : "neutral"
                      }
                    >
                      {backup.state}
                    </Badge>
                  </td>
                  <td>
                    <Badge
                      tone={
                        backup.protected ? "success" : "neutral"
                      }
                    >
                      {backup.protected ? "Protected" : "Standard"}
                    </Badge>
                  </td>
                  <td>
                    <div className="backup-actions">
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={downloadingId === backup.id}
                        onClick={() => void onDownload(backup)}
                      >
                        {downloadingId === backup.id
                          ? "Downloading…"
                          : "Download"}
                      </Button>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={!stopped || restoreBusy}
                        title={
                          stopped
                            ? "Restore this backup"
                            : `Restore requires a stopped server (current: ${observed}).`
                        }
                        onClick={() => {
                          setRestoreTarget(backup);
                          setPreRestore(true);
                          setRestoreError(null);
                        }}
                      >
                        Restore
                      </Button>
                      <Button
                        size="sm"
                        variant="danger"
                        onClick={() => {
                          setDeleteTarget(backup);
                          setDeleteError(null);
                        }}
                      >
                        Delete
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <div className="section-heading">
        <h2>Backup policies</h2>
        {policies.length > 0 && (
          <Button size="sm" onClick={openCreatePolicy}>
            New policy
          </Button>
        )}
      </div>
      <p className="muted" role="note">
        {UTC_NOTE} The last verified backup is never deleted.
      </p>

      {policiesQuery.isLoading && <SkeletonTable />}
      {policiesQuery.error && (
        <ErrorState
          message="Could not load backup policies."
          technicalDetails={
            policiesQuery.error instanceof Error
              ? policiesQuery.error.message
              : undefined
          }
          onRetry={() => void policiesQuery.refetch()}
        />
      )}
      {!policiesQuery.isLoading &&
        !policiesQuery.error &&
        policies.length === 0 && (
          <EmptyState
            title="No backup policies yet"
            description="Create a schedule to take backups automatically."
            action={
              <Button onClick={openCreatePolicy}>
                Create a policy
              </Button>
            }
          />
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
              {policies.map((policy) => (
                <tr key={policy.id}>
                  <td>{policy.name || "—"}</td>
                  <td title={policy.timezone_note ?? undefined}>
                    <code>{policy.schedule}</code>{" "}
                    <span className="muted">
                      ({policy.timezone})
                    </span>
                  </td>
                  <td>
                    keep {policy.retention_count} /{" "}
                    {policy.max_age_days}d
                  </td>
                  <td>
                    <Badge
                      tone={policy.enabled ? "success" : "neutral"}
                    >
                      {policy.enabled ? "On" : "Off"}
                    </Badge>
                  </td>
                  <td>{formatDateTime(policy.next_run_at)}</td>
                  <td>
                    <div className="policy-actions">
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={() => openEditPolicy(policy)}
                      >
                        Edit
                      </Button>
                      <Button
                        size="sm"
                        variant="danger"
                        onClick={() => {
                          setPolicyDeleteTarget(policy);
                          setPolicyDeleteError(null);
                        }}
                      >
                        Delete
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <ConfirmDialog
        open={restoreTarget !== null}
        onClose={() => {
          if (restoreMutation.isPending) return;
          setRestoreTarget(null);
          setRestoreError(null);
        }}
        onConfirm={() => {
          if (restoreTarget !== null) {
            restoreMutation.mutate({
              id: restoreTarget.id,
              pre: preRestore,
            });
          }
        }}
        title="Restore backup"
        description={
          restoreTarget !== null
            ? `Restore backup ${restoreTarget.id}? Current server data will be replaced.`
            : undefined
        }
        confirmLabel="Restore backup"
        busy={restoreMutation.isPending}
        checkbox={{
          label: "Create a protected pre-restore backup",
          hint: "Recommended. The current world is snapshotted first.",
          checked: preRestore,
          onChange: setPreRestore,
        }}
      >
        {restoreError !== null && (
          <p className="err" role="alert">
            {restoreError}
          </p>
        )}
      </ConfirmDialog>

      <ConfirmDialog
        open={deleteTarget !== null}
        onClose={() => {
          if (deleteMutation.isPending) return;
          setDeleteTarget(null);
          setDeleteError(null);
        }}
        onConfirm={() => {
          if (deleteTarget !== null) {
            deleteMutation.mutate(deleteTarget.id);
          }
        }}
        title="Delete backup"
        description={
          deleteTarget !== null
            ? deleteTarget.protected
              ? "This backup is protected by a retention policy. Deleting it may be refused."
              : "This backup will be removed. This cannot be undone."
            : undefined
        }
        confirmLabel="Delete backup"
        busy={deleteMutation.isPending}
      >
        {deleteError !== null && (
          <p className="err" role="alert">
            {deleteError}
          </p>
        )}
      </ConfirmDialog>

      <Dialog
        open={policyOpen}
        onClose={closePolicy}
        title={
          editingPolicy !== null
            ? "Edit backup policy"
            : "New backup policy"
        }
        description="Schedules use cron. Retention keeps recent snapshots and removes older ones."
        wide
      >
        <form
          onSubmit={(event) => {
            event.preventDefault();
            setPolicyError(null);
            policyMutation.mutate();
          }}
        >
          <div className="form-grid">
            <Field label="Name">
              <Input
                value={pName}
                onChange={(event) => setPName(event.target.value)}
                placeholder="nightly"
              />
            </Field>
            <Field
              label="Schedule (cron)"
              hint="Example: 0 2 * * * runs daily at 02:00."
            >
              <Input
                value={pSchedule}
                onChange={(event) =>
                  setPSchedule(event.target.value)
                }
                placeholder="0 2 * * *"
              />
            </Field>
            <Field label="Timezone">
              <Input
                value={pTimezone}
                onChange={(event) =>
                  setPTimezone(event.target.value)
                }
                placeholder="UTC"
              />
            </Field>
          </div>

          <div className="form-grid">
            <Field label="Retention count">
              <Input
                value={pRetentionCount}
                onChange={(event) =>
                  setPRetentionCount(event.target.value)
                }
                inputMode="numeric"
              />
            </Field>
            <Field label="Max age (days)">
              <Input
                value={pMaxAgeDays}
                onChange={(event) =>
                  setPMaxAgeDays(event.target.value)
                }
                inputMode="numeric"
              />
            </Field>
          </div>

          <Checkbox
            label="Enabled"
            hint="Disabled policies keep their settings but do not run."
            checked={pEnabled}
            onChange={(event) => setPEnabled(event.target.checked)}
          />

          {policyError !== null && (
            <p className="err" role="alert">
              {policyError}
            </p>
          )}
          {policyPreviewError !== null && (
            <p className="err" role="alert">
              {policyPreviewError}
            </p>
          )}
          {policyPreview !== null && (
            <p className="muted" role="status">
              Preview: would delete{" "}
              {policyPreview.would_delete.length} backup
              {policyPreview.would_delete.length === 1 ? "" : "s"}
              {policyPreview.would_delete.length > 0 &&
                ` (${policyPreview.would_delete.join(", ")})`}
              ; {policyPreview.retained} retained;{" "}
              {policyPreview.protected_kept} protected kept.
            </p>
          )}

          <div className="dialog-actions">
            <Button
              type="button"
              variant="secondary"
              onClick={() => void onPreviewPolicy()}
              loading={policyPreviewBusy}
            >
              Preview retention
            </Button>
            <Button
              type="button"
              variant="secondary"
              onClick={closePolicy}
              disabled={policyMutation.isPending}
            >
              Cancel
            </Button>
            <Button
              type="submit"
              loading={policyMutation.isPending}
            >
              {editingPolicy !== null
                ? "Save policy"
                : "Create policy"}
            </Button>
          </div>
        </form>
      </Dialog>

      <ConfirmDialog
        open={policyDeleteTarget !== null}
        onClose={() => {
          if (policyDeleteMutation.isPending) return;
          setPolicyDeleteTarget(null);
          setPolicyDeleteError(null);
        }}
        onConfirm={() => {
          if (policyDeleteTarget !== null) {
            policyDeleteMutation.mutate(policyDeleteTarget.id);
          }
        }}
        title="Delete backup policy"
        description={
          policyDeleteTarget !== null
            ? `Delete “${policyDeleteTarget.name || policyDeleteTarget.id}”? Scheduled backups for this policy stop running.`
            : undefined
        }
        confirmLabel="Delete policy"
        busy={policyDeleteMutation.isPending}
      >
        {policyDeleteError !== null && (
          <p className="err" role="alert">
            {policyDeleteError}
          </p>
        )}
      </ConfirmDialog>
    </section>
  );
}
