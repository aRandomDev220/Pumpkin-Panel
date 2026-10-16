import { Navigate, Route, Routes } from "react-router-dom";
import type { ReactNode } from "react";
import { useAuth } from "./lib/auth";
import { isPrivilegedRole } from "./lib/admin";
import Login from "./pages/Login";
import Setup from "./pages/Setup";
import Overview from "./pages/Overview";
import Monitoring from "./pages/Monitoring";
import Servers from "./pages/Servers";
import ServerDetail from "./pages/ServerDetail";
import Config from "./pages/Config";
import Files from "./pages/Files";
import Users from "./pages/Users";
import Audit from "./pages/Audit";
import Settings from "./pages/Settings";

function RequireAuth({ children }: { children: ReactNode }) {
  const { token, isLoading, needsSetupLoading, needsSetup } = useAuth();
  if (needsSetupLoading) return <p>Loading…</p>;
  if (needsSetup === true) return <Navigate to="/setup" replace />;
  if (!token) return <Navigate to="/login" replace />;
  if (isLoading) return <p>Loading…</p>;
  return <>{children}</>;
}

// Owner/admin-only pages. Viewers (and signed-out edge cases that reach here)
// get an explicit denied state instead of a silent 404.
function RequirePrivileged({ children }: { children: ReactNode }) {
  const { user, isLoading } = useAuth();
  if (isLoading) return <p>Loading…</p>;
  if (!isPrivilegedRole(user?.role)) {
    return (
      <section>
        <h2>Not available</h2>
        <p className="err" role="alert">
          This page requires an owner or admin account. Viewers keep
          Overview, Servers, and Monitoring.
        </p>
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
            <ServerDetail />
          </RequireAuth>
        }
      />
      <Route
        path="/servers/:id/config"
        element={
          <RequireAuth>
            <Config />
          </RequireAuth>
        }
      />
      <Route
        path="/servers/:id/files"
        element={
          <RequireAuth>
            <Files />
          </RequireAuth>
        }
      />
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
      <Route path="*" element={<p className="err">Not found.</p>} />
    </Routes>
  );
}
