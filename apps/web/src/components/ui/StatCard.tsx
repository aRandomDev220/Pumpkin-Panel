import type { ReactNode } from "react";

export interface StatCardProps {
  label: ReactNode;
  value: ReactNode;
  detail?: ReactNode;
  tone?: "default" | "success" | "warning" | "danger";
  accent?: boolean;
}

export default function StatCard({
  label,
  value,
  detail,
  tone = "default",
  accent = false,
}: StatCardProps): React.JSX.Element {
  const classes = [
    "stat-card",
    tone === "default" ? "" : `stat-card-${tone}`,
    accent ? "stat-card-accent" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={classes}>
      <div className="stat-value">{value}</div>
      <div className="stat-label">{label}</div>
      {detail !== undefined && <div className="stat-detail">{detail}</div>}
    </div>
  );
}
