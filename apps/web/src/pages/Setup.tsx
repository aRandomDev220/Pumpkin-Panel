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
    <section>
      <h2>First-time setup</h2>
      <p className="muted">Create the initial admin account. This runs once.</p>
      <form onSubmit={onSubmit}>
        <label>
          Admin username
          <input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="username"
            required
          />
        </label>
        <label>
          Password (min 8 chars)
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="new-password"
            required
          />
        </label>
        <label>
          Confirm password
          <input
            type="password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            autoComplete="new-password"
            required
          />
        </label>
        {(error ?? setupError) && <p className="err">{error ?? setupError}</p>}
        <button type="submit" disabled={setupPending || needsSetupLoading}>
          {setupPending ? "Creating…" : "Create admin"}
        </button>
      </form>
    </section>
  );
}
