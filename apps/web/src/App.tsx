import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter } from "react-router-dom";
import ErrorBoundary from "./components/ErrorBoundary";
import AppShell from "./components/app-shell/AppShell";
import ToastProvider from "./components/ui/ToastProvider";
import { AuthProvider, useAuth } from "./lib/auth";
import AppRoutes from "./routes";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      staleTime: 5_000,
      refetchOnWindowFocus: false,
    },
  },
});

function AppContent(): React.JSX.Element {
  const { user } = useAuth();
  if (!user) {
    return (
      <div className="auth-shell">
        <AppRoutes />
      </div>
    );
  }
  return (
    <AppShell>
      <AppRoutes />
    </AppShell>
  );
}

export default function App(): React.JSX.Element {
  return (
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <ToastProvider>
          <BrowserRouter>
            <AuthProvider>
              <AppContent />
            </AuthProvider>
          </BrowserRouter>
        </ToastProvider>
      </QueryClientProvider>
    </ErrorBoundary>
  );
}
