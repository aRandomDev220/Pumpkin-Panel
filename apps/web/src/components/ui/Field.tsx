import { cloneElement, isValidElement, useId } from "react";
import type {
  InputHTMLAttributes,
  ReactElement,
  ReactNode,
  SelectHTMLAttributes,
  TextareaHTMLAttributes,
} from "react";

export interface FieldProps {
  label: ReactNode;
  id?: string;
  hint?: ReactNode;
  error?: string | null;
  children: ReactNode;
  className?: string;
}

export default function Field({
  label,
  id,
  hint,
  error,
  children,
  className,
}: FieldProps): React.JSX.Element {
  const generatedId = useId();
  const fieldId = id ?? generatedId;
  const hintId = hint !== undefined ? `${fieldId}-hint` : undefined;
  const errorId = error ? `${fieldId}-error` : undefined;
  const describedBy =
    [hintId, errorId].filter(Boolean).join(" ") || undefined;

  const control = isValidElement(children)
    ? cloneElement(children as ReactElement<Record<string, unknown>>, {
        id: fieldId,
        "aria-describedby": describedBy,
        "aria-invalid": error ? true : undefined,
      })
    : children;

  return (
    <div className={["field", className ?? ""].filter(Boolean).join(" ")}>
      <label className="field-label" htmlFor={fieldId}>
        {label}
      </label>
      {control}
      {hint !== undefined && (
        <span className="field-hint" id={hintId}>
          {hint}
        </span>
      )}
      {error && (
        <span className="field-error" id={errorId} role="alert">
          {error}
        </span>
      )}
    </div>
  );
}

export function Input(
  props: InputHTMLAttributes<HTMLInputElement>,
): React.JSX.Element {
  return <input {...props} />;
}

export function Select(
  props: SelectHTMLAttributes<HTMLSelectElement>,
): React.JSX.Element {
  return <select {...props} />;
}

export function Textarea(
  props: TextareaHTMLAttributes<HTMLTextAreaElement>,
): React.JSX.Element {
  return <textarea {...props} />;
}

export interface CheckboxProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, "type"> {
  label: ReactNode;
  hint?: ReactNode;
  error?: string | null;
}

export function Checkbox({
  label,
  hint,
  error,
  id,
  className,
  ...rest
}: CheckboxProps): React.JSX.Element {
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const hintId = hint !== undefined ? `${inputId}-hint` : undefined;
  const errorId = error ? `${inputId}-error` : undefined;
  const describedBy =
    [hintId, errorId].filter(Boolean).join(" ") || undefined;

  return (
    <div
      className={["field-checkbox-wrap", className ?? ""]
        .filter(Boolean)
        .join(" ")}
    >
      <label className="field-checkbox" htmlFor={inputId}>
        <input
          id={inputId}
          type="checkbox"
          aria-describedby={describedBy}
          aria-invalid={error ? true : undefined}
          {...rest}
        />
        <span className="field-checkbox-text">
          {label}
          {hint !== undefined && (
            <small className="field-hint" id={hintId}>
              {hint}
            </small>
          )}
        </span>
      </label>
      {error && (
        <span className="field-error" id={errorId} role="alert">
          {error}
        </span>
      )}
    </div>
  );
}
