import Badge from "./Badge";
import type { BadgeTone } from "./Badge";

const KNOWN_STATES: Record<string, { label: string; tone: BadgeTone }> = {
  online: { label: "Online", tone: "success" },
  running: { label: "Running", tone: "success" },
  starting: { label: "Starting", tone: "warning" },
  stopping: { label: "Stopping", tone: "warning" },
  stopped: { label: "Stopped", tone: "neutral" },
  created: { label: "Created", tone: "neutral" },
  unhealthy: { label: "Unhealthy", tone: "danger" },
  missing: { label: "Missing", tone: "danger" },
  exited: { label: "Exited", tone: "danger" },
  unknown: { label: "Unknown", tone: "neutral" },
};

function titleCase(value: string): string {
  if (!value) return "Unknown";
  return value.charAt(0).toUpperCase() + value.slice(1).replace(/_/g, " ");
}

export interface StatusPillProps {
  state: string;
  label?: string;
  title?: string;
  className?: string;
}

export default function StatusPill({
  state,
  label,
  title,
  className,
}: StatusPillProps): React.JSX.Element {
  const known = KNOWN_STATES[state.toLowerCase()];

  return (
    <Badge
      tone={known?.tone ?? "neutral"}
      title={title}
      className={className}
    >
      <span className="status-dot" aria-hidden="true" />
      {label ?? known?.label ?? titleCase(state)}
    </Badge>
  );
}
