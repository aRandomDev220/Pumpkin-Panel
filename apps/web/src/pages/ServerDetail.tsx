import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import {
  fetchMetricsSummary,
  fetchServerLogs,
  fetchServerTasks,
  fetchStatus,
  fetchTask,
  getServer,
  isLifecycleAccepted,
  isTerminalTaskState,
  postLifecycle,
} from "../lib/servers";
import type { LifecycleOp, TaskStatus } from "../lib/servers";

const LOG_TAIL_OPTIONS = [50, 200, 500];

function formatBytes(bytes: number): string {
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

export default function ServerDetail() {
  const { id } = useParams<{ id: string }>();
  const qc = useQueryClient();
  const [taskId, setTaskId] = useState("");
  const [submittedTaskId, setSubmittedTaskId] = useState<string | null>(null);
  const [activeTaskId, setActiveTaskId] = useState<string | null>(null);
  const [activeOp, setActiveOp] = useState<LifecycleOp | null>(null);
  const [opFeedback, setOpFeedback] = useState<string | null>(null);
  const [opError, setOpError] = useState<string | null>(null);
  const [logTail, setLogTail] = useState<number>(200);
  const [copied, setCopied] = useState(false);

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

  const logsQuery = useQuery({
    queryKey: ["servers", id, "logs", logTail],
    queryFn: () => fetchServerLogs(id ?? "", logTail),
    enabled: !!id,
    refetchInterval: 5_000,
    retry: 1,
  });

  const historyQuery = useQuery({
    queryKey: ["servers", id, "tasks"],
    queryFn: () => fetchServerTasks(id ?? "", 10, 0),
    enabled: !!id,
    retry: 1,
  });

  const metricsQuery = useQuery({
    queryKey: ["metrics", "summary"],
    queryFn: fetchMetricsSummary,
    retry: 1,
    staleTime: 10_000,
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

  if (!id) return <p className="err">Missing server id.</p>;
  if (serverQuery.isLoading) return <p>Loading server…</p>;
  if (serverQuery.error || !serverQuery.data)
    return <p className="err">Failed to load server.</p>;

  const s = serverQuery.data;
  const st = statusQuery.data;
  const ping = st?.readiness?.ping;
  const metric = metricsQuery.data?.servers.find((m) => m.server_id === id);
  const opBusy = lifecycleMutation.isPending || activeTaskId !== null;

  return (
    <section>
      <p className="muted">
        <Link to="/servers">← Servers</Link>
      </p>
      <h2>{s.name}</h2>

      <nav className="row" aria-label="Server sections">
        <span className="badge badge-ok">Overview</span>
        <Link to={`/servers/${encodeURIComponent(s.id)}/config`}>Config</Link>
        <Link to={`/servers/${encodeURIComponent(s.id)}/files`}>Files</Link>
      </nav>

      <div className="row" role="group" aria-label="Lifecycle controls">
        {(["start", "stop", "restart"] as const).map((op) => (
          <button
            key={op}
            type="button"
            disabled={opBusy}
            title={
              opBusy
                ? "An operation is already in flight"
                : `Set desired state via ${op}`
            }
            onClick={() => runLifecycle(op)}
          >
            {lifecycleMutation.isPending && activeOp === null
              ? "Sending…"
              : activeTaskId && activeOp === op
                ? `${op}… polling`
                : op}
          </button>
        ))}
      </div>
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

      <dl>
        <dt>ID</dt>
        <dd>{s.id}</dd>
        <dt>Game port</dt>
        <dd>{s.game_port}</dd>
        <dt>Image</dt>
        <dd>{s.image_reference}</dd>
        <dt>Desired</dt>
        <dd>
          <span className="badge">{s.desired_state}</span>
        </dd>
        <dt>Observed</dt>
        <dd>
          <span className="badge">{s.observed_state}</span>
        </dd>
        {s.container_id && (
          <>
            <dt>Container</dt>
            <dd>{s.container_id}</dd>
          </>
        )}
        {s.last_error && (
          <>
            <dt>Last error</dt>
            <dd className="err">{s.last_error}</dd>
          </>
        )}
        <dt>Readiness</dt>
        <dd>
          {statusQuery.isLoading && <span className="muted">checking…</span>}
          {statusQuery.error && <span className="err">Status probe failed.</span>}
          {st?.readiness && (
            <span
              className={
                st.readiness.state === "online"
                  ? "badge badge-ok"
                  : st.readiness.state === "unhealthy"
                    ? "badge badge-err"
                    : "badge"
              }
              title={st.readiness.reason}
            >
              {st.readiness.state}: {st.readiness.reason}
            </span>
          )}
          {st && !st.readiness && (
            <span className="muted">readiness not reported (backend revision)</span>
          )}
        </dd>
        {ping && (
          <>
            <dt>Players</dt>
            <dd>
              {ping.players_online} / {ping.players_max}
            </dd>
            <dt>Version</dt>
            <dd>{ping.version}</dd>
            <dt>Latency</dt>
            <dd>{ping.latency_ms} ms</dd>
            <dt>MOTD</dt>
            <dd>{ping.motd}</dd>
          </>
        )}
        <dt>Metrics</dt>
        <dd>
          {metricsQuery.isLoading && <span className="muted">loading…</span>}
          {(metricsQuery.error || (!metricsQuery.isLoading && !metric)) && (
            <span className="muted">Metrics unavailable for this server.</span>
          )}
          {metric && (
            <span>
              CPU{" "}
              {metric.cpu_percent === null
                ? "unavailable"
                : `${metric.cpu_percent.toFixed(1)}%`}
              {" · "}mem{" "}
              {metric.memory_bytes === null
                ? "unavailable"
                : formatBytes(metric.memory_bytes)}
              {" · "}{metric.state}
            </span>
          )}
        </dd>
      </dl>

      <h3>Logs</h3>
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
          disabled={!logsQuery.data || logsQuery.data.logs.length === 0}
        >
          {copied ? "Copied ✓" : "Copy"}
        </button>
        <span className="muted">auto-refreshes every 5s</span>
      </div>
      {logsQuery.isLoading && <p>Loading logs…</p>}
      {logsQuery.error && <p className="err">Failed to load logs.</p>}
      {logsQuery.data && logsQuery.data.logs.length === 0 && (
        <p className="muted">No log lines.</p>
      )}
      {logsQuery.data && logsQuery.data.logs.length > 0 && (
        <pre className="log" tabIndex={0}>
          {logsQuery.data.logs.join("\n")}
        </pre>
      )}

      <h3>Task history</h3>
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

      <h3>Task lookup</h3>
      <p className="muted">
        Start/stop/reconcile operations return a task id. Paste it here to poll
        status via <code>GET /api/v1/tasks/:id</code>.
      </p>
      <form onSubmit={onTaskSubmit}>
        <label>
          Task ID
          <input
            value={taskId}
            onChange={(e) => setTaskId(e.target.value)}
            placeholder="task id"
          />
        </label>
        <button type="submit">Check task</button>
      </form>
      {taskQuery.isLoading && <p>Loading task…</p>}
      {taskQuery.error && <p className="err">Task not found or failed to load.</p>}
      {taskQuery.data && (
        <dl>
          <dt>Task</dt>
          <dd>{taskQuery.data.id}</dd>
          <dt>Kind</dt>
          <dd>{taskQuery.data.kind}</dd>
          <dt>State</dt>
          <dd>
            <span className="badge">{taskQuery.data.state}</span>
          </dd>
          {taskQuery.data.error && (
            <>
              <dt>Error</dt>
              <dd className="err">{taskQuery.data.error}</dd>
            </>
          )}
        </dl>
      )}
    </section>
  );
}
