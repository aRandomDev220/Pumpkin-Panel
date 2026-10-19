import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";
import Button from "./ui/Button";

interface ErrorBoundaryState {
  error: Error | null;
}

export default class ErrorBoundary extends Component<
  { children: ReactNode },
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("Pumpkin Manager UI error", error, info);
  }

  render(): ReactNode {
    const { error } = this.state;
    if (error === null) return this.props.children;

    return (
      <div className="page-content">
        <div className="error-state" role="alert">
          <h3>The panel hit an unexpected error</h3>
          <p>
            Nothing was changed. Reload the panel to continue; if the error
            persists, check the manager logs.
          </p>
          <div className="row">
            <Button onClick={() => window.location.reload()}>
              Reload panel
            </Button>
            <Button
              variant="secondary"
              onClick={() => this.setState({ error: null })}
            >
              Try again
            </Button>
          </div>
          <details>
            <summary>Technical details</summary>
            <pre className="mono">{error.message}</pre>
          </details>
        </div>
      </div>
    );
  }
}
