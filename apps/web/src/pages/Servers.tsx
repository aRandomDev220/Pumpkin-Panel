import { useState } from "react";
import type { FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  createServer,
  deleteServer,
  digestWarning,
  listServers,
  validateCreateServer,
} from "../lib/servers";

export default function Servers() {
  const qc = useQueryClient();
  const serversQuery = useQuery({
    queryKey: ["servers"],
    queryFn: listServers,
  });

  const [name, setName] = useState("");
  const [gamePort, setGamePort] = useState("25565");
  const [imageReference, setImageReference] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [retainData, setRetainData] = useState(true);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  const createMutation = useMutation({
    mutationFn: createServer,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["servers"] });
      setName("");
      setImageReference("");
      setFormError(null);
    },
    onError: (err) => {
      setFormError(err instanceof Error ? err.message : "Create failed.");
    },
  });

  const deleteMutation = useMutation({
    mutationFn: ({ id, retain }: { id: string; retain: boolean }) =>
      deleteServer(id, retain),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["servers"] });
      setDeleteTarget(null);
    },
  });

  function onCreate(e: FormEvent) {
    e.preventDefault();
    const port = Number(gamePort);
    const validation = validateCreateServer({
      name: name.trim(),
      game_port: port,
      image_reference: imageReference.trim(),
    });
    if (validation) {
      setFormError(validation);
      return;
    }
    setFormError(null);
    createMutation.mutate({
      name: name.trim(),
      game_port: port,
      image_reference: imageReference.trim(),
    });
  }

  const warning = digestWarning(imageReference);
  const servers = serversQuery.data?.servers ?? [];

  return (
    <section>
      <h2>Servers</h2>
      {serversQuery.isLoading && <p>Loading…</p>}
      {serversQuery.error && <p className="err">Failed to load servers.</p>}
      {!serversQuery.isLoading && !serversQuery.error && servers.length === 0 && (
        <p className="muted">No servers yet. Create the first one below.</p>
      )}
      <ul>
        {servers.map((s) => (
          <li key={s.id}>
            <Link to={`/servers/${encodeURIComponent(s.id)}`}>{s.name}</Link>{" "}
            <span className="badge" title="desired state">
              desired:{s.desired_state}
            </span>{" "}
            <span className="badge" title="observed state">
              observed:{s.observed_state}
            </span>{" "}
            <span className="muted">:{s.game_port}</span>{" "}
            {deleteTarget === s.id ? (
              <span>
                <label>
                  <input
                    type="checkbox"
                    checked={retainData}
                    onChange={(e) => setRetainData(e.target.checked)}
                  />{" "}
                  retain data
                </label>{" "}
                <button
                  type="button"
                  disabled={deleteMutation.isPending}
                  onClick={() =>
                    deleteMutation.mutate({ id: s.id, retain: retainData })
                  }
                >
                  Confirm delete
                </button>{" "}
                <button type="button" onClick={() => setDeleteTarget(null)}>
                  Cancel
                </button>
                {deleteMutation.isError && (
                  <span className="err"> Delete failed.</span>
                )}
              </span>
            ) : (
              <button type="button" onClick={() => setDeleteTarget(s.id)}>
                Delete…
              </button>
            )}
          </li>
        ))}
      </ul>

      <h3>Create server</h3>
      <form onSubmit={onCreate}>
        <label>
          Name
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="survival-1"
            required
          />
        </label>
        <label>
          Game port
          <input
            value={gamePort}
            onChange={(e) => setGamePort(e.target.value)}
            inputMode="numeric"
            placeholder="25565"
            required
          />
        </label>
        <label>
          Image reference
          <input
            value={imageReference}
            onChange={(e) => setImageReference(e.target.value)}
            placeholder="registry.example.com/pumpkin@sha256:…"
            required
          />
        </label>
        {warning && <p className="muted">Warning: {warning}</p>}
        {formError && <p className="err">{formError}</p>}
        <button type="submit" disabled={createMutation.isPending}>
          {createMutation.isPending ? "Creating…" : "Create"}
        </button>
      </form>
    </section>
  );
}
