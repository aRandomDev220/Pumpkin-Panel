import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { listAudit } from "../lib/admin";

const PAGE_SIZE = 50;

function outcomeClass(outcome: string): string {
  const o = outcome.toLowerCase();
  if (o === "ok") return "badge badge-ok";
  if (o === "denied" || o === "failed" || o === "error") return "badge badge-err";
  return "badge";
}

export default function Audit() {
  const [action, setAction] = useState("");
  const [appliedAction, setAppliedAction] = useState("");
  const [actorSearch, setActorSearch] = useState("");
  const [offset, setOffset] = useState(0);

  const auditQuery = useQuery({
    queryKey: ["admin", "audit", appliedAction, offset],
    queryFn: () =>
      listAudit({
        limit: PAGE_SIZE,
        offset,
        action: appliedAction === "" ? undefined : appliedAction,
      }),
    retry: 1,
  });

  const events = auditQuery.data?.events ?? [];
  const needle = actorSearch.trim().toLowerCase();
  const visible =
    needle === ""
      ? events
      : events.filter((e) =>
          (e.actor_user_id ?? "").toLowerCase().includes(needle),
        );

  function applyFilter() {
    setOffset(0);
    setAppliedAction(action.trim());
  }

  return (
    <section>
      <div className="page-heading">
        <div><div className="eyebrow">Administration</div><h2>Audit log</h2><p>Review sensitive actions in newest-first order. Metadata is redacted server-side.</p></div>
        <span className="badge">{events.length} visible</span>
      </div>
      <p className="subtle-note">
        Newest first. Metadata is redacted server-side — secrets never appear
        here.
      </p>

      <div className="row">
        <label>
          Action filter{" "}
          <input
            value={action}
            onChange={(e) => setAction(e.target.value)}
            placeholder="e.g. users.update"
            aria-label="Filter by action"
          />
        </label>
        <label>
          Actor search{" "}
          <input
            value={actorSearch}
            onChange={(e) => setActorSearch(e.target.value)}
            placeholder="actor user id"
            aria-label="Search by actor"
          />
        </label>
        <button
          type="button"
          onClick={applyFilter}
          disabled={auditQuery.isFetching}
        >
          Apply
        </button>
        <button
          type="button"
          onClick={() => {
            setAction("");
            setActorSearch("");
            setAppliedAction("");
            setOffset(0);
          }}
        >
          Clear
        </button>
      </div>

      {auditQuery.isLoading && <p>Loading audit events…</p>}
      {auditQuery.error && (
        <p className="err" role="alert">
          Failed to load audit events.
        </p>
      )}
      {auditQuery.data && events.length === 0 && (
        <p className="muted">No audit events match this filter.</p>
      )}
      {visible.length > 0 && (
        <div className="table-scroll">
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Time</th>
              <th scope="col">Actor</th>
              <th scope="col">Action</th>
              <th scope="col">Target</th>
              <th scope="col">Outcome</th>
              <th scope="col">Metadata</th>
            </tr>
          </thead>
          <tbody>
            {visible.map((e) => (
              <tr key={e.id}>
                <td>{e.created_at || <span className="muted">—</span>}</td>
                <td>
                  {e.actor_user_id ? (
                    <code>{e.actor_user_id}</code>
                  ) : (
                    <span className="muted">—</span>
                  )}
                </td>
                <td>
                  <code>{e.action}</code>
                </td>
                <td>
                  {e.target_type || e.target_id ? (
                    <code>
                      {e.target_type}
                      {e.target_id ? `:${e.target_id}` : ""}
                    </code>
                  ) : (
                    <span className="muted">—</span>
                  )}
                </td>
                <td>
                  <span className={outcomeClass(e.outcome)}>{e.outcome || "—"}</span>
                </td>
                <td>
                  {e.metadata ? (
                    <code title={e.metadata}>
                      {e.metadata.length > 80
                        ? `${e.metadata.slice(0, 80)}…`
                        : e.metadata}
                    </code>
                  ) : (
                    <span className="muted">—</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      )}

      <div className="row">
        <button
          type="button"
          disabled={offset === 0 || auditQuery.isFetching}
          onClick={() => setOffset((o) => Math.max(0, o - PAGE_SIZE))}
        >
          ‹ Prev
        </button>
        <button
          type="button"
          disabled={events.length < PAGE_SIZE || auditQuery.isFetching}
          onClick={() => setOffset((o) => o + PAGE_SIZE)}
        >
          Next ›
        </button>
        <span className="muted" role="status">
          showing {events.length === 0 ? 0 : offset + 1}–{offset + events.length}
          {appliedAction !== "" && (
            <>
              {" "}
              for action <code>{appliedAction}</code>
            </>
          )}
        </span>
      </div>
    </section>
  );
}
