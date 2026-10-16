import { useQueries, useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  digestWarning,
  fetchMetricsSummary,
  fetchServerTasks,
  fetchStatus,
  listServers,
} from "../lib/servers";
import type { ReadinessInfo, ServerSummary, ServerTask } from "../lib/servers";
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

function readinessClass(state: string): string {
  if (state === "online") return "badge badge-ok";
  if (state === "unhealthy") return "badge badge-err";
  return "badge";
}

function shelfClass(state: string): string {
  if (state === "online") return "server-row server-online";
  if (state === "unhealthy") return "server-row server-error";
  if (state === "starting" || state === "stopping") return "server-row server-warning";
  return "server-row";
}

function ReadinessBadge({ readiness, observed }: { readiness?: ReadinessInfo | null; observed: string }): React.JSX.Element {
  if (readiness) {
    return <span className={readinessClass(readiness.state)} title={readiness.reason}>{readiness.state}</span>;
  }
  return <span className="badge" title="Readiness is not reported by this backend">{observed}</span>;
}

type Alert = { level: "err" | "warn"; text: string };

function buildAlerts(servers: ServerSummary[], statuses: Map<string, { dockerAvailable: boolean; containerMissing: boolean }>, statusFailed: boolean): Alert[] {
  const alerts: Alert[] = [];
  for (const server of servers) {
    if (digestWarning(server.image_reference) !== null) {
      alerts.push({ level: "warn", text: `${server.name} uses an unpinned image reference.` });
    }
    if (server.desired_state !== server.observed_state) {
      alerts.push({ level: "warn", text: `${server.name} has desired/observed state drift.` });
    }
    const status = statuses.get(server.id);
    if (status?.dockerAvailable === false) alerts.push({ level: "err", text: `${server.name} cannot reach Docker.` });
    if (status?.containerMissing) alerts.push({ level: "err", text: `${server.name} is missing its container.` });
  }
  if (statusFailed && servers.length > 0) alerts.push({ level: "warn", text: "Some live status checks failed." });
  return alerts;
}

