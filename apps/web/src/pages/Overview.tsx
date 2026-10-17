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
  if (!Number.isFinite(bytes) || bytes < 0) return "Unavailable";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
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
    created: "Created",
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

function shelfClass(state: string): string {
  if (state === "online") return "server-row server-online";
  if (state === "unhealthy" || state === "missing") return "server-row server-error";
  if (state === "starting" || state === "stopping") return "server-row server-warning";
  return "server-row";
}

function ReadinessBadge({ readiness, observed }: { readiness?: ReadinessInfo | null; observed: string }): React.JSX.Element {
  const state = readiness?.state ?? observed;
  return <span className={readinessClass(state)} title={readiness?.reason}>{stateLabel(state)}</span>;
}

type Alert = { level: "err" | "warn"; text: string };

function buildAlerts(
  servers: ServerSummary[],
  statuses: Map<string, { dockerAvailable: boolean; containerMissing: boolean }>,
  statusFailed: boolean,
): Alert[] {
  const alerts: Alert[] = [];
  for (const server of servers) {
    if (digestWarning(server.image_reference) !== null) {
      alerts.push({ level: "warn", text: `${server.name} uses an unpinned image reference.` });
    }
    if (server.desired_state !== server.observed_state) {
      alerts.push({ level: "warn", text: `${server.name} has desired state drift.` });
    }
    const status = statuses.get(server.id);
    if (status?.dockerAvailable === false) alerts.push({ level: "err", text: `${server.name} cannot reach Docker.` });
    if (status?.containerMissing) alerts.push({ level: "err", text: `${server.name} is missing its container.` });
  }
  if (statusFailed && servers.length > 0) alerts.push({ level: "warn", text: "Some live status checks need another look." });
  return alerts;
}

