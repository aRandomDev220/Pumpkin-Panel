import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import {
  fetchServerLogs,
  fetchServerSettings,
  fetchServerTasks,
  fetchStatus,
  fetchTask,
  getServer,
  isLifecycleAccepted,
  isTerminalTaskState,
  postLifecycle,
  updateServerSettings,
} from "../lib/servers";
import type { LifecycleOp, TaskStatus } from "../lib/servers";
import {
  METRICS_RANGES,
  fetchMetricsHistory,
  isMetricsRange,
} from "../lib/monitoring";
import type { MetricsRange } from "../lib/monitoring";
import { ServerHistoryPanels } from "./Monitoring";
import BackupsSection from "./Backups";
import {
  createScheduledRestart,
  deleteScheduledRestart,
  listScheduledRestarts,
} from "../lib/schedules";
import type { ScheduledRestart } from "../lib/schedules";
import {
  AdminApiError,
  errMsg,
  previousImageOf,
  previewUpdate,
  requestRollback,
  requestUpdate,
} from "../lib/admin";
import type { UpdatePreview } from "../lib/admin";
import { useAuth } from "../lib/auth";

const LOG_TAIL_OPTIONS = [50, 200, 500];

const ROLLBACK_ONE_WAY =
  "Rollback is one-way for data: world files written by the new version may " +
  "not load on the old image. Keep a backup before rolling back.";

interface TaskProgress {
  state?: string;
  progress_percent?: number | null;
  message?: string | null;
  error?: string | null;
}

function taskProgress(t: TaskStatus): TaskProgress {
  return t as unknown as TaskProgress;
}

