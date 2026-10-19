import { Navigate, Route, Routes } from "react-router-dom";
import type { ReactNode } from "react";
import { useAuth } from "./lib/auth";
import { isPrivilegedRole } from "./lib/admin";
import Button from "./components/ui/Button";
import EmptyState from "./components/ui/EmptyState";
import PageHeader from "./components/ui/PageHeader";
import Login from "./pages/Login";
import Setup from "./pages/Setup";
import Overview from "./pages/Overview";
import Monitoring from "./pages/Monitoring";
import Servers from "./pages/Servers";
import ServerLayout from "./components/server/ServerLayout";
import Console from "./pages/server/Console";
import BackupsPage from "./pages/server/BackupsPage";
import ServerSettings from "./pages/server/Settings";
import Config from "./pages/Config";
import Files from "./pages/Files";
import Users from "./pages/Users";
import Audit from "./pages/Audit";
import Settings from "./pages/Settings";
import NotFound from "./pages/NotFound";

function RouteLoading(): React.JSX.Element {
  return (
    <div className="route-loading" role="status">
      <span className="brand-mark" aria-hidden="true">
        <img src="/favicon.svg" alt="" />
      </span>
      <p className="muted">Loading your workspace…</p>
    </div>
  );
}

function RequireAuth({ children }: { children: ReactNode }) {
  const { isAuthenticated, isLoading, needsSetupLoading, needsSetup } = useAuth();
  if (needsSetupLoading) return <RouteLoading />;
  if (needsSetup === true) return <Navigate to="/setup" replace />;
  if (isLoading) return <RouteLoading />;
  if (!isAuthenticated) return <Navigate to="/login" replace />;
  return <>{children}</>;
}

// Owner/admin-only pages. Viewers (and signed-out edge cases that reach here)
// get an explicit denied state instead of a silent 404.
function RequirePrivileged({ children }: { children: ReactNode }) {
  const { user, isLoading } = useAuth();
  if (isLoading) return <RouteLoading />;
  if (!isPrivilegedRole(user?.role)) {
    return (
      <section>
        <PageHeader
          title="Not available"
          description="This page requires an owner or admin account."
        />
        <EmptyState
          title="Ask an owner for access"
          description="Viewers keep the dashboard, servers, and monitoring."
          action={<Button to="/">Back to dashboard</Button>}
        />
      </section>
    );
  }
  return <>{children}</>;
}

export default function AppRoutes() {
  return (
    <Routes>
      <Route path="/login" element={<Login />} />
      <Route path="/setup" element={<Setup />} />
      <Route
        path="/servers"
        element={
          <RequireAuth>
            <Servers />
          </RequireAuth>
        }
      />
      <Route
        path="/servers/:id"
        element={
          <RequireAuth>
            <ServerLayout />
          </RequireAuth>
        }
      >
        <Route index element={<Console />} />
        <Route path="files" element={<Files />} />
        <Route path="config" element={<Config />} />
        <Route path="backups" element={<BackupsPage />} />
        <Route path="settings" element={<ServerSettings />} />
      </Route>
      <Route
        path="/monitoring"
        element={
          <RequireAuth>
            <Monitoring />
          </RequireAuth>
        }
      />
      <Route
        path="/users"
        element={
          <RequireAuth>
            <RequirePrivileged>
              <Users />
            </RequirePrivileged>
          </RequireAuth>
        }
      />
      <Route
        path="/audit"
        element={
          <RequireAuth>
            <RequirePrivileged>
              <Audit />
            </RequirePrivileged>
          </RequireAuth>
        }
      />
      <Route
        path="/settings"
        element={
          <RequireAuth>
            <RequirePrivileged>
              <Settings />
            </RequirePrivileged>
          </RequireAuth>
        }
      />
      <Route
        path="/"
        element={
          <RequireAuth>
            <Overview />
          </RequireAuth>
        }
      />
      <Route path="*" element={<NotFound />} />
    </Routes>
  );
}
