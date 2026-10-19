import { useState } from "react";
import { Play, RotateCcw, Square } from "lucide-react";
import Button from "../ui/Button";
import ConfirmDialog from "../ui/ConfirmDialog";
import type { LifecycleController } from "../../hooks/useLifecycle";

export interface LifecycleButtonsProps {
  lifecycle: LifecycleController;
  serverName: string;
  running: boolean;
}

export default function LifecycleButtons({
  lifecycle,
  serverName,
  running,
}: LifecycleButtonsProps): React.JSX.Element {
  const [confirmOp, setConfirmOp] = useState<"stop" | "restart" | null>(null);

  const stopTitle =
    confirmOp === "stop" ? `Stop ${serverName}?` : `Restart ${serverName}?`;
  const stopDescription =
    confirmOp === "stop"
      ? "Connected players will be disconnected. The world is saved before the server stops."
      : "Connected players will briefly disconnect while the server restarts.";

  return (
    <>
      <Button
        size="sm"
        variant="primary"
        icon={<Play size={14} />}
        disabled={lifecycle.isBusy || running}
        loading={lifecycle.isBusy && lifecycle.activeOp === "start"}
        onClick={() => lifecycle.run("start")}
      >
        Start
      </Button>
      <Button
        size="sm"
        variant="secondary"
        icon={<RotateCcw size={14} />}
        disabled={lifecycle.isBusy || !running}
        loading={lifecycle.isBusy && lifecycle.activeOp === "restart"}
        onClick={() => setConfirmOp("restart")}
      >
        Restart
      </Button>
      <Button
        size="sm"
        variant="danger"
        icon={<Square size={14} />}
        disabled={lifecycle.isBusy || !running}
        loading={lifecycle.isBusy && lifecycle.activeOp === "stop"}
        onClick={() => setConfirmOp("stop")}
      >
        Stop
      </Button>

      <ConfirmDialog
        open={confirmOp !== null}
        onClose={() => setConfirmOp(null)}
        onConfirm={() => {
          if (confirmOp !== null) {
            lifecycle.run(confirmOp);
            setConfirmOp(null);
          }
        }}
        title={stopTitle}
        description={stopDescription}
        confirmLabel={confirmOp === "stop" ? "Stop server" : "Restart server"}
        tone={confirmOp === "stop" ? "danger" : "primary"}
        busy={lifecycle.isBusy}
      />
    </>
  );
}
