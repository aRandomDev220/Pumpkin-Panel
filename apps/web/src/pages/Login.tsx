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
    <section>
      <h2>Login</h2>
      {needsSetup === true && (
        <p className="muted">
          No admin exists yet. <Link to="/setup">Run first-time setup</Link>{" "}
          instead.
        </p>
      )}
      <form onSubmit={onSubmit}>
        <label>
          Username
          <input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="username"
            required
          />
        </label>
        <label>
          Password
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="current-password"
            required
          />
        </label>
        {(error ?? loginError) && <p className="err">{error ?? loginError}</p>}
        <button type="submit" disabled={loginPending}>
          {loginPending ? "Logging in…" : "Login"}
        </button>
      </form>
    </section>
  );
}
