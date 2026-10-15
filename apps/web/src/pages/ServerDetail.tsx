import { useState } from "react";
import type { FormEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { fetchTask, getServer } from "../lib/servers";
import type { TaskStatus } from "../lib/servers";

export default function ServerDetail() {
  const { id } = useParams<{ id: string }>();
  const [taskId, setTaskId] = useState("");
  const [submittedTaskId, setSubmittedTaskId] = useState<string | null>(null);

  const serverQuery = useQuery({
    queryKey: ["servers", id],
    queryFn: () => getServer(id ?? ""),
    enabled: !!id,
  });

  const taskQuery = useQuery<TaskStatus>({
    queryKey: ["tasks", submittedTaskId],
    queryFn: () => fetchTask(submittedTaskId ?? ""),
    enabled: submittedTaskId !== null && submittedTaskId !== "",
    retry: false,
  });

  function onTaskSubmit(e: FormEvent) {
    e.preventDefault();
    setSubmittedTaskId(taskId.trim() === "" ? null : taskId.trim());
  }

  if (!id) return <p className="err">Missing server id.</p>;
  if (serverQuery.isLoading) return <p>Loading server…</p>;
  if (serverQuery.error || !serverQuery.data)
    return <p className="err">Failed to load server.</p>;

  const s = serverQuery.data;

  return (
    <section>
      <p className="muted">
        <Link to="/servers">← Servers</Link>
      </p>
      <h2>{s.name}</h2>
      <dl>
        <dt>ID</dt>
        <dd>{s.id}</dd>
        <dt>Game port</dt>
        <dd>{s.game_port}</dd>
        <dt>Image</dt>
        <dd>{s.image_reference}</dd>
        <dt>Desired</dt>
        <dd>
          <span className="badge">{s.desired_state}</span>
        </dd>
        <dt>Observed</dt>
        <dd>
          <span className="badge">{s.observed_state}</span>
        </dd>
        {s.container_id && (
          <>
            <dt>Container</dt>
            <dd>{s.container_id}</dd>
          </>
        )}
        {s.last_error && (
          <>
            <dt>Last error</dt>
            <dd className="err">{s.last_error}</dd>
          </>
        )}
      </dl>

      <h3>Task lookup</h3>
      <p className="muted">
        Start/stop/reconcile operations return a task id. Paste it here to poll
        status via <code>GET /api/v1/tasks/:id</code>.
      </p>
      <form onSubmit={onTaskSubmit}>
        <label>
          Task ID
          <input
            value={taskId}
            onChange={(e) => setTaskId(e.target.value)}
            placeholder="task id"
          />
        </label>
        <button type="submit">Check task</button>
      </form>
      {taskQuery.isLoading && <p>Loading task…</p>}
      {taskQuery.error && <p className="err">Task not found or failed to load.</p>}
      {taskQuery.data && (
        <dl>
          <dt>Task</dt>
          <dd>{taskQuery.data.id}</dd>
          <dt>Kind</dt>
          <dd>{taskQuery.data.kind}</dd>
          <dt>State</dt>
          <dd>
            <span className="badge">{taskQuery.data.state}</span>
          </dd>
          {taskQuery.data.error && (
            <>
              <dt>Error</dt>
              <dd className="err">{taskQuery.data.error}</dd>
            </>
          )}
        </dl>
      )}
    </section>
  );
}
