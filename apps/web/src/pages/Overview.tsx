import { useState } from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import Badge from "../components/ui/Badge";
import Button from "../components/ui/Button";
import Card from "../components/ui/Card";
import EmptyState from "../components/ui/EmptyState";
import ErrorState from "../components/ui/ErrorState";
import Meter from "../components/ui/Meter";
import PageHeader from "../components/ui/PageHeader";
import { SkeletonCard, SkeletonText } from "../components/ui/Skeleton";
import StatCard from "../components/ui/StatCard";
import ServerCard from "../components/servers/ServerCard";
import CreateServerDialog from "../components/servers/CreateServerDialog";
import { usePageTitle } from "../hooks/usePageTitle";
import { formatBytes, formatPercent } from "../lib/format";
import {
  fetchMetricsSummary,
  fetchServerTasks,
  fetchStatus,
  listServers,
} from "../lib/servers";
import type {
  ServerMetric,
  ServerStatus,
  ServerSummary,
} from "../lib/servers";
import { fetchAlerts } from "../lib/monitoring";
import type { BackendAlert } from "../lib/monitoring";

interface FleetAlert {
  id: string;
  tone: "warning" | "danger";
  text: string;
  serverId?: string;
}

function buildFleetAlerts(
  servers: ServerSummary[],
  statuses: Map<string, ServerStatus | undefined>,
  statusFailed: Set<string>,
  backendAlerts: BackendAlert[],
  alertsFailed: boolean,
): FleetAlert[] {
  const alerts: FleetAlert[] = [];
  const seen = new Set<string>();

  function push(alert: FleetAlert): void {
    if (seen.has(alert.text)) return;
    seen.add(alert.text);
    alerts.push(alert);
  }

  for (const server of servers) {
    if (server.desired_state !== server.observed_state) {
      push({
        id: `drift-${server.id}`,
        tone: "warning",
        text: `${server.name} is ${server.observed_state} but should be ${server.desired_state}.`,
        serverId: server.id,
      });
    }
    const status = statuses.get(server.id);
    if (status?.docker_available === false) {
      push({
        id: `docker-${server.id}`,
        tone: "danger",
        text: `${server.name} cannot reach Docker.`,
        serverId: server.id,
      });
    }
    if (status?.container === null) {
      push({
        id: `container-${server.id}`,
        tone: "danger",
        text: `${server.name} is missing its container.`,
        serverId: server.id,
      });
    }
    if (statusFailed.has(server.id)) {
      push({
        id: `status-${server.id}`,
        tone: "warning",
        text: `Live status check failed for ${server.name}.`,
        serverId: server.id,
      });
    }
  }

  for (const [index, alert] of backendAlerts.entries()) {
    push({
      id: `backend-${index}`,
      tone: /critical|error|err/i.test(alert.severity)
        ? "danger"
        : "warning",
      text: alert.message,
      serverId: alert.server_id ?? undefined,
    });
  }

  if (alertsFailed) {
    push({
      id: "alerts-failed",
      tone: "warning",
      text: "Monitoring alerts could not be loaded.",
    });
  }

  return alerts;
}

