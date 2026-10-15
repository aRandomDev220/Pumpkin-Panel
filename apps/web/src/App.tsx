import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { BrowserRouter, Link } from "react-router-dom";
import { fetchHealth } from "./lib/api";
import { AuthProvider } from "./lib/auth";
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

export default function App() {
  return (
    <QueryClientProvider client={qc}>
      <BrowserRouter>
        <AuthProvider>
          <div className="shell">
            <header>
              <h1>Pumpkin Manager</h1>
              <nav>
                <Link to="/">Overview</Link>
                <Link to="/servers">Servers</Link>
              </nav>
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
