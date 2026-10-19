import type { ReactNode } from "react";
import Button from "./Button";
import Dialog from "./Dialog";
import { Checkbox } from "./Field";

export interface ConfirmDialogProps {
  open: boolean;
  onClose: () => void;
  onConfirm: () => void;
  title: string;
  description?: ReactNode;
  confirmLabel: string;
  cancelLabel?: string;
  tone?: "danger" | "primary";
  busy?: boolean;
  children?: ReactNode;
  checkbox?: {
    label: ReactNode;
    hint?: ReactNode;
    checked: boolean;
    onChange: (checked: boolean) => void;
  };
}

export default function ConfirmDialog({
  open,
  onClose,
  onConfirm,
  title,
  description,
  confirmLabel,
  cancelLabel = "Cancel",
  tone = "danger",
  busy = false,
  children,
  checkbox,
}: ConfirmDialogProps): React.JSX.Element {
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={title}
      description={description}
      actions={
        <>
          <Button variant="secondary" onClick={onClose} disabled={busy}>
            {cancelLabel}
          </Button>
          <Button
            variant={tone === "danger" ? "danger" : "primary"}
            onClick={onConfirm}
            loading={busy}
          >
            {confirmLabel}
          </Button>
        </>
      }
    >
      {children}
      {checkbox !== undefined && (
        <Checkbox
          label={checkbox.label}
          hint={checkbox.hint}
          checked={checkbox.checked}
          onChange={(event) => checkbox.onChange(event.target.checked)}
        />
      )}
    </Dialog>
  );
}