export default function Overview(): React.JSX.Element {
  const serversQuery = useQuery({ queryKey: ["servers"], queryFn: listServers });
  const metricsQuery = useQuery({ queryKey: ["metrics", "summary"], queryFn: fetchMetricsSummary, retry: 1 });
  const backendAlertsQuery = useQuery({ queryKey: ["alerts"], queryFn: fetchAlerts, retry: 1 });
  const servers = serversQuery.data?.servers ?? [];

  const statusQueries = useQueries({
    queries: servers.map((server) => ({
      queryKey: ["servers", server.id, "status"],
      queryFn: () => fetchStatus(server.id),
      retry: 1,
      staleTime: 10_000,
    })),
  });
  const recentTaskQueries = useQueries({
    queries: servers.map((server) => ({
      queryKey: ["servers", server.id, "tasks", "recent"],
      queryFn: () => fetchServerTasks(server.id, 3, 0),
      retry: 1,
      staleTime: 10_000,
    })),
  });

  const statusById = new Map<string, { dockerAvailable: boolean; containerMissing: boolean }>();
  servers.forEach((server, index) => {
    const status = statusQueries[index]?.data;
    if (status) statusById.set(server.id, { dockerAvailable: status.docker_available, containerMissing: status.container === null || status.observed_state === "missing" });
  });
  const alerts = buildAlerts(servers, statusById, statusQueries.some((query) => query.isError));
  const desiredRunning = servers.filter((server) => server.desired_state === "running").length;
  const observedRunning = servers.filter((server) => server.observed_state === "running").length;
  const drifted = servers.filter((server) => server.desired_state !== server.observed_state).length;
  const recentActivity: Array<{ serverName: string; task: ServerTask }> = [];
  servers.forEach((server, index) => {
    for (const task of recentTaskQueries[index]?.data?.tasks ?? []) recentActivity.push({ serverName: server.name, task });
  });

  return (
    <section>
      <div className="page-heading">
        <div>
          <div className="eyebrow">Workspace overview</div>
          <h2>Keep your worlds in view.</h2>
          <p>A clear read on server health, host capacity, and the work happening across your control plane.</p>
        </div>
        <div className="heading-actions"><Link to="/servers"><button type="button">Create server</button></Link></div>
      </div>

      {serversQuery.isLoading && <div className="panel"><p className="muted">Loading your servers…</p></div>}
      {serversQuery.error && <div className="panel"><p className="err" role="alert">Failed to load servers. Refresh to try again.</p></div>}

      {!serversQuery.isLoading && !serversQuery.error && servers.length === 0 && (
        <div className="empty-state">
          <h3>Your workspace is ready.</h3>
          <p>Create the first Pumpkin server to start monitoring worlds, logs, backups, and player activity from here.</p>
          <Link to="/servers"><button type="button">Create your first server</button></Link>
        </div>
      )}

      {servers.length > 0 && (
        <>
          <div className="metric-grid" aria-label="Server summary">
            <div className="metric-card"><div className="card-num">{servers.length}</div><div className="metric-label">Servers</div><div className="metric-detail">Across this workspace</div></div>
            <div className="metric-card"><div className="card-num">{observedRunning}</div><div className="metric-label">Running now</div><div className="metric-detail">{desiredRunning} desired running</div></div>
            <div className="metric-card"><div className="card-num">{drifted}</div><div className="metric-label">Needs attention</div><div className="metric-detail">Desired/observed drift</div></div>
            <div className="metric-card"><div className="card-num">{alerts.length}</div><div className="metric-label">Open alerts</div><div className="metric-detail">Across servers and host</div></div>
          </div>

          <div className="overview-grid">
            <div className="panel attention-panel">
              <h3>Attention</h3>
              {alerts.length === 0 ? <div className="attention-empty"><strong>All clear</strong><span>No server issues require action.</span></div> : <ul className="attention-list">{alerts.slice(0, 5).map((alert, index) => <li key={`${alert.text}-${index}`} className={alert.level}><span>{alert.text}</span><Link to="/monitoring">Open monitoring</Link></li>)}</ul>}
            </div>
            <div className="server-shelf">
              <div className="server-shelf-header"><strong>Server shelf</strong><span>{servers.length} registered</span></div>
              {servers.map((server, index) => {
                const status = statusQueries[index]?.data;
                const metric = metricsQuery.data?.servers.find((item) => item.server_id === server.id);
                const readiness = status?.readiness;
                return <div className={shelfClass(readiness?.state ?? server.observed_state)} key={server.id}>
                  <div><Link className="server-name" to={`/servers/${encodeURIComponent(server.id)}`}>{server.name}</Link><div className="server-meta">Port {server.game_port} · desired {server.desired_state}</div></div>
                  <div className="server-state">{statusQueries[index]?.isLoading ? <span className="muted">Checking…</span> : <ReadinessBadge readiness={readiness} observed={server.observed_state} />}</div>
                  <div className="server-stat"><strong>{readiness?.ping ? `${readiness.ping.players_online}/${readiness.ping.players_max}` : "—"}</strong><small>players</small></div>
                  <Link className="section-link" to={`/servers/${encodeURIComponent(server.id)}`}>Open</Link>
                  {metric && <span className="sr-only">CPU {metric.cpu_percent === null ? "unavailable" : `${metric.cpu_percent.toFixed(1)} percent`}</span>}
                </div>;
              })}
            </div>
          </div>

          {metricsQuery.data && <><div className="section-heading"><h3>Host capacity</h3><Link className="section-link" to="/monitoring">View monitoring</Link></div><div className="host-summary"><div className="card"><div className="card-num">{metricsQuery.data.host.cpu_percent.toFixed(1)}%</div><div className="muted">Host CPU</div></div><div className="card"><div className="card-num">{formatBytes(metricsQuery.data.host.memory_used_bytes)}</div><div className="muted">Memory used of {formatBytes(metricsQuery.data.host.memory_total_bytes)}</div></div><div className="card"><div className="card-num">{formatBytes(metricsQuery.data.host.disk_free_bytes)}</div><div className="muted">Disk available</div></div></div></>}

          <div className="section-heading"><h3>Recent activity</h3><Link className="section-link" to="/monitoring">All monitoring</Link></div>
          {recentTaskQueries.some((query) => query.isLoading) && <div className="panel"><p className="muted">Loading recent activity…</p></div>}
          {!recentTaskQueries.some((query) => query.isLoading) && recentActivity.length === 0 && <div className="panel"><p className="muted">No tasks have run recently.</p></div>}
          {recentActivity.length > 0 && <ul className="activity-list">{recentActivity.slice(0, 8).map(({ serverName, task }) => <li key={task.id}><span className="badge">{task.state}</span><span className="activity-kind">{task.kind}</span><strong>{serverName}</strong><span className="muted">{task.progress_percent !== null && task.progress_percent !== undefined ? `${task.progress_percent}% ` : ""}{task.message ?? task.error ?? ""}</span></li>)}</ul>}

          <div className="section-heading"><h3>Monitoring alerts</h3><Link className="section-link" to="/monitoring">Open monitoring</Link></div>
          {backendAlertsQuery.isLoading && <div className="panel"><p className="muted">Loading monitoring alerts…</p></div>}
          {backendAlertsQuery.error && <div className="panel"><p className="err">Failed to load monitoring alerts.</p></div>}
          {backendAlertsQuery.data && <div className="panel"><AlertsList alerts={backendAlertsQuery.data.alerts} /></div>}
        </>
      )}
    </section>
  );
}
