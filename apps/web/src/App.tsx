import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { BrowserRouter, Link, Route, Routes } from "react-router-dom";
import { fetchHealth, fetchServers } from "./lib/api";

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

function Dashboard() {
  const { data, isLoading, error } = useQuery({
    queryKey: ["servers"],
    queryFn: fetchServers,
  });
  return (
    <section>
      <h2>Servers</h2>
      {isLoading && <p>Loading…</p>}
      {error && <p className="err">Failed to load servers.</p>}
      {data && data.servers.length === 0 && (
        <p className="muted">
          No servers yet. Phase 2 adds create/start/stop. The vertical slice
          ends with one manageable Pumpkin instance.
        </p>
      )}
    </section>
  );
}

export default function App() {
  return (
    <QueryClientProvider client={qc}>
      <BrowserRouter>
        <div className="shell">
          <header>
            <h1>Pumpkin Manager</h1>
            <nav>
              <Link to="/">Overview</Link>
            </nav>
            <HealthBadge />
          </header>
          <main>
            <Routes>
              <Route path="/" element={<Dashboard />} />
            </Routes>
          </main>
        </div>
      </BrowserRouter>
    </QueryClientProvider>
  );
}
