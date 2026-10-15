import { Navigate, Route, Routes } from "react-router-dom";
import type { ReactNode } from "react";
import { useAuth } from "./lib/auth";
import Login from "./pages/Login";
import Setup from "./pages/Setup";
import Overview from "./pages/Overview";
import Servers from "./pages/Servers";
import ServerDetail from "./pages/ServerDetail";
import Config from "./pages/Config";
import Files from "./pages/Files";

function RequireAuth({ children }: { children: ReactNode }) {
  const { token, isLoading, needsSetupLoading, needsSetup } = useAuth();
  if (needsSetupLoading) return <p>Loading…</p>;
  if (needsSetup === true) return <Navigate to="/setup" replace />;
  if (!token) return <Navigate to="/login" replace />;
  if (isLoading) return <p>Loading…</p>;
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
