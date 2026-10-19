import type { ReactNode } from "react";

export interface PageHeaderProps {
  title: ReactNode;
  eyebrow?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
}

export default function PageHeader({
  title,
  eyebrow,
  description,
  actions,
}: PageHeaderProps): React.JSX.Element {
  return (
    <header className="page-header">
      <div>
        {eyebrow !== undefined && <div className="eyebrow">{eyebrow}</div>}
        <h1>{title}</h1>
        {description !== undefined && (
          <p className="page-description">{description}</p>
        )}
      </div>
      {actions !== undefined && <div className="page-actions">{actions}</div>}
    </header>
  );
}