function UpdateSection({
  serverId,
  server,
}: {
  serverId: string;
  server: object;
}) {
  const qc = useQueryClient();
  const { can } = useAuth();
  const canUpdate = can("server.update");
  const [target, setTarget] = useState("");
  const [digest, setDigest] = useState("");
  const [preview, setPreview] = useState<UpdatePreview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [noBackup, setNoBackup] = useState(false);
  const [updateTaskId, setUpdateTaskId] = useState<string | null>(null);
  const [rollbackError, setRollbackError] = useState<string | null>(null);
  const [rollbackTaskId, setRollbackTaskId] = useState<string | null>(null);

  const current = (server as { image_reference?: string }).image_reference ?? "";
  const currentDigest = (server as { image_digest?: unknown }).image_digest;
  const previous = previousImageOf(server);
  const previousDigest = (server as { previous_image_digest?: unknown })
    .previous_image_digest;

  const updateTaskQuery = useQuery<TaskStatus>({
    queryKey: ["tasks", updateTaskId],
    queryFn: () => fetchTask(updateTaskId ?? ""),
    enabled: updateTaskId !== null && updateTaskId !== "",
    refetchInterval: updateTaskId ? 2_000 : false,
    retry: false,
  });

  const rollbackTaskQuery = useQuery<TaskStatus>({
    queryKey: ["tasks", rollbackTaskId],
    queryFn: () => fetchTask(rollbackTaskId ?? ""),
    enabled: rollbackTaskId !== null && rollbackTaskId !== "",
    refetchInterval: rollbackTaskId ? 2_000 : false,
    retry: false,
  });

  useEffect(() => {
    const t = updateTaskQuery.data;
    if (!t || !isTerminalTaskState(t.state)) return;
    setUpdateTaskId(null);
    qc.invalidateQueries({ queryKey: ["servers", serverId] });
    qc.invalidateQueries({ queryKey: ["servers", serverId, "tasks"] });
  }, [updateTaskQuery.data, serverId, qc]);

  useEffect(() => {
    const t = rollbackTaskQuery.data;
    if (!t || !isTerminalTaskState(t.state)) return;
    setRollbackTaskId(null);
    qc.invalidateQueries({ queryKey: ["servers", serverId] });
    qc.invalidateQueries({ queryKey: ["servers", serverId, "tasks"] });
  }, [rollbackTaskQuery.data, serverId, qc]);

  async function onPreview() {
    setPreview(null);
    setPreviewError(null);
    setNoBackup(false);
    if (!target.trim()) {
      setPreviewError("Enter a target image reference first.");
      return;
    }
    setPreviewBusy(true);
    try {
      setPreview(await previewUpdate(serverId, target.trim(), digest));
    } catch (err) {
      setPreviewError(errMsg(err));
    } finally {
      setPreviewBusy(false);
    }
  }

  const updateMutation = useMutation({
    mutationFn: () => requestUpdate(serverId, target.trim(), digest),
    onSuccess: (res) => {
      setUpdateError(null);
      setNoBackup(false);
      setUpdateTaskId(res.task_id);
    },
    onError: (err) => {
      if (
        err instanceof AdminApiError &&
        (err.status === 409 || /UPDATE_NO_BACKUP/i.test(err.message))
      ) {
        setNoBackup(true);
        setUpdateError(null);
      } else {
        setNoBackup(false);
        setUpdateError(errMsg(err));
      }
    },
  });

  const rollbackMutation = useMutation({
    mutationFn: () => requestRollback(serverId),
    onSuccess: (res) => {
      setRollbackError(null);
      setRollbackTaskId(res.task_id);
    },
    onError: (err) => setRollbackError(errMsg(err)),
  });

  const updateTask = updateTaskQuery.data
    ? taskProgress(updateTaskQuery.data)
    : null;
  const rollbackTask = rollbackTaskQuery.data
    ? taskProgress(rollbackTaskQuery.data)
    : null;

  return (
    <section aria-label="Update image">
      <h3>Update image</h3>
      <dl>
        <dt>Current</dt>
        <dd>
          <code>{current || "unknown"}</code>
          {typeof currentDigest === "string" && currentDigest !== "" && (
            <>
              {" "}
              <code title={currentDigest}>
                {currentDigest.slice(0, 19)}…
              </code>
            </>
          )}
        </dd>
        <dt>Previous (rollback target)</dt>
        <dd>
          {previous ? (
            <code>{previous}</code>
          ) : (
            <span className="muted">
              none recorded — rollback unavailable
            </span>
          )}
          {typeof previousDigest === "string" && previousDigest !== "" && (
            <>
              {" "}
              <code title={previousDigest}>
                {previousDigest.slice(0, 19)}…
              </code>
            </>
          )}
        </dd>
      </dl>

      <div className="row">
        <label>
          Target image{" "}
          <input
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            placeholder="pumpkin/mc:1.22"
            aria-label="Target image reference"
          />
        </label>
        <label>
          Digest (optional){" "}
          <input
            value={digest}
            onChange={(e) => setDigest(e.target.value)}
            placeholder="sha256:…"
            aria-label="Target image digest (optional)"
          />
        </label>
        <button
          type="button"
          disabled={previewBusy}
          onClick={onPreview}
        >
          {previewBusy ? "Previewing…" : "Preview"}
        </button>
      </div>
      {previewError && (
        <p className="err" role="alert">
          {previewError}
        </p>
      )}
      {preview && (
        <div>
          <p className="muted" role="status">
            Preview of <code>{target.trim()}</code>
            {preview.resolved_digest && (
              <>
                {" "}(resolves to <code>{preview.resolved_digest}</code>)
              </>
            )}
            {preview.current_reference !== "" &&
              preview.current_reference !== target.trim() && (
                <>
                  {" "}— current is <code>{preview.current_reference}</code>
                </>
              )}
            .
          </p>
          {preview.warnings.length === 0 && (
            <p className="muted">No warnings.</p>
          )}
          {preview.warnings.length > 0 && (
            <ul>
              {preview.warnings.map((w, i) => (
                <li key={i} className="warn">
                  {w}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      <div className="row">
        <button
          type="button"
          disabled={target.trim() === "" || updateMutation.isPending || !canUpdate}
          title={canUpdate ? undefined : "Your role cannot update images"}
          onClick={() => updateMutation.mutate()}
        >
          {updateTaskId ? "Updating… polling" : "Update to this image"}
        </button>
      </div>
      {updateError && (
        <p className="err" role="alert">
          {updateError}
        </p>
      )}
      {noBackup && (
        <p className="warn" role="alert">
          No verified backup found — the update was refused.{" "}
          <a href="#backups">Create a backup first</a>, then retry.
        </p>
      )}
      {updateTask && updateTaskId && (
        <p className="muted" role="status">
          Update task {updateTaskQuery.data?.id}:{" "}
          <span className="badge">{updateTaskQuery.data?.state}</span>
          {typeof updateTask.progress_percent === "number" &&
            ` ${updateTask.progress_percent}%`}
          {updateTask.message ? ` — ${updateTask.message}` : ""}
          {updateTask.error ? ` — error: ${updateTask.error}` : ""}
        </p>
      )}
      {updateTaskQuery.error && (
        <p className="err" role="alert">
          Failed to poll update task status.
        </p>
      )}

      <p className="warn" role="note">
        {ROLLBACK_ONE_WAY}
      </p>
      <div className="row">
        <button
          type="button"
          disabled={previous === null || rollbackMutation.isPending || !canUpdate}
          title={
            !canUpdate
              ? "Your role cannot roll back images"
              : previous === null
                ? "No previous image recorded — rollback unavailable"
                : `Roll back to ${previous}`
          }
          onClick={() => {
            if (
              window.confirm(
                `Roll back to ${previous ?? "the previous image"}? ` +
                  ROLLBACK_ONE_WAY,
              )
            ) {
              rollbackMutation.mutate();
            }
          }}
        >
          {previous
            ? `Roll back to ${previous}`
            : "Rollback unavailable (no previous image)"}
        </button>
      </div>
      {rollbackError && (
        <p className="err" role="alert">
          {rollbackError}
        </p>
      )}
      {rollbackTask && rollbackTaskId && (
        <p className="muted" role="status">
          Rollback task {rollbackTaskQuery.data?.id}:{" "}
          <span className="badge">{rollbackTaskQuery.data?.state}</span>
          {rollbackTask.message ? ` — ${rollbackTask.message}` : ""}
          {rollbackTask.error ? ` — error: ${rollbackTask.error}` : ""}
        </p>
      )}
      {rollbackTaskQuery.error && (
        <p className="err" role="alert">
          Failed to poll rollback task status.
        </p>
      )}
    </section>
  );
}

function formatMemory(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "Unavailable";
  const gib = bytes / (1024 ** 3);
  return `${gib.toFixed(gib < 1 ? 2 : 1)} GiB`;
}

function RuntimeSettingsSection({ serverId }: { serverId: string }): React.JSX.Element {
  const qc = useQueryClient();
  const { user } = useAuth();
  const settingsQuery = useQuery({
    queryKey: ["servers", serverId, "settings"],
    queryFn: () => fetchServerSettings(serverId),
    retry: 1,
  });
  const [memoryGiB, setMemoryGiB] = useState("2");
  const [cpuLimit, setCpuLimit] = useState("2");
  const [autoStart, setAutoStart] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const readOnly = user?.role === "viewer";

  useEffect(() => {
    const settings = settingsQuery.data?.settings;
    if (!settings) return;
    setMemoryGiB((settings.memory_limit_bytes / (1024 ** 3)).toString());
    setCpuLimit(settings.cpu_limit.toString());
    setAutoStart(settings.auto_start);
  }, [settingsQuery.data]);

  const mutation = useMutation({
    mutationFn: () => {
      const memory = Number(memoryGiB);
      const cpu = Number(cpuLimit);
      if (!Number.isFinite(memory) || memory < 0.25 || memory > 32) {
        throw new Error("Memory must be between 0.25 and 32 GiB.");
      }
      if (!Number.isFinite(cpu) || cpu < 0.1 || cpu > 16) {
        throw new Error("CPU must be between 0.1 and 16 vCPU.");
      }
      return updateServerSettings(serverId, {
        memory_limit_bytes: Math.round(memory * (1024 ** 3)),
        cpu_limit: cpu,
        auto_start: autoStart,
      });
    },
    onSuccess: (result) => {
      setError(null);
      setMessage(result.restart_required
        ? "Saved. The new limits apply the next time this world is recreated."
        : "Runtime settings saved.");
      qc.invalidateQueries({ queryKey: ["servers", serverId, "settings"] });
    },
    onError: (err) => {
      setMessage(null);
      setError(err instanceof Error ? err.message : "Runtime settings could not be saved.");
    },
  });

  return (
    <section className="panel runtime-settings" aria-labelledby="runtime-settings-heading">
      <div className="section-heading runtime-settings-heading">
        <div>
          <h3 id="runtime-settings-heading">Runtime resources</h3>
          <p className="muted">Bound the container so one world cannot starve the node.</p>
        </div>
        <span className="badge">Docker limits</span>
      </div>
      {settingsQuery.isLoading && <p className="muted">Loading resource policy…</p>}
      {settingsQuery.error && <p className="err" role="alert">Resource policy could not be loaded.</p>}
      {settingsQuery.data && (
        <>
          <div className="resource-grid">
            <label>
              Memory limit
              <span>256 MiB to 32 GiB</span>
              <input type="number" min="0.25" max="32" step="0.25" value={memoryGiB} onChange={(event) => setMemoryGiB(event.target.value)} disabled={readOnly || mutation.isPending} />
            </label>
            <label>
              CPU limit
              <span>0.1 to 16 vCPU</span>
              <input type="number" min="0.1" max="16" step="0.1" value={cpuLimit} onChange={(event) => setCpuLimit(event.target.value)} disabled={readOnly || mutation.isPending} />
            </label>
            <label className="resource-toggle">
              <span>Startup behavior</span>
              <span><input type="checkbox" checked={autoStart} onChange={(event) => setAutoStart(event.target.checked)} disabled={readOnly || mutation.isPending} /> Start this world when the manager boots</span>
            </label>
          </div>
          <div className="runtime-settings-footer">
            <span className="muted">Current: {formatMemory(settingsQuery.data.settings.memory_limit_bytes)} · {settingsQuery.data.settings.cpu_limit} vCPU · manager restart policy</span>
            <button type="button" disabled={readOnly || mutation.isPending} onClick={() => mutation.mutate()}>{mutation.isPending ? "Saving…" : "Save resource policy"}</button>
          </div>
          {readOnly && <p className="muted" role="note">Viewer accounts can inspect resource limits but cannot change them.</p>}
          {message && <p className="muted" role="status">{message}</p>}
          {error && <p className="err" role="alert">{error}</p>}
        </>
      )}
    </section>
  );
}

function stateLabel(state: string): string {
  const labels: Record<string, string> = {
    online: "Online",
    starting: "Starting",
    stopping: "Stopping",
    stopped: "Stopped",
    unhealthy: "Unhealthy",
    unknown: "Unknown",
    running: "Running",
    exited: "Stopped",
    missing: "Missing",
  };
  return labels[state] ?? state.replaceAll("_", " ");
}

function readinessClass(state: string): string {
  if (state === "online") return "badge badge-ok";
  if (state === "unhealthy" || state === "missing") return "badge badge-err";
  if (state === "starting" || state === "stopping") return "badge badge-warn";
  return "badge";
}

function ScheduledRestartsSection({
  serverId,
}: {
  serverId: string;
}): React.JSX.Element {
  const qc = useQueryClient();
  const { can } = useAuth();
  const canManage = can("server.lifecycle");
  const [schedule, setSchedule] = useState("0 4 * * *");
  const [timezone, setTimezone] = useState("UTC");
  const [enabled, setEnabled] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const query = useQuery({
    queryKey: ["servers", serverId, "scheduled-restarts"],
    queryFn: () => listScheduledRestarts(serverId),
    retry: 1,
  });
  const create = useMutation({
    mutationFn: () =>
      createScheduledRestart(serverId, {
        schedule: schedule.trim(),
        timezone: timezone.trim(),
        enabled,
      }),
    onSuccess: () => {
      setError(null);
      setNotice("Restart scheduled.");
      qc.invalidateQueries({
        queryKey: ["servers", serverId, "scheduled-restarts"],
      });
    },
    onError: (err) =>
      setError(
        err instanceof Error ? err.message : "Failed to schedule restart.",
      ),
  });
  const remove = useMutation({
    mutationFn: (id: string) => deleteScheduledRestart(id),
    onSuccess: () => {
      setError(null);
      setNotice("Scheduled restart removed.");
      qc.invalidateQueries({
        queryKey: ["servers", serverId, "scheduled-restarts"],
      });
    },
    onError: (err) =>
      setError(
        err instanceof Error ? err.message : "Failed to delete schedule.",
      ),
  });

  function onCreate(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setError(null);
    setNotice(null);
    if (schedule.trim() === "") {
      setError("Schedule is required.");
      return;
    }
    create.mutate();
  }

  return (
    <section className="workspace-section" id="restarts">
      <div className="section-heading">
        <h3>Scheduled restarts</h3>
        <span className="muted">5-field cron · UTC unless set</span>
      </div>
      <p className="muted">
        Restarts are queued through the task runner; a busy server is retried,
        never double-run.
      </p>
      {error && <p className="err" role="alert">{error}</p>}
      {notice && <p className="muted" role="status">{notice}</p>}
      {query.isLoading && <p className="muted">Loading schedules…</p>}
      {query.error && (
        <p className="err" role="alert">Failed to load schedules.</p>
      )}
      {query.data && query.data.length === 0 && (
        <p className="muted">No scheduled restarts.</p>
      )}
      {query.data && query.data.length > 0 && (
        <ul className="row-list">
          {query.data.map((task: ScheduledRestart) => (
            <li key={task.id} className="row">
              <code>{task.schedule}</code>
              <span className="muted">{task.timezone}</span>
              <span className="badge">
                {task.enabled ? "enabled" : "disabled"}
              </span>
              <span className="muted">
                next: {task.next_run_at ?? "—"}
              </span>
              {canManage && (
                <button
                  type="button"
                  disabled={remove.isPending}
                  onClick={() => {
                    if (
                      window.confirm(
                        `Delete restart schedule ${task.schedule}?`,
                      )
                    ) {
                      remove.mutate(task.id);
                    }
                  }}
                >
                  Delete
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      <form onSubmit={onCreate}>
        <div className="row">
          <label>
            Cron schedule{" "}
            <input
              value={schedule}
              onChange={(event) => setSchedule(event.target.value)}
              placeholder="0 4 * * *"
              disabled={!canManage}
              required
            />
          </label>
          <label>
            Timezone{" "}
            <input
              value={timezone}
              onChange={(event) => setTimezone(event.target.value)}
              placeholder="UTC"
              disabled={!canManage}
            />
          </label>
          <label>
            <input
              type="checkbox"
              checked={enabled}
              onChange={(event) => setEnabled(event.target.checked)}
              disabled={!canManage}
            />{" "}
            Enabled
          </label>
          <button type="submit" disabled={!canManage || create.isPending}>
            {create.isPending ? "Scheduling…" : "Add restart"}
          </button>
        </div>
      </form>
      {!canManage && (
        <p className="muted">
          Your role cannot manage scheduled restarts.
        </p>
      )}
    </section>
  );
}

export default function ServerDetail() {
  const { id } = useParams<{ id: string }>();
  const { can } = useAuth();
  const canLifecycle = can("server.lifecycle");
  const qc = useQueryClient();
  const [taskId, setTaskId] = useState("");
  const [submittedTaskId, setSubmittedTaskId] = useState<string | null>(null);
  const [activeTaskId, setActiveTaskId] = useState<string | null>(null);
  const [activeOp, setActiveOp] = useState<LifecycleOp | null>(null);
  const [opFeedback, setOpFeedback] = useState<string | null>(null);
  const [opError, setOpError] = useState<string | null>(null);
  const [logTail, setLogTail] = useState<number>(200);
  const [copied, setCopied] = useState(false);
  const [copiedAddress, setCopiedAddress] = useState(false);
  const [historyRange, setHistoryRange] = useState<MetricsRange>("1h");

  const serverQuery = useQuery({
    queryKey: ["servers", id],
    queryFn: () => getServer(id ?? ""),
    enabled: !!id,
  });

  const statusQuery = useQuery({
    queryKey: ["servers", id, "status"],
    queryFn: () => fetchStatus(id ?? ""),
    enabled: !!id,
    refetchInterval: 15_000,
    retry: 1,
  });

  const hasContainer = Boolean(serverQuery.data?.container_id);
  const logsQuery = useQuery({
    queryKey: ["servers", id, "logs", logTail],
    queryFn: () => fetchServerLogs(id ?? "", logTail),
    // A fresh server has no container yet; polling would 404 every 5s.
    enabled: !!id && hasContainer,
    refetchInterval: 5_000,
    retry: 1,
  });

  const historyQuery = useQuery({
    queryKey: ["servers", id, "tasks"],
    queryFn: () => fetchServerTasks(id ?? "", 10, 0),
    enabled: !!id,
    retry: 1,
  });

  const historyDetailQuery = useQuery({
    queryKey: ["servers", id, "metrics", "history", historyRange],
    queryFn: () => fetchMetricsHistory(id ?? "", historyRange),
    enabled: !!id,
    retry: 1,
    staleTime: 30_000,
  });

  const taskQuery = useQuery<TaskStatus>({
    queryKey: ["tasks", submittedTaskId],
    queryFn: () => fetchTask(submittedTaskId ?? ""),
    enabled: submittedTaskId !== null && submittedTaskId !== "",
    retry: false,
  });

  const lifecycleMutation = useMutation({
    mutationFn: (op: LifecycleOp) => postLifecycle(id ?? "", op),
    onSuccess: (result, op) => {
      if (isLifecycleAccepted(result)) {
        setActiveOp(op);
        setActiveTaskId(result.task_id);
        setOpFeedback(`“${op}” accepted (task ${result.task_id}). Polling…`);
        setOpError(null);
      } else {
        setActiveOp(null);
        setActiveTaskId(null);
        setOpFeedback(`Already ${result.desired_state} — no work queued.`);
        setOpError(null);
      }
    },
    onError: (err) => {
      setActiveOp(null);
      setActiveTaskId(null);
      setOpError(err instanceof Error ? err.message : "Operation failed.");
    },
  });

  const activeTaskQuery = useQuery<TaskStatus>({
    queryKey: ["tasks", activeTaskId],
    queryFn: () => fetchTask(activeTaskId ?? ""),
    enabled: activeTaskId !== null && activeTaskId !== "",
    refetchInterval: activeTaskId ? 2_000 : false,
    retry: false,
  });

  useEffect(() => {
    const t = activeTaskQuery.data;
    if (!t || !isTerminalTaskState(t.state)) return;
    const op = activeOp;
    qc.invalidateQueries({ queryKey: ["servers", id] });
    qc.invalidateQueries({ queryKey: ["servers", id, "status"] });
    qc.invalidateQueries({ queryKey: ["servers", id, "tasks"] });
    if (t.state === "succeeded") {
      setOpFeedback(op ? `“${op}” succeeded.` : "Operation succeeded.");
      setOpError(null);
    } else {
      setOpError(
        t.error ?? (op ? `“${op}” ended with state: ${t.state}.` : `Task ${t.state}.`),
      );
    }
    setActiveTaskId(null);
    setActiveOp(null);
  }, [activeTaskQuery.data, activeOp, id, qc]);

  function onTaskSubmit(e: FormEvent) {
    e.preventDefault();
    setSubmittedTaskId(taskId.trim() === "" ? null : taskId.trim());
  }

  function runLifecycle(op: LifecycleOp) {
    setOpFeedback(null);
    setOpError(null);
    lifecycleMutation.mutate(op);
  }

  async function copyLogs() {
    const text = (logsQuery.data?.logs ?? []).join("\n");
    try {
      if (navigator.clipboard) {
        await navigator.clipboard.writeText(text);
      } else {
        const ta = document.createElement("textarea");
        ta.value = text;
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      }
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      setCopied(false);
    }
  }

  async function copyAddress(address: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(address);
      setCopiedAddress(true);
      window.setTimeout(() => setCopiedAddress(false), 2000);
    } catch {
      setCopiedAddress(false);
    }
  }

  if (!id) return <p className="err">Missing server id.</p>;
  if (serverQuery.isLoading) return <p>Loading server…</p>;
  if (serverQuery.error || !serverQuery.data)
    return <p className="err">Failed to load server.</p>;

  const s = serverQuery.data;
  const st = statusQuery.data;
  const runtimeStale = statusQuery.isError || st?.runtime_state_stale === true;
  const ping = runtimeStale ? null : st?.readiness?.ping;
  const opBusy = lifecycleMutation.isPending || activeTaskId !== null;
  const readiness = runtimeStale ? null : st?.readiness;
  const readinessState = runtimeStale ? "unknown" : readiness?.state ?? s.observed_state;
  const observedState = runtimeStale ? "unknown" : s.observed_state;
  const readinessReason = statusQuery.isError
    ? "Live status unavailable; cached state is not current."
    : st?.readiness?.reason ?? "Waiting for a live status probe.";
  const connectionAddress = `${window.location.hostname}:${s.game_port}`;

  return (
    <section className="server-workspace">
      <p className="server-breadcrumb"><Link to="/servers">Servers</Link> <span className="muted">/ {s.name}</span></p>
      <div className="server-hero">
        <div className="server-hero-copy">
          <div className="eyebrow">Pumpkin world · Server workspace</div>
          <div className="server-title-row"><span className="server-avatar server-avatar-large" aria-hidden="true">{s.name.slice(0, 1).toUpperCase()}</span><div><h2>{s.name}</h2><div className="server-hero-meta">Pumpkin · Java {s.game_port} · {s.image_reference}</div></div></div>
          <div className="server-connection"><span className="connection-label">Connect</span><code>{connectionAddress}</code><button className="button-secondary" type="button" onClick={() => void copyAddress(connectionAddress)}>{copiedAddress ? "Copied" : "Copy address"}</button></div>
        </div>
        <div className="server-actions" role="group" aria-label="Lifecycle controls">
          {(["start", "stop", "restart"] as const).map((op) => (
            <button
              key={op}
              type="button"
              disabled={opBusy || !canLifecycle}
              title={
                !canLifecycle
                  ? "Your role cannot run lifecycle operations"
                  : opBusy
                    ? "An operation is already in flight"
                    : `Set desired state via ${op}`
              }
              onClick={() => runLifecycle(op)}
            >
              {lifecycleMutation.isPending && activeOp === null ? "Sending…" : activeTaskId && activeOp === op ? `${op}… polling` : op}
            </button>
          ))}
        </div>
      </div>

      <nav className="server-tabs" aria-label="Server sections">
        <a className="active" href="#overview">Overview</a>
        <a href="#console">Console & logs</a>
        <Link to={`/servers/${encodeURIComponent(s.id)}/config`}>Configuration</Link>
        <Link to={`/servers/${encodeURIComponent(s.id)}/files`}>Files</Link>
        <a href="#backups">Backups</a>
        <a href="#restarts">Restarts</a>
        <a href="#metrics">Metrics</a>
      </nav>
      {opFeedback && (
        <p className="muted" role="status">
          {opFeedback}
        </p>
      )}
      {opError && (
        <p className="err" role="alert">
          {opError}
        </p>
      )}
      {activeTaskQuery.error && (
        <p className="err" role="alert">
          Failed to poll task status.
        </p>
      )}

      <div id="overview" className="server-summary-grid">
         <div className="server-summary-card server-summary-status"><span className="summary-label">Readiness</span><strong className={readinessClass(readinessState)}>{stateLabel(readinessState)}</strong><span className="summary-detail">{readinessReason}</span></div>
        <div className="server-summary-card"><span className="summary-label">Players</span><strong>{ping ? `${ping.players_online}/${ping.players_max}` : "—"}</strong><span className="summary-detail">{ping ? `${ping.latency_ms} ms latency` : "Unavailable until online"}</span></div>
        <div className="server-summary-card"><span className="summary-label">Runtime</span><strong>{ping?.version ?? "Pumpkin"}</strong><span className="summary-detail">{s.image_reference}</span></div>
         <div className="server-summary-card"><span className="summary-label">Observed state</span><strong>{stateLabel(observedState)}</strong><span className="summary-detail">Desired: {stateLabel(s.desired_state)}</span></div>
      </div>
      {s.last_error && <p className="server-error-banner err" role="alert">{s.last_error}</p>}

      <RuntimeSettingsSection serverId={s.id} />

      <div id="metrics" className="section-heading"><h3>Metrics history</h3><Link className="section-link" to="/monitoring">Full monitoring</Link></div>
      <div className="row">
        <label>
          Range{" "}
          <select
            value={historyRange}
            onChange={(e) => {
              const v = e.target.value;
              if (isMetricsRange(v)) setHistoryRange(v);
            }}
            aria-label="Metrics history range"
          >
            {METRICS_RANGES.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        </label>
      </div>
      {historyDetailQuery.isLoading && <p>Loading metrics history…</p>}
      {historyDetailQuery.error && (
        <p className="err">Failed to load metrics history.</p>
      )}
      {historyDetailQuery.data && (
        <ServerHistoryPanels samples={historyDetailQuery.data.samples} />
      )}

      <div id="console" className="section-heading"><h3>Console & logs</h3><span className="muted">Read-only until Pumpkin command input is verified</span></div>
      <div className="console-capability"><span className="capability-icon">i</span><span>Pumpkin’s remote command interface is not verified for this build, so this workspace is safely read-only. Live streaming is the next event-layer upgrade.</span></div>
      <div className="row">
        <label>
          Tail{" "}
          <select
            value={logTail}
            onChange={(e) => setLogTail(Number(e.target.value))}
            aria-label="Log tail lines"
          >
            {LOG_TAIL_OPTIONS.map((n) => (
              <option key={n} value={n}>
                last {n} lines
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          onClick={copyLogs}
          disabled={
            !hasContainer ||
            !logsQuery.data ||
            logsQuery.data.logs.length === 0
          }
        >
          {copied ? "Copied ✓" : "Copy"}
        </button>
        <span className="muted">auto-refreshes every 5s</span>
      </div>
      {!hasContainer && !serverQuery.isLoading && (
        <p className="muted">
          No container yet — start the world to see logs.
        </p>
      )}
      {hasContainer && logsQuery.isLoading && <p>Loading logs…</p>}
      {hasContainer && logsQuery.error && (
        <p className="muted" role="status">
          Logs are unavailable right now (the container may not be running).
        </p>
      )}
      {logsQuery.data && logsQuery.data.logs.length === 0 && (
        <p className="muted">No log lines.</p>
      )}
      {logsQuery.data && logsQuery.data.logs.length > 0 && (
        <pre className="log" tabIndex={0}>
          {logsQuery.data.logs.join("\n")}
        </pre>
      )}

      <div className="section-heading"><h3>Task history</h3><span className="muted">Lifecycle and recovery operations</span></div>
      {historyQuery.isLoading && <p>Loading task history…</p>}
      {historyQuery.error && <p className="err">Failed to load task history.</p>}
      {historyQuery.data && historyQuery.data.tasks.length === 0 && (
        <p className="muted">No tasks recorded for this server.</p>
      )}
      {historyQuery.data && historyQuery.data.tasks.length > 0 && (
        <ul>
          {historyQuery.data.tasks.map((t) => (
            <li key={t.id}>
              <span className="badge">{t.state}</span>{" "}
              <span className="muted">{t.kind}</span>{" "}
              <span className="muted">
                {t.progress_percent !== null && `${t.progress_percent}% `}
                {t.message ?? ""}
                {t.error ? ` — ${t.error}` : ""}
              </span>
            </li>
          ))}
        </ul>
      )}

      <details className="advanced-debug">
        <summary>Advanced task lookup</summary>
        <p className="muted">Use a task ID from support or audit work when you need to inspect a persisted task directly.</p>
        <form onSubmit={onTaskSubmit}>
          <label>Task ID<input value={taskId} onChange={(e) => setTaskId(e.target.value)} placeholder="task id" /></label>
          <button type="submit">Check task</button>
        </form>
        {taskQuery.isLoading && <p>Loading task…</p>}
        {taskQuery.error && <p className="err">Task not found or failed to load.</p>}
        {taskQuery.data && <dl role="status"><dt>Task</dt><dd>{taskQuery.data.id}</dd><dt>Kind</dt><dd>{taskQuery.data.kind}</dd><dt>State</dt><dd><span className="badge">{stateLabel(taskQuery.data.state)}</span></dd>{taskQuery.data.error && <><dt>Error</dt><dd className="err">{taskQuery.data.error}</dd></>}</dl>}
      </details>
      <div id="backups" className="workspace-section">
        <BackupsSection
          serverId={s.id}
          observedState={s.observed_state}
          desiredState={s.desired_state}
        />
      </div>
      <ScheduledRestartsSection serverId={s.id} />
      <div className="workspace-section"><UpdateSection serverId={s.id} server={s} /></div>
    </section>
  );
}
