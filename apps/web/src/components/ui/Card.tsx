import type { HTMLAttributes, ReactNode } from "react";

export interface CardProps
  extends Omit<HTMLAttributes<HTMLDivElement>, "title"> {
  accent?: boolean;
  title?: ReactNode;
  actions?: ReactNode;
  footer?: ReactNode;
}

export default function Card({
  accent = false,
  title,
  actions,
  footer,
  className,
  children,
  ...rest
}: CardProps): React.JSX.Element {
  const classes = ["card", accent ? "card-accent" : "", className ?? ""]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={classes} {...rest}>
      {(title !== undefined || actions !== undefined) && (
        <div className="card-header">
          {title !== undefined && <h2 className="card-title">{title}</h2>}
          {actions !== undefined && (
            <div className="card-header-actions">{actions}</div>
          )}
        </div>
      )}
      <div className="card-body">{children}</div>
      {footer !== undefined && <div className="card-footer">{footer}</div>}
    </div>
  );
}
