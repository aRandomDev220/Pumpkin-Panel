import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter } from "react-router-dom";
import AppShell from "./components/app-shell/AppShell";
import { AuthProvider, useAuth } from "./lib/auth";
import AppRoutes from "./routes";

const qc = new QueryClient();

function AppContent() {
  const { user } = useAuth();
  if (!user) return <div className="auth-shell"><AppRoutes /></div>;
  return <AppShell><AppRoutes /></AppShell>;
}

export default function App(): React.JSX.Element {
  return (
    <QueryClientProvider client={qc}>
      <BrowserRouter>
        <AuthProvider>
          <AppContent />
        </AuthProvider>
      </BrowserRouter>
    </QueryClientProvider>
  );
}
