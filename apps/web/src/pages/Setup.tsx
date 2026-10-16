import { useState } from "react";
import type { FormEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useAuth } from "../lib/auth";

export default function Setup() {
  const { setup, setupPending, setupError, needsSetup, needsSetupLoading } =
    useAuth();
  const navigate = useNavigate();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);

  // Gated on setup-status: only the first admin may register while needs_setup.
  // Once needs_setup === false the wizard redirects to login.
  if (!needsSetupLoading && needsSetup === false) {
    return (
      <section>
        <h2>Setup complete</h2>
        <p className="muted">
          An admin already exists. <Link to="/login">Login</Link> instead.
        </p>
      </section>
    );
  }

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    if (password.length < 8) {
      setError("Password must be at least 8 characters.");
      return;
    }
    if (password !== confirm) {
      setError("Passwords do not match.");
      return;
    }
    try {
      await setup(username.trim(), password);
      navigate("/servers", { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Setup failed.");
    }
  }

  return (
    <section className="auth-page">
      <div className="auth-card">
        <div className="auth-heading">
          <span className="auth-kicker">PUMPKIN CONTROL PLANE</span>
          <h2>Set up your panel</h2>
          <p className="muted">Create the owner account. This runs once.</p>
        </div>
        {window.location.protocol === "http:" && (
          <p className="auth-notice" role="status">
            This LAN install is using HTTP. Keep non-secure cookies enabled, or
            place the panel behind HTTPS before exposing it outside your network.
          </p>
        )}
        <form className="auth-form" onSubmit={onSubmit}>
          <label className="field">
            <span>Admin username</span>
            <input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="username"
            required
          />
          </label>
          <label className="field">
            <span>Password (min 8 chars) <small>8 characters minimum</small></span>
            <input
              type="password"
              aria-label="Password (min 8 chars)"
              value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="new-password"
            required
          />
          </label>
          <label className="field">
            <span>Confirm password</span>
            <input
            type="password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            autoComplete="new-password"
            required
          />
          </label>
          {(error ?? setupError) && (
            <p className="err auth-error" role="alert">{error ?? setupError}</p>
          )}
          <button
            className="auth-submit"
            type="submit"
            aria-label="Create admin"
            disabled={setupPending || needsSetupLoading}
          >
            {setupPending ? "Creating account…" : "Create owner account"}
          </button>
        </form>
        <p className="auth-switch muted">Already configured? <Link to="/login">Sign in</Link>.</p>
      </div>
    </section>
  );
}
