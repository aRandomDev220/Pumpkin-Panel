import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import type { ReactNode } from "react";
import { useAuth } from "./lib/auth";
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
  const { isAuthenticated, isLoading, needsSetupLoading, needsSetup } = useAuth();
  const location = useLocation();
  if (needsSetupLoading) return <p>Loading…</p>;
  if (needsSetup === true) return <Navigate to="/setup" replace />;
  if (isLoading) return <p>Loading…</p>;
  if (!isAuthenticated) {
    return <Navigate to="/login" state={{ from: location }} replace />;
  }
  return <>{children}</>;
}

// Permission-gated pages. The check mirrors the backend matrix, so a user
// never reaches a page whose API calls would all 403.
function RequirePermission({
  permission,
  children,
}: {
  permission: string;
  children: ReactNode;
}) {
  const { can, isLoading } = useAuth();
  if (isLoading) return <p>Loading…</p>;
  if (!can(permission)) {
    const ownerOnly =
      permission === "users.manage" || permission === "settings.manage";
    return (
      <section>
        <h2>Not available</h2>
        <p className="err" role="alert">
          This page requires {ownerOnly ? "an owner" : "an admin or owner"}{" "}
          account.
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
            <RequirePermission permission="users.manage">
              <Users />
            </RequirePermission>
          </RequireAuth>
        }
      />
      <Route
        path="/audit"
        element={
          <RequireAuth>
            <RequirePermission permission="audit.read">
              <Audit />
            </RequirePermission>
          </RequireAuth>
        }
      />
      <Route
        path="/settings"
        element={
          <RequireAuth>
            <RequirePermission permission="settings.manage">
              <Settings />
            </RequirePermission>
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
