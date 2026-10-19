import { useEffect, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  fetchTask,
  isTerminalTaskState,
} from "../lib/servers";
import type { TaskStatus } from "../lib/servers";

export interface TaskPollingResult {
  task: TaskStatus | undefined;
  isLoading: boolean;
}

export function useTaskPolling(
  taskId: string | null,
  onTerminal: (task: TaskStatus) => void,
): TaskPollingResult {
  const notified = useRef<string | null>(null);

  const query = useQuery<TaskStatus>({
    queryKey: ["tasks", taskId],
    queryFn: () => fetchTask(taskId ?? ""),
    enabled: taskId !== null && taskId !== "",
    refetchInterval: (current) => {
      const state = current.state.data?.state;
      return state !== undefined && isTerminalTaskState(state)
        ? false
        : 2000;
    },
    retry: false,
  });

  useEffect(() => {
    const task = query.data;
    if (task === undefined || !isTerminalTaskState(task.state)) return;
    if (notified.current === task.id) return;
    notified.current = task.id;
    onTerminal(task);
  }, [query.data, onTerminal]);

  return { task: query.data, isLoading: query.isLoading };
}

export function isTaskInFlight(task: TaskStatus | undefined): boolean {
  return task !== undefined && !isTerminalTaskState(task.state);
}
