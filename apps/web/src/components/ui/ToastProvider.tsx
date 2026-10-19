import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { AlertTriangle, CheckCircle2, Info, X } from "lucide-react";
import { ToastContext } from "./toast-context";
import type { ToastApi, ToastInput, ToastTone } from "./toast-context";

interface ToastItem {
  id: number;
  tone: ToastTone;
  message: string;
}

const ICONS = {
  success: CheckCircle2,
  error: AlertTriangle,
  info: Info,
} as const;

export default function ToastProvider({
  children,
}: {
  children: ReactNode;
}): React.JSX.Element {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const nextId = useRef(1);
  const timers = useRef<Set<number>>(new Set());

  const dismiss = useCallback((id: number) => {
    setToasts((current) => current.filter((toast) => toast.id !== id));
  }, []);

  const push = useCallback(
    (input: string | ToastInput) => {
      const item = typeof input === "string" ? { message: input } : input;
      const id = nextId.current;
      nextId.current += 1;
      const toast: ToastItem = {
        id,
        tone: item.tone ?? "info",
        message: item.message,
      };
      setToasts((current) => [...current, toast]);

      const duration = item.duration ?? 5000;
      if (duration > 0) {
        const timer = window.setTimeout(() => {
          timers.current.delete(timer);
          dismiss(id);
        }, duration);
        timers.current.add(timer);
      }
    },
    [dismiss],
  );

  useEffect(() => {
    const activeTimers = timers.current;
    return () => {
      for (const timer of activeTimers) window.clearTimeout(timer);
      activeTimers.clear();
    };
  }, []);

  const api = useMemo<ToastApi>(
    () => ({
      push,
      dismiss,
      success: (message: string) => push({ message, tone: "success" }),
      error: (message: string) => push({ message, tone: "error" }),
      info: (message: string) => push({ message, tone: "info" }),
    }),
    [push, dismiss],
  );

  return (
    <ToastContext.Provider value={api}>
      {children}
      <div className="toast-region" aria-live="polite" aria-atomic="false">
        {toasts.map((toast) => {
          const IconComponent = ICONS[toast.tone];
          return (
            <div
              key={toast.id}
              className={`toast toast-${toast.tone}`}
              role={toast.tone === "error" ? "alert" : "status"}
            >
              <span className="toast-icon" aria-hidden="true">
                <IconComponent size={18} />
              </span>
              <span className="toast-message">{toast.message}</span>
              <button
                className="toast-close"
                type="button"
                aria-label="Dismiss notification"
                onClick={() => dismiss(toast.id)}
              >
                <X size={16} />
              </button>
            </div>
          );
        })}
      </div>
    </ToastContext.Provider>
  );
}
