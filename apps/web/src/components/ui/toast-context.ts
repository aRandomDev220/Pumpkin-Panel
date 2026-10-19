import { createContext, useContext } from "react";

export type ToastTone = "success" | "error" | "info";

export interface ToastInput {
  message: string;
  tone?: ToastTone;
  duration?: number;
}

export interface ToastApi {
  push: (input: string | ToastInput) => void;
  success: (message: string) => void;
  error: (message: string) => void;
  info: (message: string) => void;
  dismiss: (id: number) => void;
}

export const ToastContext = createContext<ToastApi | null>(null);

export function useToast(): ToastApi {
  const context = useContext(ToastContext);
  if (context === null) {
    throw new Error("useToast must be used within a ToastProvider");
  }
  return context;
}
