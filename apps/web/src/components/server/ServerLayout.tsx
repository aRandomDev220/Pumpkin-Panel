import { useQuery } from "@tanstack/react-query";
import { Outlet, useLocation, useParams } from "react-router-dom";
import type { LifecycleController } from "../../hooks/useLifecycle";
import { useLifecycle } from "../../hooks/useLifecycle";
import { usePageTitle } from "../../hooks/usePageTitle";
import { formatBytes, formatPercent } from "../../lib/format";
import {
  fetchMetricsSummary,
  fetchStatus,
  getServer,
} from "../../lib/servers";
import type {
  ServerDetail,
  ServerMetric,
  ServerStatus,
} from "../../lib/servers";
import Breadcrumbs from "../ui/Breadcrumbs";
import ErrorState from "../ui/ErrorState";
import { SkeletonCard } from "../ui/Skeleton";
import StatusPill from "../ui/StatusPill";
import LifecycleButtons from "../servers/LifecycleButtons";
import ServerTabs from "./ServerTabs";

export interface ServerOutletContext {
  server: ServerDetail;
  status: ServerStatus | undefined;
  statusLoading: boolean;
  lifecycle: LifecycleController;
  metric: ServerMetric | undefined;
}

function tabLabel(pathname: string): string {
  if (pathname.includes("/files")) return "Files";
  if (pathname.includes("/config")) return "Config";
  if (pathname.includes("/backups")) return "Backups";
  if (pathname.includes("/settings")) return "Settings";
  return "Console";
}

function serverRunning(
  server: ServerDetail,
  state: string | undefined,
): boolean {
  return (
    state === "online" ||
    state === "running" ||
    (state !== "stopped" && server.desired_state === "running")
  );
}

function ResourceItem({
  label,
  value,
}: {
  label: string;
  value: string;
}): React.JSX.Element {
  return (
    <div className="resource-item">
      <span className="resource-label">{label}</span>
      <strong className="resource-value">{value}</strong>
    </div>
  );
}

export default function ServerLayout(): React.JSX.Element {
  const { id } = useParams<{ id: string }>();
  const location = useLocation();

  const serverQuery = useQuery({
    queryKey: ["servers", id],
    queryFn: () => getServer(id ?? ""),
    enabled: id !== undefined && id !== "",
  });
  const statusQuery = useQuery({
    queryKey: ["servers", id, "status"],
    queryFn: () => fetchStatus(id ?? ""),
    enabled: id !== undefined && id !== "",
    refetchInterval: 15_000,
    retry: 1,
  });
  const metricsQuery = useQuery({
    queryKey: ["metrics", "summary"],
    queryFn: fetchMetricsSummary,
    retry: 1,
    staleTime: 10_000,
  });

  const server = serverQuery.data;
  const lifecycle = useLifecycle(id ?? "", server?.name);
  usePageTitle(
    server !== undefined
      ? `${server.name} — ${tabLabel(location.pathname)}`
      : "Server",
  );

  if (id === undefined || id === "") {
    return <ErrorState message="Missing server id." />;
  }

  if (serverQuery.isLoading) {
    return (
      <div className="server-loading">
        <SkeletonCard />
        <SkeletonCard />
      </div>
    );
  }

  if (serverQuery.error || server === undefined) {
    return (
      <ErrorState
        title="Could not load this server"
        message="The server may have been deleted, or the manager cannot reach it."
        technicalDetails={
          serverQuery.error instanceof Error
            ? serverQuery.error.message
            : undefined
        }
        onRetry={() => void serverQuery.refetch()}
      />
    );
  }

  const status = statusQuery.data;
  const readiness = status?.readiness;
  const state = readiness?.state ?? server.observed_state;
  const ping = readiness?.ping;
  const metric = metricsQuery.data?.servers.find(
    (item) => item.server_id === server.id,
  );

  return (
    <section className="server-layout">
      <Breadcrumbs
        items={[
          { label: "Servers", to: "/servers" },
          { label: server.name },
        ]}
      />

      <header className="server-header">
        <div className="server-header-main">
          <div className="server-header-title">
            <h1>{server.name}</h1>
            {statusQuery.isLoading && status === undefined ? (
              <StatusPill state="unknown" label="Checking" />
            ) : (
              <StatusPill state={state} title={readiness?.reason} />
            )}
          </div>
          <div className="server-header-meta">
            Port {server.game_port} ·{" "}
            <code>{server.image_reference}</code>
          </div>
        </div>
        <div className="server-header-actions">
          <LifecycleButtons
            lifecycle={lifecycle}
            serverName={server.name}
            running={serverRunning(server, state)}
          />
        </div>
      </header>

      <div
        className="resource-strip"
        role="group"
        aria-label="Server resources"
      >
        <ResourceItem
          label="CPU"
          value={
            metric?.cpu_percent !== null &&
            metric?.cpu_percent !== undefined
              ? formatPercent(metric.cpu_percent)
              : "—"
          }
        />
        <ResourceItem
          label="Memory"
          value={
            metric?.memory_bytes !== null &&
            metric?.memory_bytes !== undefined
              ? formatBytes(metric.memory_bytes)
              : "—"
          }
        />
        <ResourceItem
          label="Players"
          value={
            ping !== null && ping !== undefined
              ? `${ping.players_online}/${ping.players_max}`
              : "—"
          }
        />
        <ResourceItem
          label="Ping"
          value={
            ping !== null && ping !== undefined
              ? `${ping.latency_ms} ms`
              : "—"
          }
        />
      </div>

      <ServerTabs serverId={server.id} />

      <Outlet
        context={{
          server,
          status,
          statusLoading: statusQuery.isLoading,
          lifecycle,
          metric,
        }}
      />
    </section>
  );
}
