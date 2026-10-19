import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useOutletContext } from "react-router-dom";
import type { ServerOutletContext } from "../../components/server/ServerLayout";
import TaskProgress from "../../components/server/TaskProgress";
import Badge from "../../components/ui/Badge";
import Button from "../../components/ui/Button";
import Card from "../../components/ui/Card";
import ErrorState from "../../components/ui/ErrorState";
import LogViewer from "../../components/ui/LogViewer";
import { SkeletonText } from "../../components/ui/Skeleton";
import { formatDateTime } from "../../lib/format";
import {
  METRICS_RANGES,
  fetchMetricsHistory,
} from "../../lib/monitoring";
import type { MetricsRange } from "../../lib/monitoring";
import {
  fetchServerLogs,
  fetchServerTasks,
} from "../../lib/servers";
import { ServerHistoryPanels } from "../Monitoring";

const LOG_TAIL_OPTIONS = [50, 200, 500];

const OP_LABEL = {
  start: "Start",
  stop: "Stop",
  restart: "Restart",
} as const;

export default function Console(): React.JSX.Element {
  const { server, status, lifecycle } =
    useOutletContext<ServerOutletContext>();
  const [logTail, setLogTail] = useState(200);
  const [range, setRange] = useState<MetricsRange>("1h");

  const logsQuery = useQuery({
    queryKey: ["servers", server.id, "logs", logTail],
    queryFn: () => fetchServerLogs(server.id, logTail),
    refetchInterval: 5_000,
    retry: 1,
  });
  const tasksQuery = useQuery({
    queryKey: ["servers", server.id, "tasks"],
    queryFn: () => fetchServerTasks(server.id, 10, 0),
    retry: 1,
  });
  const historyQuery = useQuery({
    queryKey: ["servers", server.id, "metrics", "history", range],
    queryFn: () => fetchMetricsHistory(server.id, range),
    retry: 1,
    staleTime: 30_000,
  });

  const tasks = tasksQuery.data?.tasks ?? [];
  const taskLabel =
    lifecycle.activeOp !== null
      ? `${OP_LABEL[lifecycle.activeOp]} task`
      : "Server task";
  const readiness = status?.readiness;

  return (
    <section>
      {lifecycle.task !== undefined && (
        <TaskProgress task={lifecycle.task} label={taskLabel} />
      )}

      <div className="console-grid">
        <div className="console-main">
          <div className="section-heading">
            <h2>Console</h2>
            <span className="muted">Auto-refreshes every 5 seconds</span>
          </div>

          {logsQuery.error ? (
            <ErrorState
              message="Could not load the server logs."
              technicalDetails={
                logsQuery.error instanceof Error
                  ? logsQuery.error.message
                  : undefined
              }
              onRetry={() => void logsQuery.refetch()}
            />
          ) : (
            <LogViewer
              lines={logsQuery.data?.logs ?? []}
              loading={logsQuery.isLoading}
              toolbar={
                <label className="log-tail">
                  Tail
                  <select
                    value={logTail}
                    onChange={(event) =>
                      setLogTail(Number(event.target.value))
                    }
                    aria-label="Log tail lines"
                  >
                    {LOG_TAIL_OPTIONS.map((lineCount) => (
                      <option key={lineCount} value={lineCount}>
                        last {lineCount}
                      </option>
                    ))}
                  </select>
                </label>
              }
            />
          )}

          <Card
            title="Metrics history"
            actions={
              <div
                className="filter-group"
                role="group"
                aria-label="Metrics history range"
              >
                {METRICS_RANGES.map((value) => (
                  <Button
                    key={value}
                    size="sm"
                    variant={range === value ? "primary" : "secondary"}
                    aria-pressed={range === value}
                    onClick={() => setRange(value)}
                  >
                    {value}
                  </Button>
                ))}
              </div>
            }
          >
            {historyQuery.isLoading && <SkeletonText lines={3} />}
            {historyQuery.error && (
              <ErrorState
                message="Could not load metrics history."
                technicalDetails={
                  historyQuery.error instanceof Error
                    ? historyQuery.error.message
                    : undefined
                }
                onRetry={() => void historyQuery.refetch()}
              />
            )}
            {historyQuery.data && (
              <ServerHistoryPanels samples={historyQuery.data.samples} />
            )}
          </Card>
        </div>

        <aside className="console-rail">
          <Card title="Server info">
            <dl className="server-info-list">
              <dt>Server ID</dt>
              <dd>
                <code>{server.id}</code>
              </dd>
              <dt>Image</dt>
              <dd>
                <code>{server.image_reference}</code>
              </dd>
              <dt>Game port</dt>
              <dd>{server.game_port}</dd>
              <dt>Readiness</dt>
              <dd>
                {readiness !== undefined && readiness !== null
                  ? `${readiness.state} — ${readiness.reason}`
                  : "Not reported yet"}
              </dd>
              <dt>Requested</dt>
              <dd>{server.desired_state}</dd>
              <dt>Actual</dt>
              <dd>{server.observed_state}</dd>
              {server.container_id !== undefined &&
                server.container_id !== null && (
                  <>
                    <dt>Container</dt>
                    <dd>
                      <code>{server.container_id}</code>
                    </dd>
                  </>
                )}
              <dt>Created</dt>
              <dd>{formatDateTime(server.created_at)}</dd>
            </dl>
          </Card>

          <Card
            title="Recent tasks"
            actions={<span className="muted">{tasks.length}</span>}
          >
            {tasksQuery.isLoading && <SkeletonText lines={3} />}
            {tasksQuery.error && (
              <div className="task-error">
                <p className="err">Could not load tasks.</p>
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => void tasksQuery.refetch()}
                >
                  Retry
                </Button>
              </div>
            )}
            {!tasksQuery.isLoading &&
              !tasksQuery.error &&
              tasks.length === 0 && (
                <p className="muted">No tasks yet.</p>
              )}
            {tasks.length > 0 && (
              <ul className="task-list">
                {tasks.map((task) => (
                  <li key={task.id}>
                    <div className="task-row">
                      <Badge
                        tone={
                          task.state === "succeeded"
                            ? "success"
                            : task.state === "failed"
                              ? "danger"
                              : "neutral"
                        }
                      >
                        {task.state}
                      </Badge>
                      <span className="muted">{task.kind}</span>
                    </div>
                    {(task.message !== null ||
                      task.error !== null) && (
                      <p className={task.error ? "err" : "muted"}>
                        {task.error ?? task.message}
                      </p>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </aside>
      </div>
    </section>
  );
}
