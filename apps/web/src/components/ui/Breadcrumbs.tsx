import { Fragment } from "react";
import { Link } from "react-router-dom";

export interface Crumb {
  label: string;
  to?: string;
}

export default function Breadcrumbs({
  items,
}: {
  items: Crumb[];
}): React.JSX.Element {
  return (
    <nav className="breadcrumbs" aria-label="Breadcrumb">
      {items.map((item, index) => (
        <Fragment key={`${item.label}-${index}`}>
          {index > 0 && (
            <span className="breadcrumb-sep" aria-hidden="true">
              /
            </span>
          )}
          {item.to !== undefined ? (
            <Link to={item.to}>{item.label}</Link>
          ) : (
            <span aria-current="page">{item.label}</span>
          )}
        </Fragment>
      ))}
    </nav>
  );
}
