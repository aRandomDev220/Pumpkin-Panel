import { useState } from "react";
import type { FormEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useAuth } from "../lib/auth";

export default function Login() {
  const { login, loginPending, loginError, needsSetup } = useAuth();
  const navigate = useNavigate();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    try {
      await login(username.trim(), password);
      navigate("/servers", { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Login failed.");
    }
  }

  return (
    <section className="auth-page">
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
            aria-label="Login"
            disabled={loginPending}
          >
            {loginPending ? "Signing in…" : "Sign in"}
          </button>
        </form>
      </div>
    </section>
  );
}
