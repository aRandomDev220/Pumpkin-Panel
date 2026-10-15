import { useQueries, useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  digestWarning,
  fetchMetricsSummary,
  fetchServerTasks,
  fetchStatus,
  listServers,
} from "../lib/servers";
import type {
  ReadinessInfo,
  ServerSummary,
  ServerTask,
} from "../lib/servers";
import { fetchAlerts } from "../lib/monitoring";
import { AlertsList } from "./Monitoring";

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

function readinessBadgeClass(state: string): string {
  if (state === "online") return "badge badge-ok";
  if (state === "unhealthy") return "badge badge-err";
  return "badge";
}

function ReadinessBadge({
  readiness,
  observed,
}: {
  readiness?: ReadinessInfo | null;
  observed: string;
}): React.JSX.Element {
  if (readiness) {
    return (
      <span className={readinessBadgeClass(readiness.state)} title={readiness.reason}>
        {readiness.state}: {readiness.reason}
      </span>
    );
  }
  // Backend revisions before the readiness contract omit `readiness`;
  // fall back to observed state text so the row still carries status.
  return (
    <span className="badge" title="Readiness not reported by backend">
      observed:{observed} · readiness n/a
    </span>
  );
}

interface Alert {
  level: "err" | "warn";
  text: string;
}

function buildAlerts(
  servers: ServerSummary[],
  statuses: Map<string, { dockerAvailable: boolean; containerMissing: boolean }>,
  statusFailed: boolean,
): Alert[] {
  const alerts: Alert[] = [];
  for (const s of servers) {
    if (digestWarning(s.image_reference) !== null) {
      alerts.push({
        level: "warn",
        text: `${s.name}: unpinned image (no @sha256:) — tag may drift between pulls.`,
      });
    }
    if (s.desired_state !== s.observed_state) {
      alerts.push({
        level: "warn",
        text: `${s.name}: desired≠observed drift (desired:${s.desired_state} observed:${s.observed_state}).`,
      });
    }
    const st = statuses.get(s.id);
    if (st) {
      if (!st.dockerAvailable) {
        alerts.push({ level: "err", text: `${s.name}: docker unavailable.` });
      }
      if (st.containerMissing) {
        alerts.push({ level: "err", text: `${s.name}: missing container.` });
      }
    }
  }
  if (statusFailed && servers.length > 0) {
    alerts.push({
      level: "warn",
      text: "Some status probes failed; docker/container alerts may be incomplete.",
    });
  }
  return alerts;
}

