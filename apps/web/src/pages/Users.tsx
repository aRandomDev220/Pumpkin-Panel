import { useState } from "react";
import type { FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useAuth } from "../lib/auth";
import {
  AdminApiError,
  createUser,
  deleteUser,
  errMsg,
  listUsers,
  updateUser,
} from "../lib/admin";
import type { AdminUser } from "../lib/admin";

const ROLES = ["owner", "admin", "viewer"] as const;

function actionError(err: unknown): { text: string; lastOwner: boolean } {
  const lastOwner =
    err instanceof AdminApiError &&
    (err.status === 409 || /LAST_OWNER/i.test(err.message));
  return { text: errMsg(err), lastOwner };
}

function RoleEditor({
  user,
  disabled,
  busy,
  onDone,
  onError,
}: {
  user: AdminUser;
  disabled: boolean;
  busy: boolean;
  onDone: () => void;
  onError: (err: unknown) => void;
}) {
  const [role, setRole] = useState(user.role);
  const qc = useQueryClient();
  const mutation = useMutation({
    mutationFn: (next: string) => updateUser(user.id, { role: next }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["admin", "users"] });
      onDone();
    },
    onError,
  });
  if (user.role === role && !mutation.isPending) {
    return (
      <label>
        Role{" "}
        <select
          value={role}
          disabled={disabled || busy}
          aria-label={`Role for ${user.username}`}
          onChange={(e) => {
            const next = e.target.value;
            setRole(next);
            if (
              next !== user.role &&
              window.confirm(
                `Change ${user.username} to role “${next}”? Demoting the last owner is refused.`,
              )
            ) {
              mutation.mutate(next);
            } else {
              setRole(user.role);
            }
          }}
        >
          {ROLES.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </select>
      </label>
    );
  }
  return <span className="muted">saving…</span>;
}