function MetricCard({ value, label, detail }: { value: string | number; label: string; detail: string }): React.JSX.Element {
  return <div className="metric-card"><div className="card-num">{value}</div><div className="metric-label">{label}</div><div className="metric-detail">{detail}</div></div>;
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
      refetchInterval: 15_000,
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
  const readinessStates = servers.map((server, index) => statusQueries[index]?.data?.readiness?.state ?? server.observed_state);
  const onlineCount = readinessStates.filter((state) => state === "online").length;
  const activeCount = readinessStates.filter((state) => state === "online" || state === "starting" || state === "stopping").length;
  const drifted = servers.filter((server) => server.desired_state !== server.observed_state).length;
  const recentActivity: Array<{ serverName: string; task: ServerTask }> = [];
  servers.forEach((server, index) => {
    for (const task of recentTaskQueries[index]?.data?.tasks ?? []) recentActivity.push({ serverName: server.name, task });
  });

  return (
    <section>
      <div className="overview-hero">
        <div className="overview-intro">
          <div className="eyebrow">Home node · Pumpkin runtime</div>
          <h2>Your worlds, in one calm control room.</h2>
          <p>See what is online, what needs attention, and what is happening across your Minecraft worlds.</p>
          <div className="heading-actions">
            <Link className="button-link" to="/servers#create-server">Create a server</Link>
            <Link className="text-link" to="/monitoring">Open monitoring</Link>
          </div>
        </div>
        <div className="node-card">
          <div className="node-card-header"><h3>Home node</h3><span className="node-chip"><span className="status-dot" />Docker connected</span></div>
          <p className="muted">One protected control plane for your Pumpkin instances.</p>
          <div className="node-card-metrics">
            <div><strong>{servers.length}</strong><span>worlds registered</span></div>
            <div><strong>{onlineCount}</strong><span>online now</span></div>
          </div>
        </div>
      </div>

      {serversQuery.isLoading && <div className="panel loading-panel"><p className="muted">Loading your worlds…</p></div>}
      {serversQuery.error && <div className="panel error-panel"><p className="err" role="alert">The server list could not be loaded. Refresh to try again.</p></div>}

      {!serversQuery.isLoading && !serversQuery.error && servers.length === 0 && (
        <div className="empty-state">
          <div className="eyebrow">First world</div>
          <h3>Your node is ready for its first world.</h3>
          <p>Create a Pumpkin server to start seeing readiness, logs, backups, and resource usage here.</p>
          <Link className="button-link" to="/servers#create-server">Create your first server</Link>
        </div>
      )}

      {servers.length > 0 && (
        <>
          <div className="metric-grid" aria-label="Server summary">
            <MetricCard value={servers.length} label="Worlds" detail="Registered on this node" />
            <MetricCard value={onlineCount} label="Online" detail={`${activeCount} currently active or starting`} />
            <MetricCard value={drifted} label="Needs attention" detail="Desired state differs from reality" />
            <MetricCard value={alerts.length} label="Open signals" detail="Warnings and runtime issues" />
          </div>

          <div className="overview-grid">
            <div className="panel attention-panel">
              <div className="section-heading"><h3>Attention</h3><Link className="section-link" to="/monitoring">View all</Link></div>
              {alerts.length === 0 ? <div className="attention-empty"><strong>All clear</strong><span>No world issues require action.</span></div> : <ul className="attention-list">{alerts.slice(0, 5).map((alert, index) => <li key={`${alert.text}-${index}`} className={alert.level}><span>{alert.text}</span><Link to="/monitoring">Review signal</Link></li>)}</ul>}
            </div>
            <div className="server-shelf">
              <div className="server-shelf-header"><strong>Your worlds</strong><span>{servers.length} registered</span></div>
              {servers.map((server, index) => {
                const status = statusQueries[index]?.data;
                const readiness = status?.readiness;
                const state = readiness?.state ?? server.observed_state;
                return <div className={shelfClass(state)} key={server.id}>
                  <div><Link className="server-name" to={`/servers/${encodeURIComponent(server.id)}`}>{server.name}</Link><div className="server-meta">Java {server.game_port} · desired {stateLabel(server.desired_state)}</div></div>
                  <div className="server-state">{statusQueries[index]?.isLoading ? <span className="muted">Checking…</span> : <ReadinessBadge readiness={readiness} observed={server.observed_state} />}</div>
                  <div className="server-stat"><strong>{readiness?.ping ? `${readiness.ping.players_online}/${readiness.ping.players_max}` : "—"}</strong><small>players</small></div>
                  <Link className="section-link" to={`/servers/${encodeURIComponent(server.id)}`}>Open</Link>
                </div>;
              })}
            </div>
          </div>

          {metricsQuery.data && <><div className="section-heading"><h3>Node capacity</h3><Link className="section-link" to="/monitoring">View monitoring</Link></div><div className="host-summary"><div className="card"><div className="card-num">{metricsQuery.data.host.cpu_percent.toFixed(1)}%</div><div className="muted">Host CPU</div></div><div className="card"><div className="card-num">{formatBytes(metricsQuery.data.host.memory_used_bytes)}</div><div className="muted">Memory used of {formatBytes(metricsQuery.data.host.memory_total_bytes)}</div></div><div className="card"><div className="card-num">{formatBytes(metricsQuery.data.host.disk_free_bytes)}</div><div className="muted">Disk available</div></div></div></>}

          <div className="section-heading"><h3>Recent activity</h3><Link className="section-link" to="/monitoring">All activity</Link></div>
          {recentTaskQueries.some((query) => query.isLoading) && <div className="panel"><p className="muted">Loading recent activity…</p></div>}
          {!recentTaskQueries.some((query) => query.isLoading) && recentActivity.length === 0 && <div className="panel"><p className="muted">No operations have run recently.</p></div>}
          {recentActivity.length > 0 && <ul className="activity-list">{recentActivity.slice(0, 8).map(({ serverName, task }) => <li key={task.id}><span className="badge">{stateLabel(task.state)}</span><span className="activity-kind">{task.kind}</span><strong>{serverName}</strong><span className="muted">{task.progress_percent !== null && task.progress_percent !== undefined ? `${task.progress_percent}% ` : ""}{task.message ?? task.error ?? ""}</span></li>)}</ul>}

          <div className="section-heading"><h3>Runtime signals</h3><Link className="section-link" to="/monitoring">Open monitoring</Link></div>
          {backendAlertsQuery.isLoading && <div className="panel"><p className="muted">Loading runtime signals…</p></div>}
          {backendAlertsQuery.error && <div className="panel"><p className="err">Runtime signals could not be loaded.</p></div>}
          {backendAlertsQuery.data && <div className="panel"><AlertsList alerts={backendAlertsQuery.data.alerts} /></div>}
        </>
      )}
    </section>
  );
}
