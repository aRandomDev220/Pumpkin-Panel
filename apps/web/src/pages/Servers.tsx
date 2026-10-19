import { useState } from "react";
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import Button from "../components/ui/Button";
import ConfirmDialog from "../components/ui/ConfirmDialog";
import EmptyState from "../components/ui/EmptyState";
import ErrorState from "../components/ui/ErrorState";
import { Input } from "../components/ui/Field";
import PageHeader from "../components/ui/PageHeader";
import { SkeletonCard } from "../components/ui/Skeleton";
import { useToast } from "../components/ui/toast-context";
import CreateServerDialog from "../components/servers/CreateServerDialog";
import ServerCard from "../components/servers/ServerCard";
import { usePageTitle } from "../hooks/usePageTitle";
import { humanizeError } from "../lib/errors";
import {
  deleteServer,
  fetchMetricsSummary,
  fetchStatus,
  listServers,
} from "../lib/servers";
import type {
  ServerMetric,
  ServerStatus,
  ServerSummary,
} from "../lib/servers";

type StatusFilter = "all" | "online" | "stopped" | "attention";

const FILTERS: Array<{ value: StatusFilter; label: string }> = [
  { value: "all", label: "All" },
  { value: "online", label: "Online" },
  { value: "stopped", label: "Stopped" },
  { value: "attention", label: "Needs attention" },
];

export default function Servers(): React.JSX.Element {
  usePageTitle("Servers");
  const queryClient = useQueryClient();
  const toast = useToast();
  const [createOpen, setCreateOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<StatusFilter>("all");
  const [deleteTarget, setDeleteTarget] =
    useState<ServerSummary | null>(null);
  const [keepData, setKeepData] = useState(true);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const serversQuery = useQuery({
    queryKey: ["servers"],
    queryFn: listServers,
  });
  const metricsQuery = useQuery({
    queryKey: ["metrics", "summary"],
    queryFn: fetchMetricsSummary,
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

  const statusById = new Map<string, ServerStatus | undefined>();
  const statusFailed = new Set<string>();
  const indexById = new Map<string, number>();
  servers.forEach((server, index) => {
    indexById.set(server.id, index);
    statusById.set(server.id, statusQueries[index]?.data);
    if (statusQueries[index]?.isError) statusFailed.add(server.id);
  });

  const metricById = new Map<string, ServerMetric>(
    (metricsQuery.data?.servers ?? []).map((metric) => [
      metric.server_id,
      metric,
    ]),
  );

  const deleteMutation = useMutation({
    mutationFn: ({ id, retain }: { id: string; retain: boolean }) =>
      deleteServer(id, retain),
    onSuccess: (_data, variables) => {
      const name = servers.find(
        (server) => server.id === variables.id,
      )?.name;
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
      void queryClient.invalidateQueries({
        queryKey: ["metrics", "summary"],
      });
      void queryClient.invalidateQueries({ queryKey: ["alerts"] });
      toast.success(
        name !== undefined ? `Server “${name}” deleted` : "Server deleted",
      );
      setDeleteTarget(null);
      setDeleteError(null);
    },
    onError: (error) => {
      setDeleteError(humanizeError(error));
    },
  });

  const needle = search.trim().toLowerCase();
  const filtered = servers.filter((server) => {
    const status = statusById.get(server.id);
    const readiness = status?.readiness?.state;
    const observed = readiness ?? server.observed_state;

    if (filter === "online" && observed !== "online") return false;
    if (filter === "stopped" && observed !== "stopped") return false;
    if (filter === "attention") {
      const needsAttention =
        statusFailed.has(server.id) ||
        status?.docker_available === false ||
        status?.container === null ||
        server.desired_state !== server.observed_state ||
        readiness === "unhealthy";
      if (!needsAttention) return false;
    }

    if (needle === "") return true;
    return `${server.name} ${server.game_port} ${server.image_reference}`
      .toLowerCase()
      .includes(needle);
  });

  return (
    <section>
      <PageHeader
        title="Servers"
        description="Create, operate, and inspect every Pumpkin world from one place."
        actions={
          <Button onClick={() => setCreateOpen(true)}>New server</Button>
        }
      />

      {serversQuery.isLoading && (
        <div className="server-grid">
          {Array.from({ length: 4 }, (_, index) => (
            <SkeletonCard key={index} />
          ))}
        </div>
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
            description="Your first server will appear here with live readiness, players, resources, and backups."
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
            <div className="fleet-toolbar">
              <div className="toolbar-search">
                <Input
                  type="search"
                  aria-label="Search servers"
                  placeholder="Search by name, port, or image"
                  value={search}
                  onChange={(event) => setSearch(event.target.value)}
                />
              </div>
              <div
                className="filter-group"
                role="group"
                aria-label="Filter servers by status"
              >
                {FILTERS.map((item) => (
                  <Button
                    key={item.value}
                    size="sm"
                    variant={
                      filter === item.value ? "primary" : "secondary"
                    }
                    aria-pressed={filter === item.value}
                    onClick={() => setFilter(item.value)}
                  >
                    {item.label}
                  </Button>
                ))}
              </div>
              <span className="muted toolbar-count">
                {filtered.length} of {servers.length}
              </span>
            </div>

            {filtered.length === 0 ? (
              <EmptyState
                title="No servers match"
                description="Try a different search or status filter."
                action={
                  <Button
                    variant="secondary"
                    onClick={() => {
                      setSearch("");
                      setFilter("all");
                    }}
                  >
                    Clear filters
                  </Button>
                }
              />
            ) : (
              <div className="server-grid">
                {filtered.map((server) => {
                  const index = indexById.get(server.id) ?? 0;
                  return (
                    <ServerCard
                      key={server.id}
                      server={server}
                      status={statusById.get(server.id)}
                      statusLoading={statusQueries[index]?.isLoading}
                      metric={metricById.get(server.id)}
                      onDelete={(target) => {
                        setDeleteTarget(target);
                        setKeepData(true);
                        setDeleteError(null);
                      }}
                    />
                  );
                })}
              </div>
            )}
          </>
        )}

      <CreateServerDialog
        open={createOpen}
        onClose={() => setCreateOpen(false)}
      />

      <ConfirmDialog
        open={deleteTarget !== null}
        onClose={() => {
          if (deleteMutation.isPending) return;
          setDeleteTarget(null);
          setDeleteError(null);
        }}
        onConfirm={() => {
          if (deleteTarget !== null) {
            deleteMutation.mutate({
              id: deleteTarget.id,
              retain: keepData,
            });
          }
        }}
        title="Delete server"
        description={
          deleteTarget !== null
            ? `Remove “${deleteTarget.name}” and its container from the panel.`
            : undefined
        }
        confirmLabel="Delete server"
        busy={deleteMutation.isPending}
        checkbox={{
          label: "Keep world data",
          hint: "Leave checked to keep the world files on disk.",
          checked: keepData,
          onChange: setKeepData,
        }}
      >
        {deleteError !== null && (
          <p className="err" role="alert">
            {deleteError}
          </p>
        )}
      </ConfirmDialog>
    </section>
  );
}