export default function Users() {
  const qc = useQueryClient();
  const { user: me } = useAuth();
  const [newUsername, setNewUsername] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [newRole, setNewRole] = useState<string>("viewer");
  const [createError, setCreateError] = useState<string | null>(null);
  const [opError, setOpError] = useState<string | null>(null);
  const [lastOwnerBanner, setLastOwnerBanner] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  const usersQuery = useQuery({
    queryKey: ["admin", "users"],
    queryFn: listUsers,
    retry: 1,
  });

  const users = usersQuery.data?.users ?? [];
  const busy = usersQuery.isFetching;

  function handleOpError(err: unknown) {
    const { text, lastOwner } = actionError(err);
    if (lastOwner) {
      setLastOwnerBanner(
        "Refused: this would remove the last active owner. " +
          "Promote or activate another owner first.",
      );
      setOpError(null);
    } else {
      setOpError(text);
    }
    setConfirmDeleteId(null);
  }

  const createMutation = useMutation({
    mutationFn: () =>
      createUser({
        username: newUsername.trim(),
        password: newPassword,
        role: newRole,
      }),
    onSuccess: () => {
      setNewUsername("");
      setNewPassword("");
      setNewRole("viewer");
      setCreateError(null);
      qc.invalidateQueries({ queryKey: ["admin", "users"] });
    },
    onError: (err) => setCreateError(errMsg(err)),
  });

  const toggleMutation = useMutation({
    mutationFn: ({ id, isActive }: { id: string; isActive: boolean }) =>
      updateUser(id, { is_active: isActive }),
    onSuccess: () => {
      setOpError(null);
      qc.invalidateQueries({ queryKey: ["admin", "users"] });
    },
    onError: handleOpError,
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => deleteUser(id),
    onSuccess: () => {
      setOpError(null);
      setLastOwnerBanner(null);
      setConfirmDeleteId(null);
      qc.invalidateQueries({ queryKey: ["admin", "users"] });
    },
    onError: handleOpError,
  });

  function onCreate(e: FormEvent) {
    e.preventDefault();
    setCreateError(null);
    if (!newUsername.trim()) {
      setCreateError("Username is required.");
      return;
    }
    if (newPassword.length < 8) {
      setCreateError("Password must be at least 8 characters.");
      return;
    }
    createMutation.mutate();
  }

  function onToggleActive(u: AdminUser) {
    const next = !u.is_active;
    if (
      !window.confirm(
        `${next ? "Activate" : "Deactivate"} user ${u.username}?` +
          (next ? "" : " Deactivating revokes their sessions immediately."),
      )
    ) {
      return;
    }
    toggleMutation.mutate({ id: u.id, isActive: next });
  }

  function onDelete(u: AdminUser) {
    if (confirmDeleteId !== u.id) {
      setConfirmDeleteId(u.id);
      return;
    }
    const selfNote =
      me && u.id === me.id ? " This signs you out immediately." : "";
    if (
      !window.confirm(
        `Delete user ${u.username}? This cannot be undone.${selfNote}`,
      )
    ) {
      setConfirmDeleteId(null);
      return;
    }
    deleteMutation.mutate(u.id);
  }

  return (
    <section>
      <div className="page-heading">
        <div><div className="eyebrow">Administration</div><h2>Users</h2><p>Manage who can sign in and what they are allowed to do.</p></div>
        <span className="badge">{users.length} accounts</span>
      </div>
      <p className="subtle-note">
        Manage who can sign in. Deactivating a user revokes their sessions
        immediately.
        {me && (
          <>
            {" "}
            You are <code>{me.username}</code> ({me.role}).
          </>
        )}
      </p>

      {lastOwnerBanner && (
        <p className="warn" role="alert">
          {lastOwnerBanner}
        </p>
      )}
      {opError && (
        <p className="err" role="alert">
          {opError}
        </p>
      )}

      {usersQuery.isLoading && <p>Loading users…</p>}
      {usersQuery.error && (
        <p className="err" role="alert">
          Failed to load users.
        </p>
      )}
      {usersQuery.data && users.length === 0 && (
        <p className="muted">No users found.</p>
      )}
      {users.length > 0 && (
        <div className="table-scroll">
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Username</th>
              <th scope="col">Role</th>
              <th scope="col">Active</th>
              <th scope="col">Created</th>
              <th scope="col">Last login</th>
              <th scope="col">Actions</th>
            </tr>
          </thead>
          <tbody>
            {users.map((u) => {
              const isSelf = me !== null && u.id === me.id;
              return (
                <tr key={u.id}>
                  <td>
                    <code>{u.username}</code>
                    {isSelf && <span className="badge"> you</span>}
                  </td>
                  <td>
                    <RoleEditor
                      user={u}
                      disabled={toggleMutation.isPending || deleteMutation.isPending}
                      busy={busy}
                      onDone={() => setOpError(null)}
                      onError={handleOpError}
                    />
                  </td>
                  <td>
                    <span className="badge">{u.is_active ? "yes" : "no"}</span>
                  </td>
                  <td>{u.created_at || <span className="muted">—</span>}</td>
                  <td>{u.last_login_at || <span className="muted">—</span>}</td>
                  <td>
                    <div className="row">
                      <button
                        type="button"
                        disabled={toggleMutation.isPending}
                        onClick={() => onToggleActive(u)}
                      >
                        {u.is_active ? "Deactivate" : "Activate"}
                      </button>
                      <button
                        type="button"
                        disabled={deleteMutation.isPending}
                        onClick={() => onDelete(u)}
                      >
                        {confirmDeleteId === u.id
                          ? "Confirm delete"
                          : "Delete"}
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        </div>
      )}

      <div className="section-heading"><h3>New user</h3><span className="muted">Invite an operator or viewer</span></div>
      {createError && (
        <p className="err" role="alert">
          {createError}
        </p>
      )}
      <form onSubmit={onCreate}>
        <div className="row">
          <label>
            Username{" "}
            <input
              value={newUsername}
              onChange={(e) => setNewUsername(e.target.value)}
              placeholder="new-operator"
              autoComplete="off"
            />
          </label>
          <label>
            Password{" "}
            <input
              type="password"
              value={newPassword}
              onChange={(e) => setNewPassword(e.target.value)}
              placeholder="at least 8 characters"
              autoComplete="new-password"
            />
          </label>
          <label>
            Role{" "}
            <select
              value={newRole}
              onChange={(e) => setNewRole(e.target.value)}
              aria-label="Role for new user"
            >
              {ROLES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
          </label>
          <button type="submit" disabled={createMutation.isPending}>
            {createMutation.isPending ? "Creating…" : "Create user"}
          </button>
        </div>
      </form>

      <div className="danger-zone">
        <h3>Danger zone</h3>
        <p className="muted">
          Deleting a user cannot be undone. Deleting yourself signs you out
          immediately. The last active owner can never be demoted, deactivated,
          or deleted.
        </p>
      </div>
    </section>
  );
}
