import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
} from "react";
import type { ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

export const TOKEN_KEY = "pm_token";

let memoryToken: string | null = null;

function readStoredToken(): string | null {
  try {
    return window.localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

export function getToken(): string | null {
  if (memoryToken) return memoryToken;
  memoryToken = readStoredToken();
  return memoryToken;
}

export function setToken(token: string): void {
  memoryToken = token;
  try {
    window.localStorage.setItem(TOKEN_KEY, token);
  } catch {
    /* storage unavailable (private mode) — memory token still works */
  }
}

export function clearToken(): void {
  memoryToken = null;
  try {
    window.localStorage.removeItem(TOKEN_KEY);
  } catch {
    /* ignore */
  }
}

export async function authFetch(
  input: RequestInfo | URL,
  init: RequestInit = {},
): Promise<Response> {
  const token = getToken();
  const headers = new Headers(init.headers);
  if (token && !headers.has("Authorization")) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  return fetch(input, { ...init, headers });
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

interface TokenResponse {
  token: string;
  user: AuthUser;
}

export async function fetchSetupStatus(): Promise<SetupStatusResponse> {
  const res = await fetch("/api/v1/auth/setup-status");
  return parseJson<SetupStatusResponse>(res);
}

export async function fetchMe(): Promise<AuthUser> {
  const res = await authFetch("/api/v1/auth/me");
  return parseJson<AuthUser>(res);
}

export async function postLogin(
  username: string,
  password: string,
): Promise<TokenResponse> {
  const res = await fetch("/api/v1/auth/login", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ username, password }),
  });
  return parseJson<TokenResponse>(res);
}

export async function postSetup(
  username: string,
  password: string,
): Promise<TokenResponse> {
  const res = await fetch("/api/v1/auth/setup", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ username, password }),
  });
  return parseJson<TokenResponse>(res);
}

export async function postLogout(): Promise<void> {
  const res = await authFetch("/api/v1/auth/logout", { method: "POST" });
  if (!res.ok && res.status !== 401) {
    throw new Error(`logout failed (${res.status})`);
  }
}

interface AuthContextValue {
  user: AuthUser | null;
  token: string | null;
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
  const [, setTokenVersion] = useState(0);
  const token = getToken();

  const setupStatusQuery = useQuery({
    queryKey: ["auth", "setup-status"],
    queryFn: fetchSetupStatus,
    staleTime: 30_000,
    retry: 1,
  });

  const meQuery = useQuery({
    queryKey: ["auth", "me"],
    queryFn: fetchMe,
    enabled: token !== null,
    retry: false,
  });

  const loginMutation = useMutation({
    mutationFn: ({ username, password }: { username: string; password: string }) =>
      postLogin(username, password),
    onSuccess: (data) => {
      setToken(data.token);
      qc.setQueryData(["auth", "me"], data.user);
      setTokenVersion((v) => v + 1);
    },
  });

  const setupMutation = useMutation({
    mutationFn: ({ username, password }: { username: string; password: string }) =>
      postSetup(username, password),
    onSuccess: (data) => {
      setToken(data.token);
      qc.setQueryData(["auth", "me"], data.user);
      qc.setQueryData<SetupStatusResponse>(["auth", "setup-status"], {
        needs_setup: false,
      });
      setTokenVersion((v) => v + 1);
    },
  });

  const logoutMutation = useMutation({
    mutationFn: postLogout,
    onSettled: () => {
      clearToken();
      qc.setQueryData(["auth", "me"], null);
      qc.removeQueries({ queryKey: ["auth", "me"] });
      setTokenVersion((v) => v + 1);
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
      clearToken();
      qc.removeQueries({ queryKey: ["auth", "me"] });
      setTokenVersion((v) => v + 1);
    }
  }, [logoutMutation, qc]);

  const value = useMemo<AuthContextValue>(
    () => ({
      user: (meQuery.data as AuthUser | undefined) ?? null,
      token,
      isLoading: meQuery.isLoading,
      isAuthenticated: token !== null && meQuery.data !== undefined,
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
      token,
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
