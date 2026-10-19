import type {
  HTMLAttributes,
  ReactNode,
  TableHTMLAttributes,
} from "react";

export interface TableScrollProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode;
}

export function TableScroll({
  className,
  children,
  ...rest
}: TableScrollProps): React.JSX.Element {
  return (
    <div
      className={["table-scroll", className ?? ""].filter(Boolean).join(" ")}
      {...rest}
    >
      {children}
    </div>
  );
}

export function Table({
  className,
  children,
  ...rest
}: TableHTMLAttributes<HTMLTableElement>): React.JSX.Element {
  return (
    <table
      className={["table", className ?? ""].filter(Boolean).join(" ")}
      {...rest}
    >
      {children}
    </table>
  );
}
