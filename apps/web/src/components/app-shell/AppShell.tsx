import { useEffect, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchHealth } from "../../lib/api";
import { useAuth } from "../../lib/auth";

type Theme = "dark" | "light";

type NavItem = {
  label: string;
  to: string;
  icon: string;
  exact?: boolean;
  permission?: string;
};

const operationalNav: NavItem[] = [
  { label: "Overview", to: "/", icon: "grid", exact: true },
  { label: "Servers", to: "/servers", icon: "server" },
  { label: "Monitoring", to: "/monitoring", icon: "pulse" },
];

const adminNav: NavItem[] = [
  {
    label: "Users & roles",
    to: "/users",
    icon: "users",
    permission: "users.manage",
  },
  { label: "Audit log", to: "/audit", icon: "list", permission: "audit.read" },
  {
    label: "Settings",
    to: "/settings",
    icon: "settings",
    permission: "settings.manage",
  },
];

function Icon({ name }: { name: string }): React.JSX.Element {
  const paths: Record<string, React.JSX.Element> = {
    grid: (
      <>
        <rect x="3" y="3" width="7" height="7" rx="1.4" />
        <rect x="14" y="3" width="7" height="7" rx="1.4" />
        <rect x="3" y="14" width="7" height="7" rx="1.4" />
        <rect x="14" y="14" width="7" height="7" rx="1.4" />
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
    sun: (
      <>
        <circle cx="12" cy="12" r="4" />
        <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
      </>
    ),
    moon: <path d="M20.5 15.2A8.5 8.5 0 0 1 8.8 3.5 8.5 8.5 0 1 0 20.5 15.2Z" />,
  };

  return (
    <svg className="nav-icon" viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      {paths[name]}
    </svg>
  );
}

function PumpkinMark(): React.JSX.Element {
  return (
    <span className="brand-mark" aria-hidden="true">
      <span className="brand-stem" />
      <span className="brand-face"><i /><i /><b /></span>
    </span>
  );
}

function HealthBadge(): React.JSX.Element {
  const { data, isError, isLoading } = useQuery({
    queryKey: ["health"],
    queryFn: fetchHealth,
    refetchInterval: 15_000,
  });

  if (isLoading) return <span className="system-status status-loading"><span className="status-dot" />Checking runtime</span>;
  if (isError || !data) return <span className="system-status status-error"><span className="status-dot" />Runtime unavailable</span>;
  if (data.docker !== "reachable") return <span className="system-status status-error"><span className="status-dot" />Docker degraded</span>;
  return <span className="system-status"><span className="status-dot" />Docker connected</span>;
}

function SidebarNav({ items, onNavigate }: { items: NavItem[]; onNavigate: () => void }): React.JSX.Element {
  const location = useLocation();
  return (
    <nav className="sidebar-nav" aria-label="Main navigation">
      {items.map((item) => {
        const active = item.exact ? location.pathname === item.to : location.pathname.startsWith(item.to);
        return (
          <Link key={item.to} className={active ? "sidebar-link active" : "sidebar-link"} to={item.to} onClick={onNavigate} aria-current={active ? "page" : undefined}>
            <Icon name={item.icon} />
            <span>{item.label}</span>
          </Link>
        );
      })}
    </nav>
  );
}

function initialTheme(): Theme {
  const saved = window.localStorage.getItem("pumpkin-theme");
  return saved === "light" ? "light" : "dark";
}

export default function AppShell({ children }: { children: React.ReactNode }): React.JSX.Element {
  const { user, logout, can } = useAuth();
  const visibleAdminNav = adminNav.filter(
    (item) => !item.permission || can(item.permission),
  );
  const navigate = useNavigate();
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [theme, setTheme] = useState<Theme>(initialTheme);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    window.localStorage.setItem("pumpkin-theme", theme);
  }, [theme]);

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
          <PumpkinMark />
          <span className="brand-copy"><strong>Pumpkin</strong><small>World control</small></span>
        </div>

        <div className="sidebar-section">
          <span className="sidebar-heading">Operate</span>
          <SidebarNav items={operationalNav} onNavigate={() => setSidebarOpen(false)} />
        </div>

        {visibleAdminNav.length > 0 && (
          <div className="sidebar-section">
            <span className="sidebar-heading">Administer</span>
            <SidebarNav items={visibleAdminNav} onNavigate={() => setSidebarOpen(false)} />
          </div>
        )}

        <div className="sidebar-footer">
          <div><span className="version-label">Pumpkin Manager</span><span className="version-value">v0.1.0</span></div>
          <span className="sidebar-foot-mark">PM</span>
        </div>
      </aside>

      <div className="app-main">
        <header className="topbar">
          <button className="menu-toggle" type="button" aria-label="Open navigation" onClick={() => setSidebarOpen(true)}>
            <span /><span /><span />
          </button>
          <div className="topbar-context"><span className="context-kicker">Control room</span><span className="context-divider">/</span><span>Home node</span></div>
          <div className="topbar-actions">
            <HealthBadge />
            <button className="theme-toggle" type="button" aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} mode`} onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
              <Icon name={theme === "dark" ? "sun" : "moon"} />
              <span>{theme === "dark" ? "Light" : "Dark"}</span>
            </button>
            <details className="user-menu">
              <summary aria-label="Open user menu">
                <span className="avatar">{user.username.slice(0, 1).toUpperCase()}</span>
                <span className="user-summary"><strong>{user.username}</strong><small>{user.role}</small></span>
              </summary>
              <div className="user-menu-popover">
                <div className="user-menu-meta"><span>Signed in as</span><strong>{user.username}</strong></div>
                <button className="button-secondary" type="button" onClick={() => void onLogout()}>Sign out</button>
              </div>
            </details>
          </div>
        </header>
        <main className="page-content">{children}</main>
      </div>
    </div>
  );
}
