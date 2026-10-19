import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import Button from "../ui/Button";
import Card from "../ui/Card";
import ConfirmDialog from "../ui/ConfirmDialog";
import Field, { Input } from "../ui/Field";
import { useToast } from "../ui/toast-context";
import {
  isTaskInFlight,
  useTaskPolling,
} from "../../hooks/useTaskPolling";
import {
  previousImageOf,
  previewUpdate,
  requestRollback,
  requestUpdate,
} from "../../lib/admin";
import type { UpdatePreview } from "../../lib/admin";
import { errorStatus, humanizeError } from "../../lib/errors";
import type { ServerDetail } from "../../lib/servers";
import TaskProgress from "./TaskProgress";

const ROLLBACK_ONE_WAY =
  "Rollback is one-way for data: world files written by the new version may " +
  "not load on the old image. Keep a backup before rolling back.";

function isNoBackupError(error: unknown): boolean {
  return (
    errorStatus(error) === 409 ||
    /UPDATE_NO_BACKUP/i.test(
      error instanceof Error ? error.message : "",
    )
  );
}

export default function UpdateSection({
  server,
}: {
  server: ServerDetail;
}): React.JSX.Element {
  const queryClient = useQueryClient();
  const toast = useToast();
  const [target, setTarget] = useState("");
  const [digest, setDigest] = useState("");
  const [preview, setPreview] = useState<UpdatePreview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [noBackup, setNoBackup] = useState(false);
  const [updateTaskId, setUpdateTaskId] = useState<string | null>(null);
  const [rollbackTaskId, setRollbackTaskId] = useState<string | null>(null);
  const [rollbackError, setRollbackError] = useState<string | null>(null);
  const [rollbackOpen, setRollbackOpen] = useState(false);

  const previous = previousImageOf(server);
  const previousDigest = server.previous_image_digest;

  function invalidateServer(): void {
    void queryClient.invalidateQueries({
      queryKey: ["servers", server.id],
    });
    void queryClient.invalidateQueries({
      queryKey: ["servers", server.id, "tasks"],
    });
    void queryClient.invalidateQueries({ queryKey: ["servers"] });
    void queryClient.invalidateQueries({
      queryKey: ["metrics", "summary"],
    });
  }

  const updateTask = useTaskPolling(updateTaskId, (task) => {
    invalidateServer();
    if (task.state === "succeeded") {
      toast.success("Image update complete");
    } else {
      toast.error(
        task.error ?? `Image update failed (${task.state}).`,
      );
    }
  });

  const rollbackTask = useTaskPolling(rollbackTaskId, (task) => {
    invalidateServer();
    if (task.state === "succeeded") {
      toast.success("Rollback complete");
    } else {
      toast.error(task.error ?? `Rollback failed (${task.state}).`);
    }
  });

  const updateMutation = useMutation({
    mutationFn: () => requestUpdate(server.id, target.trim(), digest),
    onSuccess: (result) => {
      setUpdateError(null);
      setNoBackup(false);
      setUpdateTaskId(result.task_id);
      toast.info("Image update started");
    },
    onError: (error) => {
      if (isNoBackupError(error)) {
        setNoBackup(true);
        setUpdateError(null);
      } else {
        setUpdateError(humanizeError(error));
      }
    },
  });

  const rollbackMutation = useMutation({
    mutationFn: () => requestRollback(server.id),
    onSuccess: (result) => {
      setRollbackError(null);
      setRollbackTaskId(result.task_id);
      toast.info("Rollback started");
    },
    onError: (error) => setRollbackError(humanizeError(error)),
  });

  async function onPreview(): Promise<void> {
    setPreview(null);
    setPreviewError(null);
    setNoBackup(false);
    if (target.trim() === "") {
      setPreviewError("Enter a target image reference first.");
      return;
    }
    setPreviewBusy(true);
    try {
      setPreview(
        await previewUpdate(server.id, target.trim(), digest),
      );
    } catch (error) {
      setPreviewError(humanizeError(error));
    } finally {
      setPreviewBusy(false);
    }
  }

  const updateBusy =
    updateMutation.isPending || isTaskInFlight(updateTask.task);
  const rollbackBusy =
    rollbackMutation.isPending || isTaskInFlight(rollbackTask.task);

  return (
    <Card title="Update image">
      <dl className="server-info-list">
        <dt>Current</dt>
        <dd>
          <code>{server.image_reference || "unknown"}</code>
          {server.image_digest !== undefined &&
            server.image_digest !== null &&
            server.image_digest !== "" && (
              <>
                {" "}
                <code title={server.image_digest}>
                  {server.image_digest.slice(0, 19)}…
                </code>
              </>
            )}
        </dd>
        <dt>Previous</dt>
        <dd>
          {previous !== null ? (
            <code>{previous}</code>
          ) : (
            <span className="muted">
              No previous image recorded — rollback unavailable.
            </span>
          )}
          {previousDigest !== undefined &&
            previousDigest !== null &&
            previousDigest !== "" && (
              <>
                {" "}
                <code title={previousDigest}>
                  {previousDigest.slice(0, 19)}…
                </code>
              </>
            )}
        </dd>
      </dl>

      <div className="form-grid">
        <Field
          label="Target image"
          hint="The image reference to deploy."
        >
          <Input
            value={target}
            onChange={(event) => setTarget(event.target.value)}
            placeholder="ghcr.io/pumpkin-mc/pumpkin:1.22"
          />
        </Field>
        <Field
          label="Digest (optional)"
          hint="Pin a digest for reproducible updates."
        >
          <Input
            value={digest}
            onChange={(event) => setDigest(event.target.value)}
            placeholder="sha256:…"
          />
        </Field>
      </div>

      <div className="row">
        <Button
          variant="secondary"
          onClick={() => void onPreview()}
          loading={previewBusy}
        >
          Preview
        </Button>
        <Button
          onClick={() => updateMutation.mutate()}
          disabled={target.trim() === "" || updateBusy}
          loading={updateMutation.isPending}
        >
          Update to this image
        </Button>
      </div>

      {previewError !== null && (
        <p className="err" role="alert">
          {previewError}
        </p>
      )}

      {preview !== null && (
        <div className="update-preview">
          <p className="muted">
            Preview of <code>{target.trim()}</code>
            {preview.resolved_digest !== null && (
              <>
                {" "}
                (resolves to <code>{preview.resolved_digest}</code>)
              </>
            )}
            {preview.current_reference !== "" &&
              preview.current_reference !== target.trim() && (
                <>
                  {" "}
                  — current is <code>{preview.current_reference}</code>
                </>
              )}
            .
          </p>
          {preview.warnings.length === 0 ? (
            <p className="muted">No warnings.</p>
          ) : (
            <ul>
              {preview.warnings.map((warning) => (
                <li key={warning} className="warn">
                  {warning}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {updateError !== null && (
        <p className="err" role="alert">
          {updateError}
        </p>
      )}

      {noBackup && (
        <div className="notice-warning" role="alert">
          <p>
            No verified backup found — the update was refused.
          </p>
          <Button
            to={`/servers/${encodeURIComponent(server.id)}/backups`}
            size="sm"
            variant="secondary"
          >
            Create a backup first
          </Button>
        </div>
      )}

      <TaskProgress task={updateTask.task} label="Image update" />

      <p className="warn" role="note">
        {ROLLBACK_ONE_WAY}
      </p>

      <div className="row">
        <Button
          variant="danger"
          disabled={previous === null || rollbackBusy}
          title={
            previous === null
              ? "No previous image recorded — rollback unavailable"
              : `Roll back to ${previous}`
          }
          onClick={() => setRollbackOpen(true)}
        >
          {previous !== null
            ? `Roll back to ${previous}`
            : "Rollback unavailable"}
        </Button>
      </div>

      {rollbackError !== null && (
        <p className="err" role="alert">
          {rollbackError}
        </p>
      )}

      <TaskProgress task={rollbackTask.task} label="Rollback" />

      <ConfirmDialog
        open={rollbackOpen}
        onClose={() => setRollbackOpen(false)}
        onConfirm={() => {
          rollbackMutation.mutate();
          setRollbackOpen(false);
        }}
        title="Roll back to the previous image?"
        description={ROLLBACK_ONE_WAY}
        confirmLabel="Roll back"
        tone="danger"
        busy={rollbackMutation.isPending}
      />
    </Card>
  );
}