export default function Overview() {
  const serversQuery = useQuery({ queryKey: ["servers"], queryFn: listServers });
  const metricsQuery = useQuery({
    queryKey: ["metrics", "summary"],
    queryFn: fetchMetricsSummary,
    retry: 1,
  });
  const backendAlertsQuery = useQuery({
    queryKey: ["alerts"],
    queryFn: fetchAlerts,
    retry: 1,
  });

  const servers = serversQuery.data?.servers ?? [];

  const statusQueries = useQueries({
    queries: servers.map((s) => ({
      queryKey: ["servers", s.id, "status"],
      queryFn: () => fetchStatus(s.id),
      retry: 1,
      staleTime: 10_000,
    })),
  });

  const recentTaskQueries = useQueries({
    queries: servers.map((s) => ({
      queryKey: ["servers", s.id, "tasks", "recent"],
      queryFn: () => fetchServerTasks(s.id, 3, 0),
      retry: 1,
      staleTime: 10_000,
    })),
  });

  const desiredRunning = servers.filter((s) => s.desired_state === "running").length;
  const observedRunning = servers.filter((s) => s.observed_state === "running").length;
  const drifted = servers.filter((s) => s.desired_state !== s.observed_state).length;

  const statusById = new Map<
    string,
    { dockerAvailable: boolean; containerMissing: boolean }
  >();
  servers.forEach((s, i) => {
    const data = statusQueries[i]?.data;
    if (data) {
      statusById.set(s.id, {
        dockerAvailable: data.docker_available,
        containerMissing: data.container === null || data.observed_state === "missing",
      });
    }
  });
  const statusFailed = statusQueries.some((q) => q.isError);
  const statusLoading = statusQueries.some((q) => q.isLoading);
  const alerts = buildAlerts(servers, statusById, statusFailed);

  const recentActivity: { serverName: string; task: ServerTask }[] = [];
  servers.forEach((s, i) => {
    const tasks = recentTaskQueries[i]?.data?.tasks ?? [];
    for (const task of tasks) recentActivity.push({ serverName: s.name, task });
  });
  const tasksLoading = recentTaskQueries.some((q) => q.isLoading);
  const tasksFailed = recentTaskQueries.filter((q) => q.isError).length;

  return (
    <section>
      <h2>Overview</h2>

      {serversQuery.isLoading && <p>Loading servers…</p>}
      {serversQuery.error && <p className="err">Failed to load servers.</p>}
      {!serversQuery.isLoading && !serversQuery.error && servers.length === 0 && (
        <p className="muted">
          No servers yet. <Link to="/servers">Create one</Link> to get started.
        </p>
      )}

      {!serversQuery.isLoading && !serversQuery.error && servers.length > 0 && (
        <>
          <div className="cards" aria-label="Server state counts">
            <div className="card">
              <div className="card-num">{servers.length}</div>
              <div className="muted">servers total</div>
            </div>
            <div className="card">
              <div className="card-num">{desiredRunning}</div>
              <div className="muted">desired running</div>
            </div>
            <div className="card">
              <div className="card-num">{observedRunning}</div>
              <div className="muted">observed running</div>
            </div>
            <div className="card">
              <div className="card-num">{drifted}</div>
              <div className="muted">desired≠observed drift</div>
            </div>
          </div>

          <h3>Host</h3>
          {metricsQuery.isLoading && <p>Loading host stats…</p>}
          {metricsQuery.error && (
            <p className="err">Failed to load host stats.</p>
          )}
          {metricsQuery.data && (
            <div className="cards" aria-label="Host stats">
              <div className="card">
                <div className="card-num">
                  {metricsQuery.data.host.cpu_percent.toFixed(1)}%
                </div>
                <div className="muted">host CPU</div>
              </div>
              <div className="card">
                <div className="card-num">
                  {formatBytes(metricsQuery.data.host.memory_used_bytes)} /{" "}
                  {formatBytes(metricsQuery.data.host.memory_total_bytes)}
                </div>
                <div className="muted">memory used / total</div>
              </div>
              <div className="card">
                <div className="card-num">
                  {formatBytes(metricsQuery.data.host.disk_free_bytes)} free
                </div>
                <div className="muted">
                  of {formatBytes(metricsQuery.data.host.disk_total_bytes)} disk
                </div>
              </div>
            </div>
          )}

          <h3>Servers</h3>
          {statusLoading && servers.length > 0 && (
            <p className="muted">Loading live status…</p>
          )}
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Readiness</th>
                <th scope="col">Desired</th>
                <th scope="col">Observed</th>
                <th scope="col">CPU</th>
                <th scope="col">Memory</th>
              </tr>
            </thead>
            <tbody>
              {servers.map((s, i) => {
                const st = statusQueries[i]?.data;
                const metric = metricsQuery.data?.servers.find(
                  (m) => m.server_id === s.id,
                );
                return (
                  <tr key={s.id}>
                    <td>
                      <Link to={`/servers/${encodeURIComponent(s.id)}`}>
                        {s.name}
                      </Link>
                    </td>
                    <td>
                      {statusQueries[i]?.isLoading && (
                        <span className="muted">checking…</span>
                      )}
                      {statusQueries[i]?.error && (
                        <span className="err">status failed</span>
                      )}
                      {st && (
                        <ReadinessBadge
                          readiness={st.readiness}
                          observed={st.observed_state}
                        />
                      )}
                    </td>
                    <td>
                      <span className="badge">{s.desired_state}</span>
                    </td>
                    <td>
                      <span className="badge">{s.observed_state}</span>
                    </td>
                    <td>
                      {metric?.cpu_percent === null ||
                      metric?.cpu_percent === undefined
                        ? "n/a"
                        : `${metric.cpu_percent.toFixed(1)}%`}
                    </td>
                    <td>
                      {metric?.memory_bytes === null ||
                      metric?.memory_bytes === undefined
                        ? "n/a"
                        : formatBytes(metric.memory_bytes)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>

          <h3>Alerts</h3>
          {alerts.length === 0 && (
            <p className="muted">No alerts. All servers look healthy.</p>
          )}
          {alerts.length > 0 && (
            <ul>
              {alerts.map((a, i) => (
                <li key={i} className={a.level === "err" ? "err" : "warn"}>
                  {a.text}
                </li>
              ))}
            </ul>
          )}

          <h3>Monitoring alerts</h3>
          {backendAlertsQuery.isLoading && <p>Loading monitoring alerts…</p>}
          {backendAlertsQuery.error && (
            <p className="err">Failed to load monitoring alerts.</p>
          )}
          {backendAlertsQuery.data && (
            <AlertsList alerts={backendAlertsQuery.data.alerts} />
          )}
          <p className="muted">
            <Link to="/monitoring">Open monitoring →</Link>
          </p>

          <h3>Recent activity</h3>
          {tasksLoading && <p>Loading recent tasks…</p>}
          {!tasksLoading && tasksFailed > 0 && (
            <p className="err">
              Failed to load tasks for {tasksFailed} server
              {tasksFailed === 1 ? "" : "s"}.
            </p>
          )}
          {!tasksLoading && tasksFailed === 0 && recentActivity.length === 0 && (
            <p className="muted">No recent tasks.</p>
          )}
          {recentActivity.length > 0 && (
            <ul>
              {recentActivity.map(({ serverName, task }) => (
                <li key={task.id}>
                  <span className="badge">{task.state}</span>{" "}
                  <span className="muted">{task.kind}</span> {serverName}{" "}
                  <span className="muted">
                    {task.progress_percent !== null &&
                      `${task.progress_percent}% `}
                    {task.message ?? ""}
                    {task.error ? ` — ${task.error}` : ""}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </section>
  );
}
