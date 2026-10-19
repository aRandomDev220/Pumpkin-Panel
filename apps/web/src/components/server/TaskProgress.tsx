import Badge from "../ui/Badge";
import Meter from "../ui/Meter";
import type { TaskStatus } from "../../lib/servers";

export interface TaskProgressProps {
  task: TaskStatus | undefined;
  label: string;
}

export default function TaskProgress({
  task,
  label,
}: TaskProgressProps): React.JSX.Element | null {
  if (task === undefined) return null;

  const failed =
    task.state === "failed" || task.state === "cancelled";
  const succeeded = task.state === "succeeded";
  const tone = failed ? "danger" : succeeded ? "success" : "warning";
  const progress = task.progress_percent ?? 0;

  return (
    <div className="task-progress" aria-live="polite">
      <div className="task-progress-head">
        <span className="field-label">{label}</span>
        <Badge tone={tone}>{task.state}</Badge>
      </div>
      <Meter
        label="Progress"
        value={progress}
        max={100}
        valueLabel={
          task.progress_percent !== null &&
          task.progress_percent !== undefined
            ? `${task.progress_percent}%`
            : task.state
        }
        tone={tone}
      />
      {(task.message !== null && task.message !== undefined) ||
      (task.error !== null && task.error !== undefined) ? (
        <p className={task.error ? "err" : "muted"}>
          {task.error ?? task.message}
        </p>
      ) : null}
    </div>
  );
}
