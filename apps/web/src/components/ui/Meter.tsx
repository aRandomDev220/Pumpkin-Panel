import type { ReactNode } from "react";

export interface MeterProps {
  label: ReactNode;
  value: number;
  max: number;
  valueLabel?: ReactNode;
  tone?: "pumpkin" | "success" | "warning" | "danger";
}

export default function Meter({
  label,
  value,
  max,
  valueLabel,
  tone = "pumpkin",
}: MeterProps): React.JSX.Element {
  const safeMax = max > 0 ? max : 1;
  const percent = Math.min(100, Math.max(0, (value / safeMax) * 100));

  return (
    <div className="meter">
      <div className="meter-head">
        <strong>{label}</strong>
        <span>{valueLabel ?? `${Math.round(percent)}%`}</span>
      </div>
      <div
        className="meter-track"
        role="progressbar"
        aria-label={typeof label === "string" ? label : undefined}
        aria-valuenow={value}
        aria-valuemin={0}
        aria-valuemax={max}
      >
        <div
          className={tone === "pumpkin" ? "meter-fill" : `meter-fill meter-fill-${tone}`}
          style={{ width: `${percent}%` }}
        />
      </div>
    </div>
  );
}
