import { useEffect, useId, useRef } from "react";
import type { ReactNode } from "react";

export interface DialogProps {
  open: boolean;
  onClose: () => void;
  title: string;
  description?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  wide?: boolean;
}

export default function Dialog({
  open,
  onClose,
  title,
  description,
  children,
  actions,
  wide = false,
}: DialogProps): React.JSX.Element {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const previousFocus = useRef<HTMLElement | null>(null);
  const titleId = useId();

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;

    if (open) {
      previousFocus.current =
        document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null;
      if (!dialog.open) dialog.showModal();
      const first = dialog.querySelector<HTMLElement>(
        "[data-autofocus], input, select, textarea, button",
      );
      first?.focus();
      return () => {
        previousFocus.current?.focus();
      };
    }

    if (dialog.open) dialog.close();
    return undefined;
  }, [open]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const onCancel = (event: Event) => {
      event.preventDefault();
      onClose();
    };
    dialog.addEventListener("cancel", onCancel);
    return () => dialog.removeEventListener("cancel", onCancel);
  }, [onClose]);

  return (
    <dialog
      ref={dialogRef}
      className={wide ? "dialog dialog-wide" : "dialog"}
      aria-labelledby={titleId}
      onClick={(event) => {
        if (event.target === dialogRef.current) onClose();
      }}
    >
      <div className="dialog-card">
        <h2 className="dialog-title" id={titleId}>
          {title}
        </h2>
        {description !== undefined && (
          <p className="dialog-description">{description}</p>
        )}
        {children !== undefined && <div className="dialog-body">{children}</div>}
        {actions !== undefined && (
          <div className="dialog-actions">{actions}</div>
        )}
      </div>
    </dialog>
  );
}
