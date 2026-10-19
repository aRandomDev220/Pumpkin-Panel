import { useState } from "react";
import type { FormEvent } from "react";
import { Link, Navigate, useNavigate } from "react-router-dom";
import AuthLayout from "../components/auth/AuthLayout";
import { Icon } from "../components/icons";
import Button from "../components/ui/Button";
import Field, { Input } from "../components/ui/Field";
import { usePageTitle } from "../hooks/usePageTitle";
import { useAuth } from "../lib/auth";
import { humanizeError } from "../lib/errors";

interface FormErrors {
  username?: string;
  password?: string;
  confirm?: string;
}

export default function Setup(): React.JSX.Element {
  usePageTitle("Set up");
  const {
    user,
    setup,
    setupPending,
    setupError,
    needsSetup,
    needsSetupLoading,
  } = useAuth();
  const navigate = useNavigate();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<FormErrors>({});
  const [error, setError] = useState<string | null>(null);

  if (user) return <Navigate to="/servers" replace />;

  if (!needsSetupLoading && needsSetup === false) {
    return (
      <AuthLayout
        eyebrow="First launch"
        title={
          <>
            Set up a calmer way to run{" "}
            <span className="hl">your worlds.</span>
          </>
        }
        description="The owner account already exists on this manager."
      >
        <div className="auth-heading">
          <span className="auth-kicker">Setup complete</span>
          <h2>The panel is configured</h2>
          <p className="muted">
            An owner account already exists. Sign in to continue.
          </p>
        </div>
        <Button to="/login" className="auth-submit">
          Sign in
        </Button>
      </AuthLayout>
    );
  }

  function validate(): boolean {
    const next: FormErrors = {};
    if (username.trim() === "") {
      next.username = "Enter a username.";
    }
    if (password.length < 8) {
      next.password = "Use at least 8 characters.";
    }
    if (confirm !== password) {
      next.confirm = "Passwords do not match.";
    }
    setErrors(next);
    return Object.keys(next).length === 0;
  }

  async function onSubmit(event: FormEvent): Promise<void> {
    event.preventDefault();
    setError(null);
    if (!validate()) return;
    try {
      await setup(username.trim(), password);
      navigate("/servers", { replace: true });
    } catch (err) {
      setError(humanizeError(err));
    }
  }

  const message = error ?? setupError;

  return (
    <AuthLayout
      eyebrow="First launch"
      title={
        <>
          Set up a calmer way to run{" "}
          <span className="hl">your worlds.</span>
        </>
      }
      description="Create the owner account once, then keep every server, backup, and operational task in one place."
      notes={[
        "Your account is stored locally with the manager.",
        "You can add admins and viewers after setup.",
      ]}
    >
      <div className="auth-heading">
        <span className="auth-kicker">Pumpkin control plane</span>
        <h2>Set up your panel</h2>
        <p className="muted">Create the owner account. This runs once.</p>
      </div>

      {window.location.protocol === "http:" && (
        <p className="auth-notice" role="status">
          This panel is running over HTTP. Keep insecure cookies enabled on a
          LAN, or place the panel behind HTTPS before exposing it outside your
          network.
        </p>
      )}

      <form
        className="auth-form"
        onSubmit={(event) => void onSubmit(event)}
      >
        <Field label="Admin username" error={errors.username}>
          <Input
            value={username}
            onChange={(event) => setUsername(event.target.value)}
            autoComplete="username"
            required
            autoFocus
          />
        </Field>

        <Field
          label="Password (min 8 chars)"
          hint="Use at least 8 characters."
          error={errors.password}
        >
          <Input
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            autoComplete="new-password"
            required
          />
        </Field>

        <Field label="Confirm password" error={errors.confirm}>
          <Input
            type="password"
            value={confirm}
            onChange={(event) => setConfirm(event.target.value)}
            autoComplete="new-password"
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
          loading={setupPending}
          disabled={needsSetupLoading}
        >
          {setupPending ? "Creating account…" : "Create owner account"}
        </Button>
      </form>

      <p className="auth-switch muted">
        Already configured? <Link to="/login">Sign in</Link>.
      </p>
    </AuthLayout>
  );
}
