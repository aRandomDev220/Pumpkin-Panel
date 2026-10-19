import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter } from "react-router-dom";
import AppShell from "./components/app-shell/AppShell";
import { AuthProvider, useAuth } from "./lib/auth";
import AppRoutes from "./routes";

const qc = new QueryClient();

class ErrorBoundary extends Component<
  { children: ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error): { error: Error } {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("panel render error", error, info);
  }

  render(): ReactNode {
    if (this.state.error) {
      return (
        <div className="auth-shell">
          <section className="panel">
            <h2>Something went wrong</h2>
            <p className="err" role="alert">
              {this.state.error.message || "The panel could not render."}
            </p>
            <button
              type="button"
              onClick={() => window.location.reload()}
            >
              Reload the panel
            </button>
          </section>
        </div>
      );
    }
    return this.props.children;
  }
}

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
          <ErrorBoundary>
            <AppContent />
          </ErrorBoundary>
        </AuthProvider>
      </BrowserRouter>
    </QueryClientProvider>
  );
}
