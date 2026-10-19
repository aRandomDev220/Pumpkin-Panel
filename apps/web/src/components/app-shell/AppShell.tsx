import { useEffect, useRef, useState } from "react";
import { NavLink, useLocation, useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchHealth } from "../../lib/api";
import type { ReadyResponse } from "../../lib/api";
import { isPrivilegedRole } from "../../lib/admin";
import { useAuth } from "../../lib/auth";
import type { AuthUser } from "../../lib/auth";
import { Icon } from "../icons";
import type { IconName } from "../icons";
import { useToast } from "../ui/toast-context";

type NavItem = {
  label: string;
  to: string;
  icon: IconName;
  end?: boolean;
};

const OPERATIONAL_NAV: NavItem[] = [
  { label: "Dashboard", to: "/", icon: "dashboard", end: true },
  { label: "Servers", to: "/servers", icon: "server" },
  { label: "Monitoring", to: "/monitoring", icon: "activity" },
];

const ADMIN_NAV: NavItem[] = [
  { label: "Users", to: "/users", icon: "users" },
  { label: "Audit log", to: "/audit", icon: "list" },
  { label: "Settings", to: "/settings", icon: "settings" },
];

function contextForPath(pathname: string): string {
  if (pathname === "/") return "Dashboard";
  if (pathname.startsWith("/servers/")) return "Server workspace";
  if (pathname.startsWith("/servers")) return "Servers";
  if (pathname.startsWith("/monitoring")) return "Monitoring";
  if (pathname.startsWith("/users")) return "Users";
  if (pathname.startsWith("/audit")) return "Audit log";
  if (pathname.startsWith("/settings")) return "Settings";
  return "Control plane";
}

