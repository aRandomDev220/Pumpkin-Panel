import { useState } from "react";
import type { FormEvent } from "react";
import { Link, Navigate, useNavigate } from "react-router-dom";
import AuthLayout from "../components/auth/AuthLayout";
import { Icon } from "../components/icons";
import Button from "../components/ui/Button";
import Field, { Input } from "../components/ui/Field";
import { usePageTitle } from "../hooks/usePageTitle";
import { useAuth } from "../lib/auth";
import { errorStatus, humanizeError } from "../lib/errors";

export default function Login(): React.JSX.Element {
  usePageTitle("Sign in");
  const { user, login, loginPending, loginError, needsSetup } = useAuth();
  const navigate = useNavigate();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);

  if (user) return <Navigate to="/servers" replace />;

  async function onSubmit(event: FormEvent): Promise<void> {
    event.preventDefault();
    setError(null);
    try {
      await login(username.trim(), password);
      navigate("/servers", { replace: true });
    } catch (err) {
      setError(
        errorStatus(err) === 401
          ? "Invalid username or password."
          : humanizeError(err),
      );
    }
  }

  const message = error ?? loginError;

  return (
    <AuthLayout
      eyebrow="Self-hosted control plane"
      title={
        <>
          Your worlds, clearly <span className="hl">under control.</span>
        </>
      }
      description="Operate Pumpkin servers with honest status, visible tasks, and safe recovery paths."
      notes={[
        "Live readiness comes from a real Minecraft status response.",
        "Backups, config changes, and updates stay visible and deliberate.",
      ]}
    >
      <div className="auth-heading">
        <span className="auth-kicker">Pumpkin control plane</span>
        <h2>Welcome back</h2>
        <p className="muted">Sign in to manage your servers and worlds.</p>
      </div>

      {window.location.protocol === "http:" && (
        <p className="auth-notice" role="status">
          This panel is running over HTTP. On a LAN that is fine with insecure
          cookies enabled. Before exposing it outside your network, put it
          behind HTTPS and set <code>PUMPKIN_INSECURE_COOKIES=0</code>.
        </p>
      )}

      {needsSetup === true && (
        <p className="auth-switch muted">
          No owner account exists yet.{" "}
          <Link to="/setup">Run first-time setup</Link>.
        </p>
      )}

      <form
        className="auth-form"
        onSubmit={(event) => void onSubmit(event)}
      >
        <Field label="Username" hint="The account you use to sign in.">
          <Input
            value={username}
            onChange={(event) => setUsername(event.target.value)}
            autoComplete="username"
            required
            autoFocus
          />
        </Field>

        <Field label="Password">
          <Input
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            autoComplete="current-password"
            required
          />
        </Field>

        {message !== null && message !== undefined && (
          <p className="auth-error" role="alert">
            <Icon name="alert" size={16} />
            <span>{message}</span>
          </p>
        )}

        <Button
          type="submit"
          className="auth-submit"
          loading={loginPending}
        >
          {loginPending ? "Signing in…" : "Sign in"}
        </Button>
      </form>
    </AuthLayout>
  );
}
