import type { ReactNode } from "react";

export interface EmptyStateProps {
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  icon?: ReactNode;
}

export default function EmptyState({
  title,
  description,
  action,
  icon,
}: EmptyStateProps): React.JSX.Element {
  return (
    <div className="empty-state">
      {icon !== undefined && <div className="empty-state-icon">{icon}</div>}
      <h2>{title}</h2>
      {description !== undefined && <p>{description}</p>}
      {action !== undefined && <div>{action}</div>}
    </div>
  );
}
