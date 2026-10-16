import { useState } from "react";
import type { FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { createServer, deleteServer, digestWarning, listServers, validateCreateServer } from "../lib/servers";

export default function Servers(): React.JSX.Element {
  const qc = useQueryClient();
  const serversQuery = useQuery({ queryKey: ["servers"], queryFn: listServers });
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
    onError: (err) => setFormError(err instanceof Error ? err.message : "Create failed."),
  });
  const deleteMutation = useMutation({
    mutationFn: ({ id, retain }: { id: string; retain: boolean }) => deleteServer(id, retain),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["servers"] });
      setDeleteTarget(null);
    },
  });

  function onCreate(event: FormEvent): void {
    event.preventDefault();
    const port = Number(gamePort);
    const validation = validateCreateServer({ name: name.trim(), game_port: port, image_reference: imageReference.trim() });
    if (validation) {
      setFormError(validation);
      return;
    }
    setFormError(null);
    createMutation.mutate({ name: name.trim(), game_port: port, image_reference: imageReference.trim() });
  }

  const servers = serversQuery.data?.servers ?? [];
  const warning = digestWarning(imageReference);

  return (
    <section>
      <div className="page-heading">
        <div>
          <div className="eyebrow">Workspace</div>
          <h2>Servers</h2>
          <p>Register, operate, and inspect every Pumpkin world from one focused workspace.</p>
        </div>
        <div className="heading-actions"><a href="#create-server"><button type="button">Create server</button></a></div>
      </div>

      {serversQuery.isLoading && <div className="panel"><p className="muted">Loading servers…</p></div>}
      {serversQuery.error && <div className="panel"><p className="err" role="alert">Failed to load servers. Refresh to try again.</p></div>}
      {!serversQuery.isLoading && !serversQuery.error && servers.length === 0 && <div className="empty-state"><h3>No servers yet.</h3><p>Your first server will appear here with live readiness, player, and resource information.</p><a href="#create-server"><button type="button">Create the first server</button></a></div>}

      {servers.length > 0 && <>
        <div className="section-heading"><h3>Registered servers</h3><span className="muted">{servers.length} total</span></div>
        <ul className="server-list">
          {servers.map((server) => {
            const running = server.observed_state === "running";
            const drifted = server.desired_state !== server.observed_state;
            return <li className="server-list-item" key={server.id}>
              <div className="server-list-main" aria-label={`${server.name} server`}>
                <Link className="server-name" to={`/servers/${encodeURIComponent(server.id)}`}>{server.name}</Link>
                <div className="server-meta">Port {server.game_port} · image <code>{server.image_reference}</code></div>
                <div className="server-badges"><span className={running ? "badge badge-ok" : "badge"}>{running ? "Running" : server.observed_state}</span><span className={drifted ? "badge badge-err" : "badge"}>{drifted ? "State drift" : `Desired ${server.desired_state}`}</span></div>
              </div>
              <div className="server-list-actions">
                <Link className="section-link" to={`/servers/${encodeURIComponent(server.id)}`}>Open server</Link>
                {deleteTarget === server.id ? <form onSubmit={(event) => { event.preventDefault(); deleteMutation.mutate({ id: server.id, retain: retainData }); }}>
                  <label><input type="checkbox" checked={retainData} onChange={(event) => setRetainData(event.target.checked)} /> retain data</label>
                  <button type="submit" disabled={deleteMutation.isPending}>Confirm delete</button>
                  <button className="button-secondary" type="button" onClick={() => setDeleteTarget(null)}>Cancel</button>
                  {deleteMutation.isError && <span className="err">Delete failed.</span>}
                </form> : <button className="button-danger" type="button" onClick={() => setDeleteTarget(server.id)}>Delete…</button>}
              </div>
            </li>;
          })}
        </ul>
      </>}

      <div className="panel create-panel" id="create-server">
        <h3>Create a server</h3>
        <p>Use a pinned image reference when possible so updates remain deliberate and reproducible.</p>
        <form onSubmit={onCreate}>
          <div className="form-grid">
            <label>Name<input value={name} onChange={(event) => setName(event.target.value)} placeholder="survival-1" required /></label>
            <label>Game port<input value={gamePort} onChange={(event) => setGamePort(event.target.value)} inputMode="numeric" placeholder="25565" required /></label>
            <label>Image reference<input value={imageReference} onChange={(event) => setImageReference(event.target.value)} placeholder="registry.example.com/pumpkin@sha256:…" required /></label>
          </div>
          {warning && <p className="warn" role="note">{warning}</p>}
          {formError && <p className="err" role="alert">{formError}</p>}
          <div className="form-actions"><button type="submit" disabled={createMutation.isPending}>{createMutation.isPending ? "Creating…" : "Create"}</button></div>
        </form>
      </div>
    </section>
  );
}
