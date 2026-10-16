import {
  createContext,
  useEffect,
  useCallback,
  useContext,
  useMemo,
  useState,
} from "react";
import type { ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

const CSRF_COOKIE = "pm_csrf";
const CSRF_HEADER = "X-CSRF-Token";

let unauthorizedHandler: (() => void) | null = null;

function cookieValue(name: string): string | null {
  const prefix = `${name}=`;
  const pair = document.cookie
    .split(";")
    .map((part) => part.trim())
    .find((part) => part.startsWith(prefix));
  return pair ? decodeURIComponent(pair.slice(prefix.length)) : null;
}

export function setUnauthorizedHandler(handler: (() => void) | null): void {
  unauthorizedHandler = handler;
}

export async function authFetch(
  input: RequestInfo | URL,
  init: RequestInit = {},
): Promise<Response> {
  const headers = new Headers(init.headers);
  const method = (init.method ?? "GET").toUpperCase();
  if (["POST", "PUT", "PATCH", "DELETE"].includes(method)) {
    const csrf = cookieValue(CSRF_COOKIE);
    if (csrf && !headers.has(CSRF_HEADER)) {
      headers.set(CSRF_HEADER, csrf);
    }
  }
  const response = await fetch(input, {
    ...init,
    credentials: "include",
    headers,
  });
  if (response.status === 401) {
    unauthorizedHandler?.();
  }
  return response;
}

async function parseJson<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as { error?: string; message?: string };
      detail = body.error ?? body.message ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new Error(`request failed (${res.status}): ${detail}`);
  }
  return res.json() as Promise<T>;
}

export interface AuthUser {
  id: string;
  username: string;
  role: string;
}

export interface SetupStatusResponse {
  needs_setup: boolean;
}

interface SessionResponse {
  user: AuthUser;
}

export async function fetchSetupStatus(): Promise<SetupStatusResponse> {
  const res = await fetch("/api/v1/auth/setup-status", { credentials: "include" });
  return parseJson<SetupStatusResponse>(res);
}

export async function fetchMe(): Promise<AuthUser> {
  const res = await authFetch("/api/v1/auth/me");
  return parseJson<AuthUser>(res);
}

export async function postLogin(
  username: string,
  password: string,
): Promise<SessionResponse> {
  const res = await fetch("/api/v1/auth/login", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "include",
    body: JSON.stringify({ username, password }),
  });
  return parseJson<SessionResponse>(res);
}

export async function postSetup(
  username: string,
  password: string,
): Promise<SessionResponse> {
  const res = await fetch("/api/v1/auth/setup", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "include",
    body: JSON.stringify({ username, password }),
  });
  return parseJson<SessionResponse>(res);
}

export async function postLogout(): Promise<void> {
  const res = await authFetch("/api/v1/auth/logout", { method: "POST" });
  if (!res.ok && res.status !== 401) {
    throw new Error(`logout failed (${res.status})`);
  }
}

interface AuthContextValue {
  user: AuthUser | null;
  isLoading: boolean;
  isAuthenticated: boolean;
  needsSetup: boolean | null;
  needsSetupLoading: boolean;
  loginPending: boolean;
  loginError: string | null;
  login: (username: string, password: string) => Promise<void>;
  setupPending: boolean;
  setupError: string | null;
  setup: (username: string, password: string) => Promise<void>;
  logout: () => Promise<void>;
}

const AuthContext = createContext<AuthContextValue | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const qc = useQueryClient();
  const [, setAuthVersion] = useState(0);

  const setupStatusQuery = useQuery({
    queryKey: ["auth", "setup-status"],
    queryFn: fetchSetupStatus,
    staleTime: 30_000,
    retry: 1,
  });

  const meQuery = useQuery({
    queryKey: ["auth", "me"],
    queryFn: fetchMe,
    retry: false,
  });

  useEffect(() => {
    const handleUnauthorized = () => {
      qc.setQueryData(["auth", "me"], null);
      setAuthVersion((version) => version + 1);
    };
    setUnauthorizedHandler(handleUnauthorized);
    return () => setUnauthorizedHandler(null);
  }, [qc]);

  const loginMutation = useMutation({
    mutationFn: ({ username, password }: { username: string; password: string }) =>
      postLogin(username, password),
    onSuccess: (data) => {
      qc.setQueryData(["auth", "me"], data.user);
      setAuthVersion((version) => version + 1);
    },
  });

  const setupMutation = useMutation({
    mutationFn: ({ username, password }: { username: string; password: string }) =>
      postSetup(username, password),
    onSuccess: (data) => {
      qc.setQueryData(["auth", "me"], data.user);
      qc.setQueryData<SetupStatusResponse>(["auth", "setup-status"], {
        needs_setup: false,
      });
      setAuthVersion((version) => version + 1);
    },
  });

  const logoutMutation = useMutation({
    mutationFn: postLogout,
    onSettled: () => {
      qc.setQueryData(["auth", "me"], null);
      qc.removeQueries({ queryKey: ["auth", "me"] });
      setAuthVersion((version) => version + 1);
    },
  });

  const login = useCallback(
    async (username: string, password: string): Promise<void> => {
      await loginMutation.mutateAsync({ username, password });
    },
    [loginMutation],
  );

  const setup = useCallback(
    async (username: string, password: string): Promise<void> => {
      await setupMutation.mutateAsync({ username, password });
    },
    [setupMutation],
  );

  const logout = useCallback(async (): Promise<void> => {
    try {
      await logoutMutation.mutateAsync();
    } catch {
      qc.removeQueries({ queryKey: ["auth", "me"] });
      setAuthVersion((version) => version + 1);
    }
  }, [logoutMutation, qc]);

  const value = useMemo<AuthContextValue>(
    () => ({
      user: (meQuery.data as AuthUser | undefined) ?? null,
      isLoading: meQuery.isLoading,
      isAuthenticated: meQuery.data !== undefined && meQuery.data !== null,
      needsSetup: setupStatusQuery.data?.needs_setup ?? null,
      needsSetupLoading: setupStatusQuery.isLoading,
      loginPending: loginMutation.isPending,
      loginError:
        loginMutation.error instanceof Error ? loginMutation.error.message : null,
      login,
      setupPending: setupMutation.isPending,
      setupError:
        setupMutation.error instanceof Error ? setupMutation.error.message : null,
      setup,
      logout,
    }),
    [
      meQuery.data,
      meQuery.isLoading,
      setupStatusQuery.data,
      setupStatusQuery.isLoading,
      loginMutation.isPending,
      loginMutation.error,
      setupMutation.isPending,
      setupMutation.error,
      login,
      setup,
      logout,
    ],
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth must be used inside <AuthProvider>");
  return ctx;
}
