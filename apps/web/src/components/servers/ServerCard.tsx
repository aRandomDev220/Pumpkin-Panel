import { useEffect, useId, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { MoreHorizontal, Terminal, Trash2 } from "lucide-react";
import Badge from "../ui/Badge";
import Button from "../ui/Button";
import IconButton from "../ui/IconButton";
import StatusPill from "../ui/StatusPill";
import { useLifecycle } from "../../hooks/useLifecycle";
import { formatBytes, formatPercent } from "../../lib/format";
import type {
  ServerMetric,
  ServerStatus,
  ServerSummary,
} from "../../lib/servers";
import LifecycleButtons from "./LifecycleButtons";

function ServerMenu({
  server,
  onDelete,
}: {
  server: ServerSummary;
  onDelete: (server: ServerSummary) => void;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const container = containerRef.current;
      if (container !== null && !container.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  return (
    <div className="card-menu" ref={containerRef}>
      <IconButton
        label={`Actions for ${server.name}`}
        size="sm"
        aria-haspopup="menu"
        aria-expanded={open}
        icon={<MoreHorizontal size={16} />}
        onClick={() => setOpen((current) => !current)}
      />
      {open && (
        <div className="card-menu-panel" role="menu">
          <Link
            role="menuitem"
            to={`/servers/${encodeURIComponent(server.id)}`}
            onClick={() => setOpen(false)}
          >
            <Terminal size={14} />
            Open console
          </Link>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false);
              onDelete(server);
            }}
          >
            <Trash2 size={14} />
            Delete server
          </button>
        </div>
      )}
    </div>
  );
}

export interface ServerCardProps {
  server: ServerSummary;
  status?: ServerStatus;
  statusLoading?: boolean;
  metric?: ServerMetric;
  onDelete?: (server: ServerSummary) => void;
}

export default function ServerCard({
  server,
  status,
  statusLoading = false,
  metric,
  onDelete,
}: ServerCardProps): React.JSX.Element {
  const nameId = useId();
  const lifecycle = useLifecycle(server.id, server.name);
  const readiness = status?.readiness;
  const state = readiness?.state ?? server.observed_state;
  const running =
    state === "online" ||
    state === "running" ||
    (state !== "stopped" && server.desired_state === "running");
  const drifted = server.desired_state !== server.observed_state;
  const ping = readiness?.ping;

  return (
    <article className="server-card" aria-labelledby={nameId}>
      <div className="server-card-header">
        <div className="server-card-identity">
          <Link
            id={nameId}
            className="server-card-name"
            to={`/servers/${encodeURIComponent(server.id)}`}
          >
            {server.name}
          </Link>
          <div className="server-card-meta">
            Port {server.game_port} ·{" "}
            <code title={server.image_reference}>
              {server.image_reference}
            </code>
          </div>
        </div>
        <div className="server-card-status">
          {statusLoading && status === undefined ? (
            <StatusPill state="unknown" label="Checking" />
          ) : (
            <StatusPill state={state} title={readiness?.reason} />
          )}
        </div>
      </div>

      {(drifted ||
        status?.docker_available === false ||
        status?.container === null) && (
        <div className="server-card-flags">
          {drifted && <Badge tone="warning">State drift</Badge>}
          {status?.docker_available === false && (
            <Badge tone="danger">Docker offline</Badge>
          )}
          {status?.container === null && (
            <Badge tone="danger">Container missing</Badge>
          )}
        </div>
      )}

      <dl className="server-card-stats">
        <div>
          <dt>Players</dt>
          <dd>
            {ping
              ? `${ping.players_online}/${ping.players_max}`
              : "—"}
          </dd>
        </div>
        <div>
          <dt>CPU</dt>
          <dd>
            {metric?.cpu_percent !== null &&
            metric?.cpu_percent !== undefined
              ? formatPercent(metric.cpu_percent)
              : "—"}
          </dd>
        </div>
        <div>
          <dt>Memory</dt>
          <dd>
            {metric?.memory_bytes !== null &&
            metric?.memory_bytes !== undefined
              ? formatBytes(metric.memory_bytes)
              : "—"}
          </dd>
        </div>
      </dl>

      <div className="server-card-actions">
        <Button
          to={`/servers/${encodeURIComponent(server.id)}`}
          size="sm"
          variant="secondary"
          icon={<Terminal size={14} />}
        >
          Open console
        </Button>
        <LifecycleButtons
          lifecycle={lifecycle}
          serverName={server.name}
          running={running}
        />
        {onDelete !== undefined && (
          <ServerMenu server={server} onDelete={onDelete} />
        )}
      </div>
    </article>
  );
}