export default function Overview(): React.JSX.Element {
  usePageTitle("Dashboard");
  const [createOpen, setCreateOpen] = useState(false);

  const serversQuery = useQuery({
    queryKey: ["servers"],
    queryFn: listServers,
  });
  const metricsQuery = useQuery({
    queryKey: ["metrics", "summary"],
    queryFn: fetchMetricsSummary,
    retry: 1,
  });
  const alertsQuery = useQuery({
    queryKey: ["alerts"],
    queryFn: fetchAlerts,
    retry: 1,
  });

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

  const statusById = new Map<string, ServerStatus | undefined>();
  const statusFailed = new Set<string>();
  servers.forEach((server, index) => {
    statusById.set(server.id, statusQueries[index]?.data);
    if (statusQueries[index]?.isError) statusFailed.add(server.id);
  });

  const metricById = new Map<string, ServerMetric>(
    (metricsQuery.data?.servers ?? []).map((metric) => [
      metric.server_id,
      metric,
    ]),
  );

  const alerts = buildFleetAlerts(
    servers,
    statusById,
    statusFailed,
    alertsQuery.data?.alerts ?? [],
    alertsQuery.isError,
  );
  const onlineCount = servers.filter(
    (server) => statusById.get(server.id)?.readiness?.state === "online",
  ).length;
  const driftedCount = servers.filter(
    (server) => server.desired_state !== server.observed_state,
  ).length;
  const playersOnline = servers.reduce(
    (total, server) =>
      total +
      (statusById.get(server.id)?.readiness?.ping?.players_online ?? 0),
    0,
  );
  const recentTasks = servers.flatMap((server, index) =>
    (recentTaskQueries[index]?.data?.tasks ?? []).map((task) => ({
      server,
      task,
    })),
  );
  const host = metricsQuery.data?.host;
  const diskTone =
    host !== undefined &&
    host.disk_total_bytes > 0 &&
    host.disk_free_bytes / host.disk_total_bytes < 0.15
      ? "danger"
      : "success";

  return (
    <section>
      <PageHeader
        title="Dashboard"
        description="Your servers, host capacity, and open alerts at a glance."
        actions={
          <Button onClick={() => setCreateOpen(true)}>New server</Button>
        }
      />

      {serversQuery.isLoading && (
        <>
          <div className="stat-grid">
            {Array.from({ length: 4 }, (_, index) => (
              <SkeletonCard key={index} />
            ))}
          </div>
          <div className="server-grid">
            {Array.from({ length: 2 }, (_, index) => (
              <SkeletonCard key={index} />
            ))}
          </div>
        </>
      )}

      {serversQuery.error && (
        <ErrorState
          message="Could not load your servers."
          technicalDetails={
            serversQuery.error instanceof Error
              ? serversQuery.error.message
              : undefined
          }
          onRetry={() => void serversQuery.refetch()}
        />
      )}

      {!serversQuery.isLoading &&
        !serversQuery.error &&
        servers.length === 0 && (
          <EmptyState
            title="No servers yet"
            description="Create your first Pumpkin server to see live status, players, host capacity, and backups here."
            action={
              <Button onClick={() => setCreateOpen(true)}>
                Create your first server
              </Button>
            }
          />
        )}

      {!serversQuery.isLoading &&
        !serversQuery.error &&
        servers.length > 0 && (
          <>
            <div className="stat-grid">
              <StatCard
                label="Servers"
                value={servers.length}
                detail="Registered in this panel"
                accent
              />
              <StatCard
                label="Online"
                value={onlineCount}
                detail="Answering Minecraft pings"
                tone="success"
              />
              <StatCard
                label="Needs attention"
                value={alerts.length}
                detail={
                  driftedCount > 0
                    ? `${driftedCount} state drift`
                    : "No state drift"
                }
                tone={
                  alerts.length === 0
                    ? "default"
                    : alerts.some((alert) => alert.tone === "danger")
                      ? "danger"
                      : "warning"
                }
              />
              <StatCard
                label="Players online"
                value={playersOnline}
                detail="Across online servers"
              />
            </div>

            <div className="dashboard-panels">
              <Card
                title="Attention"
                actions={
                  <Link className="section-link" to="/monitoring">
                    View monitoring
                  </Link>
                }
              >
                {alerts.length === 0 ? (
                  <div className="attention-empty">
                    <strong>All clear</strong>
                    <span>No server needs attention right now.</span>
                  </div>
                ) : (
                  <>
                    <ul className="attention-list">
                      {alerts.slice(0, 5).map((alert) => (
                        <li
                          key={alert.id}
                          className={
                            alert.tone === "danger"
                              ? "attention-danger"
                              : ""
                          }
                        >
                          <div className="alert-row">
                            <Badge tone={alert.tone}>
                              {alert.tone === "danger"
                                ? "Critical"
                                : "Warning"}
                            </Badge>
                            <span>{alert.text}</span>
                          </div>
                          {alert.serverId !== undefined && (
                            <Link
                              to={`/servers/${encodeURIComponent(alert.serverId)}`}
                            >
                              Open server
                            </Link>
                          )}
                        </li>
                      ))}
                    </ul>
                    {alerts.length > 5 && (
                      <p className="muted">
                        +{alerts.length - 5} more alerts.
                      </p>
                    )}
                  </>
                )}
              </Card>

              <Card
                title="Host capacity"
                actions={
                  <Link className="section-link" to="/monitoring">
                    Details
                  </Link>
                }
              >
                {metricsQuery.isLoading && <SkeletonText lines={3} />}
                {metricsQuery.error && (
                  <ErrorState
                    message="Could not load host capacity."
                    technicalDetails={
                      metricsQuery.error instanceof Error
                        ? metricsQuery.error.message
                        : undefined
                    }
                    onRetry={() => void metricsQuery.refetch()}
                  />
                )}
                {host !== undefined && (
                  <div className="capacity-grid">
                    <Meter
                      label="CPU"
                      value={host.cpu_percent}
                      max={100}
                      valueLabel={formatPercent(host.cpu_percent)}
                    />
                    <Meter
                      label="Memory"
                      value={host.memory_used_bytes}
                      max={host.memory_total_bytes}
                      valueLabel={`${formatBytes(host.memory_used_bytes)} / ${formatBytes(host.memory_total_bytes)}`}
                      tone="warning"
                    />
                    <Meter
                      label="Disk free"
                      value={host.disk_free_bytes}
                      max={host.disk_total_bytes}
                      valueLabel={`${formatBytes(host.disk_free_bytes)} free of ${formatBytes(host.disk_total_bytes)}`}
                      tone={diskTone}
                    />
                  </div>
                )}
              </Card>
            </div>

            <div className="section-heading">
              <h2>Your servers</h2>
              <Link className="section-link" to="/servers">
                All servers
              </Link>
            </div>
            <div className="server-grid">
              {servers.map((server, index) => (
                <ServerCard
                  key={server.id}
                  server={server}
                  status={statusById.get(server.id)}
                  statusLoading={statusQueries[index]?.isLoading}
                  metric={metricById.get(server.id)}
                />
              ))}
            </div>

            <div className="section-heading">
              <h2>Recent activity</h2>
              <Link className="section-link" to="/monitoring">
                Monitoring
              </Link>
            </div>
            {recentTaskQueries.some((query) => query.isLoading) ? (
              <Card>
                <SkeletonText lines={3} />
              </Card>
            ) : recentTasks.length === 0 ? (
              <Card>
                <p className="muted">No tasks have run yet.</p>
              </Card>
            ) : (
              <ul className="activity-list">
                {recentTasks.slice(0, 8).map(({ server, task }) => (
                  <li key={`${server.id}-${task.id}`}>
                    <Badge
                      tone={
                        task.state === "succeeded"
                          ? "success"
                          : task.state === "failed"
                            ? "danger"
                            : "neutral"
                      }
                    >
                      {task.state}
                    </Badge>
                    <span className="activity-kind">{task.kind}</span>
                    <strong>{server.name}</strong>
                    <span className="muted">
                      {task.progress_percent !== null
                        ? `${task.progress_percent}% `
                        : ""}
                      {task.message ?? task.error ?? ""}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </>
        )}

      <CreateServerDialog
        open={createOpen}
        onClose={() => setCreateOpen(false)}
      />
    </section>
  );
}
