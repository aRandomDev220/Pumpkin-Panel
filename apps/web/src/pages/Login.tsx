import { useState } from "react";
import type { FormEvent } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { useAuth } from "../lib/auth";

export default function Login() {
  const { login, loginPending, loginError, needsSetup, sessionExpired } =
    useAuth();
  const navigate = useNavigate();
  const location = useLocation();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    try {
      await login(username.trim(), password);
      const from = (
        location.state as { from?: { pathname?: string } } | null
      )?.from;
      navigate(from?.pathname ?? "/servers", { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Login failed.");
    }
  }

  return (
    <section className="auth-page">
      <div className="auth-aside" aria-hidden="true">
        <div className="auth-brand"><span className="brand-mark">P</span><span>Pumpkin Manager</span></div>
        <div className="eyebrow">Self-hosted control plane</div>
        <h1>Your worlds, clearly under control.</h1>
        <p>Operate Pumpkin servers with honest status, visible tasks, and safe recovery paths.</p>
        <div className="auth-notes"><div className="auth-note"><span className="auth-note-mark" />Live readiness comes from a real Minecraft status response.</div><div className="auth-note"><span className="auth-note-mark" />Backups, config changes, and updates stay visible and deliberate.</div></div>
      </div>
      <div className="auth-card">
        <div className="auth-heading">
          <span className="auth-kicker">PUMPKIN CONTROL PLANE</span>
          <h2>Welcome back</h2>
          <p className="muted">Sign in to manage your servers and worlds.</p>
        </div>
        {window.location.protocol === "http:" && (
          <p className="auth-notice" role="status">
            You are using HTTP. LAN installs use non-secure cookies by default;
            use HTTPS with <code>PUMPKIN_INSECURE_COOKIES=0</code> for remote access.
          </p>
        )}
        {sessionExpired && (
          <p className="auth-notice" role="status">
            Your session expired. Sign in again.
          </p>
        )}
        {needsSetup === true && (
          <p className="auth-switch muted">
            No admin exists yet. <Link to="/setup">Run first-time setup</Link>.
          </p>
        )}
        <form className="auth-form" onSubmit={onSubmit}>
          <label className="field">
            <span>Username</span>
            <input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="username"
            required
          />
          </label>
          <label className="field">
            <span>Password</span>
            <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="current-password"
            required
          />
          </label>
          {(error ?? loginError) && (
            <p className="err auth-error" role="alert">{error ?? loginError}</p>
          )}
          <button
            className="auth-submit"
            type="submit"
            disabled={loginPending}
          >
            {loginPending ? "Signing in…" : "Sign in"}
          </button>
        </form>
      </div>
    </section>
  );
}