function useIsMobile(): boolean {
  const [isMobile, setIsMobile] = useState(() =>
    window.matchMedia("(max-width: 900px)").matches,
  );

  useEffect(() => {
    const media = window.matchMedia("(max-width: 900px)");
    const update = () => setIsMobile(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  return isMobile;
}

interface HealthBadgeProps {
  data: ReadyResponse | undefined;
  isError: boolean;
  isLoading: boolean;
  compact?: boolean;
}

function HealthBadge({
  data,
  isError,
  isLoading,
  compact = false,
}: HealthBadgeProps): React.JSX.Element {
  const tone = isLoading
    ? "health-indicator-loading"
    : isError || data === undefined
      ? "health-indicator-error"
      : "";

  const text = isLoading
    ? compact
      ? "Checking"
      : "Checking system"
    : isError || data === undefined
      ? compact
        ? "Offline"
        : "API unreachable"
      : compact
        ? "Healthy"
        : "System healthy";

  const title =
    data !== undefined
      ? `Database ${data.database} · Docker ${data.docker} · v${data.version}`
      : "The manager health endpoint did not respond.";

  return (
    <span className={`health-indicator ${tone}`.trim()} title={title}>
      <span className="status-dot" aria-hidden="true" />
      {text}
    </span>
  );
}

function SidebarNav({
  items,
  onNavigate,
}: {
  items: NavItem[];
  onNavigate: () => void;
}): React.JSX.Element {
  return (
    <nav className="sidebar-nav">
      {items.map((item) => (
        <NavLink
          key={item.to}
          to={item.to}
          end={item.end}
          onClick={onNavigate}
          className={({ isActive }) =>
            isActive ? "sidebar-link active" : "sidebar-link"
          }
        >
          <Icon name={item.icon} size={18} className="nav-icon" />
          <span>{item.label}</span>
        </NavLink>
      ))}
    </nav>
  );
}

function UserMenu({
  user,
  onLogout,
}: {
  user: AuthUser;
  onLogout: () => Promise<void>;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const container = containerRef.current;
      if (container !== null && !container.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
        triggerRef.current?.focus();
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  return (
    <div className="user-menu" ref={containerRef}>
      <button
        ref={triggerRef}
        className="user-menu-trigger"
        type="button"
        aria-label={`Account menu for ${user.username}`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        <span className="avatar" aria-hidden="true">
          {user.username.slice(0, 1).toUpperCase()}
        </span>
        <span className="user-summary">
          <strong>{user.username}</strong>
          <small>{user.role}</small>
        </span>
        <Icon name="chevron-down" size={14} aria-hidden="true" />
      </button>
      {open && (
        <div className="user-menu-popover" role="menu">
          <div className="user-menu-meta">
            Signed in as <strong>{user.username}</strong>
          </div>
          <button
            type="button"
            role="menuitem"
            onClick={() => void onLogout()}
          >
            <Icon name="log-out" size={14} />
            Sign out
          </button>
        </div>
      )}
    </div>
  );
}

export default function AppShell({
  children,
}: {
  children: React.ReactNode;
}): React.JSX.Element {
  const { user, logout } = useAuth();
  const navigate = useNavigate();
  const location = useLocation();
  const toast = useToast();
  const isMobile = useIsMobile();
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const sidebarRef = useRef<HTMLElement>(null);
  const menuButtonRef = useRef<HTMLButtonElement>(null);

  const healthQuery = useQuery({
    queryKey: ["health"],
    queryFn: fetchHealth,
    refetchInterval: 15_000,
    retry: 0,
  });

  const drawerHidden = isMobile && !sidebarOpen;

  useEffect(() => {
    if (!sidebarOpen) return;
    const timer = window.setTimeout(() => {
      sidebarRef.current
        ?.querySelector<HTMLElement>("a[href]")
        ?.focus();
    }, 0);
    return () => window.clearTimeout(timer);
  }, [sidebarOpen]);

  useEffect(() => {
    if (!sidebarOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setSidebarOpen(false);
        menuButtonRef.current?.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [sidebarOpen]);

  async function onLogout(): Promise<void> {
    await logout();
    toast.info("Signed out");
    navigate("/login", { replace: true });
  }

  if (!user) return <>{children}</>;

  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">
        Skip to content
      </a>

      {isMobile && sidebarOpen && (
        <button
          className="sidebar-backdrop"
          type="button"
          aria-label="Close navigation"
          onClick={() => {
            setSidebarOpen(false);
            menuButtonRef.current?.focus();
          }}
        />
      )}

      <aside
        id="app-navigation"
        ref={sidebarRef}
        className={sidebarOpen ? "sidebar open" : "sidebar"}
        aria-label="Main navigation"
        {...(drawerHidden ? { inert: true, "aria-hidden": true } : {})}
      >
        <div className="brand-lockup">
          <span className="brand-mark" aria-hidden="true">
            <img src="/favicon.svg" alt="" />
          </span>
          <span>
            <strong>Pumpkin</strong>
            <small>Manager</small>
          </span>
        </div>

        <div className="sidebar-section">
          <span className="sidebar-heading">Workspace</span>
          <SidebarNav
            items={OPERATIONAL_NAV}
            onNavigate={() => setSidebarOpen(false)}
          />
        </div>

        {isPrivilegedRole(user.role) && (
          <div className="sidebar-section">
            <span className="sidebar-heading">Administration</span>
            <SidebarNav
              items={ADMIN_NAV}
              onNavigate={() => setSidebarOpen(false)}
            />
          </div>
        )}

        <div className="sidebar-footer">
          <HealthBadge
            compact
            data={healthQuery.data}
            isError={healthQuery.isError}
            isLoading={healthQuery.isLoading}
          />
          <span className="version-value">
            {healthQuery.data !== undefined
              ? `v${healthQuery.data.version}`
              : "v—"}
          </span>
        </div>
      </aside>

      <div className="app-main">
        <header className="topbar">
          <button
            ref={menuButtonRef}
            className="menu-toggle"
            type="button"
            aria-label="Open navigation"
            aria-controls="app-navigation"
            aria-expanded={sidebarOpen}
            onClick={() => setSidebarOpen(true)}
          >
            <span />
            <span />
            <span />
          </button>
          <div className="topbar-context">
            {contextForPath(location.pathname)}
          </div>
          <div className="topbar-actions">
            <HealthBadge
              data={healthQuery.data}
              isError={healthQuery.isError}
              isLoading={healthQuery.isLoading}
            />
            <UserMenu user={user} onLogout={onLogout} />
          </div>
        </header>
        <main id="main-content" className="page-content" tabIndex={-1}>
          {children}
        </main>
      </div>
    </div>
  );
}
