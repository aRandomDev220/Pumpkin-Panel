import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { BrowserRouter, Link } from "react-router-dom";
import { fetchHealth } from "./lib/api";
import { AuthProvider, useAuth } from "./lib/auth";
import { isPrivilegedRole } from "./lib/admin";
import AppRoutes from "./routes";

const qc = new QueryClient();

function HealthBadge() {
  const { data, isError, isLoading } = useQuery({
    queryKey: ["health"],
    queryFn: fetchHealth,
    refetchInterval: 15000,
  });
  if (isLoading) return <span className="badge">checking…</span>;
  if (isError || !data) return <span className="badge badge-err">API unreachable</span>;
  return (
    <span className="badge badge-ok" title={JSON.stringify(data)}>
      API {data.status} · db {data.database} · docker {data.docker}
    </span>
  );
}

function Nav() {
  const { user } = useAuth();
  const privileged = isPrivilegedRole(user?.role);
  return (
    <nav>
      <Link to="/">Overview</Link>
      <Link to="/servers">Servers</Link>
      <Link to="/monitoring">Monitoring</Link>
      {privileged && (
        <>
          <Link to="/users">Users</Link>
          <Link to="/audit">Audit</Link>
          <Link to="/settings">Settings</Link>
        </>
      )}
    </nav>
  );
}

export default function App() {
  return (
    <QueryClientProvider client={qc}>
      <BrowserRouter>
        <AuthProvider>
          <div className="shell">
            <header>
              <h1>Pumpkin Manager</h1>
              <Nav />
              <HealthBadge />
            </header>
            <main>
              <AppRoutes />
            </main>
          </div>
        </AuthProvider>
      </BrowserRouter>
    </QueryClientProvider>
  );
}
