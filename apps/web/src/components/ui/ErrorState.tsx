import { RefreshCw } from "lucide-react";
import type { ReactNode } from "react";
import Button from "./Button";

export interface ErrorStateProps {
  title?: ReactNode;
  message: ReactNode;
  onRetry?: () => void;
  retryLabel?: string;
  technicalDetails?: string;
}

export default function ErrorState({
  title = "Something went wrong",
  message,
  onRetry,
  retryLabel = "Try again",
  technicalDetails,
}: ErrorStateProps): React.JSX.Element {
  return (
    <div className="error-state" role="alert">
      <h3>{title}</h3>
      <p>{message}</p>
      {onRetry !== undefined && (
        <Button
          variant="secondary"
          size="sm"
          onClick={onRetry}
          icon={<RefreshCw size={14} />}
        >
          {retryLabel}
        </Button>
      )}
      {technicalDetails !== undefined && technicalDetails !== "" && (
        <details>
          <summary>Technical details</summary>
          <pre className="mono">{technicalDetails}</pre>
        </details>
      )}
    </div>
  );
}
