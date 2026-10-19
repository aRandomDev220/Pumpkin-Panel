import type { HTMLAttributes } from "react";

export type BadgeTone = "neutral" | "success" | "warning" | "danger" | "info";

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: BadgeTone;
}

export default function Badge({
  tone = "neutral",
  className,
  children,
  ...rest
}: BadgeProps): React.JSX.Element {
  const classes = ["badge", `badge-${tone}`, className ?? ""]
    .filter(Boolean)
    .join(" ");

  return (
    <span className={classes} {...rest}>
      {children}
    </span>
  );
}
