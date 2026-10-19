import { NavLink } from "react-router-dom";

export interface TabItem {
  label: string;
  to: string;
  end?: boolean;
}

export interface TabsProps {
  items: TabItem[];
  ariaLabel: string;
}

export default function Tabs({
  items,
  ariaLabel,
}: TabsProps): React.JSX.Element {
  return (
    <nav className="tabs" aria-label={ariaLabel}>
      {items.map((item) => (
        <NavLink
          key={item.to}
          to={item.to}
          end={item.end}
          className={({ isActive }) =>
            isActive ? "tab tab-active" : "tab"
          }
        >
          {item.label}
        </NavLink>
      ))}
    </nav>
  );
}
