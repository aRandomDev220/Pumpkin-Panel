import type { ButtonHTMLAttributes, ReactNode } from "react";

export interface IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  label: string;
  icon: ReactNode;
  variant?: "ghost" | "secondary";
  size?: "sm" | "md";
}

export default function IconButton({
  label,
  icon,
  variant = "ghost",
  size = "md",
  className,
  type = "button",
  ...rest
}: IconButtonProps): React.JSX.Element {
  const classes = [
    "icon-btn",
    variant === "ghost" ? "icon-btn-ghost" : "icon-btn-secondary",
    size === "sm" ? "icon-btn-sm" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <button type={type} className={classes} aria-label={label} {...rest}>
      {icon}
    </button>
  );
}
