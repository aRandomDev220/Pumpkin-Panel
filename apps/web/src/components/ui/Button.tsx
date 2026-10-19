import type {
  AnchorHTMLAttributes,
  ButtonHTMLAttributes,
  ComponentProps,
  ReactNode,
} from "react";
import { Link } from "react-router-dom";

export type ButtonVariant = "primary" | "secondary" | "danger" | "ghost";
export type ButtonSize = "sm" | "md" | "lg";

export interface ButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "className"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: ReactNode;
  loading?: boolean;
  className?: string;
  to?: string;
  href?: string;
}

export default function Button({
  variant = "primary",
  size = "md",
  icon,
  loading = false,
  className,
  children,
  to,
  href,
  type = "button",
  disabled,
  ...rest
}: ButtonProps): React.JSX.Element {
  const classes = [
    "btn",
    `btn-${variant}`,
    size === "md" ? "" : `btn-${size}`,
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  const content = (
    <>
      {loading ? <span className="btn-spinner" aria-hidden="true" /> : icon}
      {children}
    </>
  );

  if (to !== undefined) {
    return (
      <Link
        to={to}
        className={classes}
        {...(rest as unknown as Omit<ComponentProps<typeof Link>, "to" | "className">)}
      >
        {content}
      </Link>
    );
  }

  if (href !== undefined) {
    return (
      <a
        href={href}
        className={classes}
        {...(rest as unknown as AnchorHTMLAttributes<HTMLAnchorElement>)}
      >
        {content}
      </a>
    );
  }

  return (
    <button
      type={type}
      className={classes}
      disabled={disabled === true || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {content}
    </button>
  );
}
