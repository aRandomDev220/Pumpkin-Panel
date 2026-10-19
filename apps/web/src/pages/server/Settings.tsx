import { useState } from "react";
import type { FormEvent } from "react";
import {
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useNavigate, useOutletContext } from "react-router-dom";
import type { ServerOutletContext } from "../../components/server/ServerLayout";
import UpdateSection from "../../components/server/UpdateSection";
import Button from "../../components/ui/Button";
import Card from "../../components/ui/Card";
import ConfirmDialog from "../../components/ui/ConfirmDialog";
import Field, { Input } from "../../components/ui/Field";
import { useToast } from "../../components/ui/toast-context";
import { humanizeError } from "../../lib/errors";
import { formatDateTime } from "../../lib/format";
import { deleteServer, fetchTask } from "../../lib/servers";
import type { TaskStatus } from "../../lib/servers";

export default function ServerSettings(): React.JSX.Element {
  const { server, status } = useOutletContext<ServerOutletContext>();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const toast = useToast();
  const [taskId, setTaskId] = useState("");
  const [submittedTaskId, setSubmittedTaskId] = useState<string | null>(
    null,
  );
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [keepData, setKeepData] = useState(true);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const taskQuery = useQuery<TaskStatus>({
    queryKey: ["tasks", submittedTaskId],
    queryFn: () => fetchTask(submittedTaskId ?? ""),
    enabled: submittedTaskId !== null && submittedTaskId !== "",
    retry: false,
  });

  const deleteMutation = useMutation({
    mutationFn: () => deleteServer(server.id, keepData),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
      void queryClient.invalidateQueries({
        queryKey: ["metrics", "summary"],
      });
      void queryClient.invalidateQueries({ queryKey: ["alerts"] });
      toast.success(`Server “${server.name}” deleted`);
      navigate("/servers", { replace: true });
    },
    onError: (error) => setDeleteError(humanizeError(error)),
  });

  function onTaskLookup(event: FormEvent): void {
    event.preventDefault();
    const trimmed = taskId.trim();
    setSubmittedTaskId(trimmed === "" ? null : trimmed);
  }

  return (
    <section>
      <div className="settings-grid">
        <UpdateSection server={server} />

        <Card title="Server details">
          <dl className="server-info-list">
            <dt>Server ID</dt>
            <dd>
              <code>{server.id}</code>
            </dd>
            <dt>Game port</dt>
            <dd>{server.game_port}</dd>
            <dt>Image</dt>
            <dd>
              <code>{server.image_reference}</code>
            </dd>
            <dt>Requested</dt>
            <dd>{server.desired_state}</dd>
            <dt>Actual</dt>
            <dd>{server.observed_state}</dd>
            <dt>Readiness</dt>
            <dd>
              {status?.readiness !== undefined &&
              status?.readiness !== null
                ? `${status.readiness.state} — ${status.readiness.reason}`
                : "Not reported yet"}
            </dd>
            <dt>Created</dt>
            <dd>{formatDateTime(server.created_at)}</dd>
          </dl>
        </Card>
      </div>

      <details className="developer-tools">
        <summary>Developer tools</summary>
        <div className="developer-body">
          <dl className="server-info-list">
            <dt>Container ID</dt>
            <dd>
              {server.container_id !== undefined &&
              server.container_id !== null ? (
                <code>{server.container_id}</code>
              ) : (
                <span className="muted">No container yet.</span>
              )}
            </dd>
            <dt>Last error</dt>
            <dd>
              {server.last_error !== undefined &&
              server.last_error !== null ? (
                <span className="err">{server.last_error}</span>
              ) : (
                <span className="muted">None.</span>
              )}
            </dd>
          </dl>

          <form className="developer-form" onSubmit={onTaskLookup}>
            <Field
              label="Task ID"
              hint="Paste a task id to inspect its state directly."
            >
              <Input
                value={taskId}
                onChange={(event) => setTaskId(event.target.value)}
                placeholder="task id"
              />
            </Field>
            <Button type="submit" size="sm" variant="secondary">
              Check task
            </Button>
          </form>

          {taskQuery.isLoading && (
            <p className="muted">Loading task…</p>
          )}
          {taskQuery.error && (
            <p className="err" role="alert">
              Task not found or failed to load.
            </p>
          )}
          {taskQuery.data !== undefined && (
            <dl className="server-info-list" role="status">
              <dt>Task</dt>
              <dd>
                <code>{taskQuery.data.id}</code>
              </dd>
              <dt>Kind</dt>
              <dd>{taskQuery.data.kind}</dd>
              <dt>State</dt>
              <dd>{taskQuery.data.state}</dd>
              <dt>Progress</dt>
              <dd>
                {taskQuery.data.progress_percent !== null &&
                taskQuery.data.progress_percent !== undefined
                  ? `${taskQuery.data.progress_percent}%`
                  : "—"}
              </dd>
              {(taskQuery.data.message !== null &&
                taskQuery.data.message !== undefined) ||
              (taskQuery.data.error !== null &&
                taskQuery.data.error !== undefined) ? (
                <>
                  <dt>Detail</dt>
                  <dd
                    className={
                      taskQuery.data.error !== null &&
                      taskQuery.data.error !== undefined
                        ? "err"
                        : "muted"
                    }
                  >
                    {taskQuery.data.error ?? taskQuery.data.message}
                  </dd>
                </>
              ) : null}
            </dl>
          )}
        </div>
      </details>

      <div className="danger-zone">
        <h3>Danger zone</h3>
        <p>
          Deleting this server removes its container and panel entry.
          Keep world data to leave the files on disk.
        </p>
        <Button
          variant="danger"
          onClick={() => {
            setDeleteOpen(true);
            setKeepData(true);
            setDeleteError(null);
          }}
        >
          Delete server
        </Button>
      </div>

      <ConfirmDialog
        open={deleteOpen}
        onClose={() => {
          if (deleteMutation.isPending) return;
          setDeleteOpen(false);
          setDeleteError(null);
        }}
        onConfirm={() => deleteMutation.mutate()}
        title="Delete server"
        description={`Remove “${server.name}” and its container from the panel.`}
        confirmLabel="Delete server"
        busy={deleteMutation.isPending}
        checkbox={{
          label: "Keep world data",
          hint: "Leave checked to keep the world files on disk.",
          checked: keepData,
          onChange: setKeepData,
        }}
      >
        {deleteError !== null && (
          <p className="err" role="alert">
            {deleteError}
          </p>
        )}
      </ConfirmDialog>
    </section>
  );
}
