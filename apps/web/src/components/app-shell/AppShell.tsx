import { useState } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchHealth } from "../../lib/api";
import { isPrivilegedRole } from "../../lib/admin";
import { useAuth } from "../../lib/auth";

type NavItem = {
  label: string;
  to: string;
  icon: string;
  exact?: boolean;
};

const operationalNav: NavItem[] = [
  { label: "Overview", to: "/", icon: "grid", exact: true },
  { label: "Servers", to: "/servers", icon: "server" },
  { label: "Monitoring", to: "/monitoring", icon: "pulse" },
];

const adminNav: NavItem[] = [
  { label: "Users", to: "/users", icon: "users" },
  { label: "Audit log", to: "/audit", icon: "list" },
  { label: "Settings", to: "/settings", icon: "settings" },
];

function Icon({ name }: { name: string }): React.JSX.Element {
  const paths: Record<string, React.JSX.Element> = {
    grid: (
      <>
        <rect x="3" y="3" width="7" height="7" rx="1" />
        <rect x="14" y="3" width="7" height="7" rx="1" />
        <rect x="3" y="14" width="7" height="7" rx="1" />
        <rect x="14" y="14" width="7" height="7" rx="1" />
      </>
    ),
    server: (
      <>
        <rect x="3" y="4" width="18" height="6" rx="1.5" />
        <rect x="3" y="14" width="18" height="6" rx="1.5" />
        <path d="M7 7h.01M7 17h.01M11 7h6M11 17h6" />
      </>
    ),
    pulse: <path d="M3 12h4l2.2-6 4.3 12 2.2-6H21" />,
    users: (
      <>
        <circle cx="9" cy="8" r="3" />
        <path d="M3.5 20c.5-3.2 2.3-5 5.5-5s5 1.8 5.5 5M16 4.5a3 3 0 0 1 0 5.8M17 15.3c2.1.7 3.3 2.2 3.6 4.7" />
      </>
    ),
    list: (
      <>
        <path d="M8 5h13M8 12h13M8 19h13" />
        <path d="M3 5h.01M3 12h.01M3 19h.01" />
      </>
    ),
    settings: (
      <>
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-1.8 1.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5v.1h-2.5v-.1a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.9.3l-.1.1-1.8-1.8.1-.1A1.7 1.7 0 0 0 8 15a1.7 1.7 0 0 0-1.5-1H6v-2.5h.5A1.7 1.7 0 0 0 8 10a1.7 1.7 0 0 0-.3-1.9l-.1-.1 1.8-1.8.1.1a1.7 1.7 0 0 0 1.9.3 1.7 1.7 0 0 0 1-1.5V5h2.5v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1 1.8 1.8-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.5 1h.1v2.5h-.1a1.7 1.7 0 0 0-1.5 1Z" />
      </>
    ),
  };

  return (
    <svg className="nav-icon" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      {paths[name]}
    </svg>
  );
}

function HealthBadge(): React.JSX.Element {
  const { data, isError, isLoading } = useQuery({
    queryKey: ["health"],
    queryFn: fetchHealth,
    refetchInterval: 15_000,
  });

  if (isLoading) {
    return <span className="health-indicator health-indicator-loading"><span className="status-dot" />Checking system</span>;
  }
  if (isError || !data) {
    return <span className="health-indicator health-indicator-error"><span className="status-dot" />API unreachable</span>;
  }
  return (
    <span className="health-indicator" title={JSON.stringify(data)}>
      <span className="status-dot" />System healthy
    </span>
  );
}

function SidebarNav({ items, onNavigate }: { items: NavItem[]; onNavigate: () => void }): React.JSX.Element {
  const location = useLocation();
  return (
    <nav className="sidebar-nav" aria-label="Main navigation">
      {items.map((item) => {
        const active = item.exact
          ? location.pathname === item.to
          : location.pathname.startsWith(item.to);
        return (
          <Link key={item.to} className={active ? "sidebar-link active" : "sidebar-link"} to={item.to} onClick={onNavigate}>
            <Icon name={item.icon} />
            <span>{item.label}</span>
          </Link>
        );
      })}
    </nav>
  );
}

export default function AppShell({ children }: { children: React.ReactNode }): React.JSX.Element {
  const { user, logout } = useAuth();
  const navigate = useNavigate();
  const [sidebarOpen, setSidebarOpen] = useState(false);

  async function onLogout(): Promise<void> {
    await logout();
    navigate("/login", { replace: true });
  }

  if (!user) return <>{children}</>;

  return (
    <div className="app-shell">
      {sidebarOpen && <button className="sidebar-backdrop" type="button" aria-label="Close navigation" onClick={() => setSidebarOpen(false)} />}
      <aside className={sidebarOpen ? "sidebar open" : "sidebar"}>
        <div className="brand-lockup">
          <span className="brand-mark" aria-hidden="true">P</span>
          <span>
            <strong>Pumpkin</strong>
            <small>Manager</small>
          </span>
        </div>

        <div className="sidebar-section">
          <span className="sidebar-heading">Workspace</span>
          <SidebarNav items={operationalNav} onNavigate={() => setSidebarOpen(false)} />
        </div>

        {isPrivilegedRole(user.role) && (
          <div className="sidebar-section">
            <span className="sidebar-heading">Administration</span>
            <SidebarNav items={adminNav} onNavigate={() => setSidebarOpen(false)} />
          </div>
        )}

        <div className="sidebar-footer">
          <span className="version-label">Control plane</span>
          <span className="version-value">v0.1.0</span>
        </div>
      </aside>

      <div className="app-main">
        <header className="topbar">
          <button className="menu-toggle" type="button" aria-label="Open navigation" onClick={() => setSidebarOpen(true)}>
            <span />
            <span />
            <span />
          </button>
          <div className="topbar-context">Pumpkin control plane</div>
          <div className="topbar-actions">
            <HealthBadge />
            <details className="user-menu">
              <summary aria-label="Open user menu">
                <span className="avatar">{user.username.slice(0, 1).toUpperCase()}</span>
                <span className="user-summary"><strong>{user.username}</strong><small>{user.role}</small></span>
              </summary>
              <div className="user-menu-popover">
                <div className="user-menu-meta">Signed in as <strong>{user.username}</strong></div>
                <button type="button" onClick={() => void onLogout()}>Sign out</button>
              </div>
            </details>
          </div>
        </header>
        <main className="page-content">{children}</main>
      </div>
    </div>
  );
}
