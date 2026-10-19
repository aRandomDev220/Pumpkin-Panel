import { useState } from "react";
import type { FormEvent } from "react";
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { useAuth } from "../lib/auth";
import { createServer, deleteServer, digestWarning, fetchStatus, listServers, validateCreateServer } from "../lib/servers";

const RECOMMENDED_IMAGE = "ghcr.io/pumpkin-mc/pumpkin:master";
const RECOMMENDED_DIGEST = "sha256:675adcf4f351ed8656385346bf514584b0bb34826f06b553cd16094196d79099";

function stateLabel(state: string): string {
  const labels: Record<string, string> = { online: "Online", starting: "Starting", stopping: "Stopping", stopped: "Stopped", unhealthy: "Unhealthy", unknown: "Unknown", running: "Running", exited: "Stopped", missing: "Missing" };
  return labels[state] ?? state.replaceAll("_", " ");
}

function stateClass(state: string): string {
  if (state === "online" || state === "running") return "badge badge-ok";
  if (state === "unhealthy" || state === "missing") return "badge badge-err";
  if (state === "starting" || state === "stopping") return "badge badge-warn";
  return "badge";
}

export default function Servers(): React.JSX.Element {
  const { can } = useAuth();
  const canCreate = can("server.create");
  const canDelete = can("server.delete");
  const qc = useQueryClient();
  const serversQuery = useQuery({ queryKey: ["servers"], queryFn: listServers });
  const [name, setName] = useState("");
  const [gamePort, setGamePort] = useState("25565");
  const [imageReference, setImageReference] = useState(RECOMMENDED_IMAGE);
  const [imageDigest, setImageDigest] = useState(RECOMMENDED_DIGEST);
  const [customImage, setCustomImage] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [retainData, setRetainData] = useState(true);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [search, setSearch] = useState("");

  const createMutation = useMutation({
    mutationFn: createServer,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["servers"] });
      setName("");
      setImageReference(RECOMMENDED_IMAGE);
      setImageDigest(RECOMMENDED_DIGEST);
      setCustomImage(false);
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
    createMutation.mutate({ name: name.trim(), game_port: port, image_reference: imageReference.trim(), image_digest: imageDigest.trim() || undefined });
  }

  const servers = (serversQuery.data?.servers ?? []).filter((server) => server.name.toLowerCase().includes(search.trim().toLowerCase()) || server.image_reference.toLowerCase().includes(search.trim().toLowerCase()));
  const allServers = serversQuery.data?.servers ?? [];
  const statusQueries = useQueries({ queries: allServers.map((server) => ({ queryKey: ["servers", server.id, "status"], queryFn: () => fetchStatus(server.id), retry: 1, staleTime: 10_000 })) });
  const warning = digestWarning(imageReference);

  return (
    <section>
      <div className="page-heading">
        <div><div className="eyebrow">Worlds · Pumpkin runtime</div><h2>Servers</h2><p>Run, inspect, and protect the worlds living on your home node.</p></div>
        {canCreate && <div className="heading-actions"><a className="button-link" href="#create-server">Create a server</a></div>}
      </div>

      <div className="server-overview-strip">
        <div><strong>{allServers.length}</strong><span>registered</span></div>
            <div><strong>{allServers.filter((_server, index) => statusQueries[index]?.data?.readiness?.state === "online").length}</strong><span>online</span></div>
        <div><strong>{allServers.filter((server) => server.desired_state !== server.observed_state).length}</strong><span>state drift</span></div>
        <label className="server-search"><span>Find a world</span><input value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search by name or image" /></label>
      </div>

      {serversQuery.isLoading && <div className="panel"><p className="muted">Loading worlds…</p></div>}
      {serversQuery.error && <div className="panel"><p className="err" role="alert">The world list could not be loaded. Refresh to try again.</p></div>}
      {!serversQuery.isLoading && !serversQuery.error && allServers.length === 0 && <div className="empty-state"><div className="eyebrow">No worlds yet</div><h3>Give your node a first world.</h3><p>Your first Pumpkin server will appear here with real readiness, player, and resource information.</p><a className="button-link" href="#create-server">Create the first server</a></div>}
      {!serversQuery.isLoading && !serversQuery.error && allServers.length > 0 && servers.length === 0 && <div className="empty-state"><h3>No worlds match that search.</h3><p>Try a different name or clear the search field.</p><button className="button-secondary" type="button" onClick={() => setSearch("")}>Clear search</button></div>}

      {servers.length > 0 && <>
        <div className="section-heading"><h3>Your worlds</h3><span className="muted">{servers.length} shown</span></div>
        <ul className="server-list">
          {servers.map((server) => {
            const index = allServers.findIndex((item) => item.id === server.id);
            const readiness = statusQueries[index]?.data?.readiness;
             const state = statusQueries[index]?.isError || statusQueries[index]?.data?.runtime_state_stale
               ? "unknown"
               : readiness?.state ?? server.observed_state;
            const drifted = server.desired_state !== server.observed_state;
            return <li className="server-list-item" key={server.id}>
              <div className={`server-list-main ${state === "online" ? "is-running" : ""} ${drifted ? "is-drifted" : ""}`} aria-label={`${server.name} server`}>
                <div className="server-title-row"><span className="server-avatar" aria-hidden="true">{server.name.slice(0, 1).toUpperCase()}</span><div><Link className="server-name" to={`/servers/${encodeURIComponent(server.id)}`}>{server.name}</Link><div className="server-meta">Java {server.game_port} · <code>{server.image_reference}</code></div></div></div>
                <div className="server-badges"><span className={stateClass(state)}>{stateLabel(state)}</span><span className={drifted ? "badge badge-warn" : "badge"}>{drifted ? "State drift" : `Wants ${stateLabel(server.desired_state)}`}</span>{readiness?.ping && <span className="badge">{readiness.ping.players_online}/{readiness.ping.players_max} players</span>}</div>
              </div>
              <div className="server-list-actions">
                <Link className="section-link" to={`/servers/${encodeURIComponent(server.id)}`}>Open world</Link>
                {deleteTarget === server.id ? <form onSubmit={(event) => { event.preventDefault(); deleteMutation.mutate({ id: server.id, retain: retainData }); }}>
                  <label><input type="checkbox" checked={retainData} onChange={(event) => setRetainData(event.target.checked)} /> retain data</label>
                  <button type="submit" disabled={deleteMutation.isPending}>Confirm delete</button>
                  <button className="button-secondary" type="button" onClick={() => setDeleteTarget(null)}>Cancel</button>
                  {deleteMutation.isError && <span className="err">Delete failed.</span>}
                </form> : canDelete ? <button className="button-danger" type="button" onClick={() => setDeleteTarget(server.id)}>Delete</button> : null}
              </div>
            </li>;
          })}
        </ul>
      </>}

      <div className="panel create-panel" id="create-server">
        <div className="form-panel-heading"><div><div className="eyebrow">New world</div><h3>Create a Pumpkin server</h3><p>Start with the verified build. Advanced image selection is available when you need it.</p></div><span className="form-panel-badge">Pumpkin / Java</span></div>
        <form onSubmit={onCreate}>
          <div className="form-grid">
            <label>Name<input value={name} onChange={(event) => setName(event.target.value)} placeholder="survival" disabled={!canCreate} required /></label>
            <label>Java port<input value={gamePort} onChange={(event) => setGamePort(event.target.value)} inputMode="numeric" placeholder="25565" disabled={!canCreate} required /></label>
            <label>Build
              <select value={customImage ? "custom" : "recommended"} disabled={!canCreate} onChange={(event) => setCustomImage(event.target.value === "custom")}>
                <option value="recommended">Verified Pumpkin build</option>
                <option value="custom">Custom image (advanced)</option>
              </select>
            </label>
          </div>
          {!customImage && <p className="subtle-note">Uses the tested Pumpkin build for Linux/amd64. The image digest is pinned for repeatable updates.</p>}
          {customImage && <label className="advanced-field">Image reference (advanced)<input value={imageReference} disabled={!canCreate} onChange={(event) => { setImageReference(event.target.value); setImageDigest(""); }} placeholder="ghcr.io/pumpkin-mc/pumpkin:tag" required /><small>Only allowlisted Pumpkin images are accepted. Pin a digest for reproducible deploys.</small></label>}
          {warning && <p className="warn" role="note">{warning}</p>}
          {formError && <p className="err" role="alert">{formError}</p>}
          <div className="form-actions"><button type="submit" disabled={createMutation.isPending || !canCreate}>{createMutation.isPending ? "Creating world…" : "Create world"}</button></div>
        </form>
        {!canCreate && <p className="muted">Your role cannot create servers.</p>}
      </div>
    </section>
  );
}
