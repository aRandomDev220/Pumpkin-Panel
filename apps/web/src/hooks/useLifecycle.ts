import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useToast } from "../components/ui/toast-context";
import { humanizeError } from "../lib/errors";
import {
  fetchTask,
  isLifecycleAccepted,
  isTerminalTaskState,
  postLifecycle,
} from "../lib/servers";
import type { LifecycleOp, TaskStatus } from "../lib/servers";

const OP_LABEL: Record<LifecycleOp, string> = {
  start: "Start",
  stop: "Stop",
  restart: "Restart",
};

export interface LifecycleController {
  run: (op: LifecycleOp) => void;
  isBusy: boolean;
  activeOp: LifecycleOp | null;
  task: TaskStatus | undefined;
  taskId: string | null;
}

export function useLifecycle(
  serverId: string,
  serverName?: string,
): LifecycleController {
  const queryClient = useQueryClient();
  const toast = useToast();
  const [activeTaskId, setActiveTaskId] = useState<string | null>(null);
  const [activeOp, setActiveOp] = useState<LifecycleOp | null>(null);
  const notifiedTaskId = useRef<string | null>(null);
  const prefix = serverName !== undefined ? `${serverName}: ` : "";

  const taskQuery = useQuery<TaskStatus>({
    queryKey: ["tasks", activeTaskId],
    queryFn: () => fetchTask(activeTaskId ?? ""),
    enabled: activeTaskId !== null && activeTaskId !== "",
    refetchInterval: (query) => {
      const state = query.state.data?.state;
      return state !== undefined && isTerminalTaskState(state) ? false : 2000;
    },
    retry: false,
  });

  const mutation = useMutation({
    mutationFn: (op: LifecycleOp) => postLifecycle(serverId, op),
    onMutate: (op) => {
      setActiveOp(op);
    },
    onSuccess: (result, op) => {
      if (isLifecycleAccepted(result)) {
        setActiveTaskId(result.task_id);
        toast.info(`${prefix}${OP_LABEL[op]} requested`);
      } else {
        toast.info(`${prefix}Server is already ${result.desired_state}.`);
      }
    },
    onError: (error) => {
      toast.error(humanizeError(error));
    },
  });

  useEffect(() => {
    const task = taskQuery.data;
    if (task === undefined || !isTerminalTaskState(task.state)) return;
    if (notifiedTaskId.current === task.id) return;
    notifiedTaskId.current = task.id;

    void queryClient.invalidateQueries({ queryKey: ["servers"] });
    void queryClient.invalidateQueries({ queryKey: ["servers", serverId] });
    void queryClient.invalidateQueries({
      queryKey: ["servers", serverId, "status"],
    });
    void queryClient.invalidateQueries({
      queryKey: ["servers", serverId, "tasks"],
    });
    void queryClient.invalidateQueries({ queryKey: ["metrics", "summary"] });
    void queryClient.invalidateQueries({ queryKey: ["alerts"] });

    const label = activeOp !== null ? OP_LABEL[activeOp] : "Operation";
    if (task.state === "succeeded") {
      toast.success(`${prefix}${label} complete`);
    } else {
      toast.error(
        task.error ?? `${prefix}${label} failed (${task.state}).`,
      );
    }
  }, [
    taskQuery.data,
    activeOp,
    queryClient,
    serverId,
    toast,
    prefix,
  ]);

  const taskState = taskQuery.data?.state;
  const taskInFlight =
    activeTaskId !== null &&
    (taskState === undefined || !isTerminalTaskState(taskState));

  return {
    run: (op: LifecycleOp) => mutation.mutate(op),
    isBusy: mutation.isPending || taskInFlight,
    activeOp,
    task: taskQuery.data,
    taskId: activeTaskId,
  };
}
