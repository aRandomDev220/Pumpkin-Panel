const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"] as const;
const ANSI_ESCAPE = new RegExp(
  `${String.fromCharCode(27)}\\[[0-?]*[ -/]*[@-~]`,
  "g",
);

export function stripAnsi(value: string): string {
  return value.replace(ANSI_ESCAPE, "");
}

export function formatBytes(
  bytes: number | null | undefined,
): string {
  if (
    bytes === null ||
    bytes === undefined ||
    !Number.isFinite(bytes) ||
    bytes < 0
  ) {
    return "n/a";
  }
  if (bytes < 1024) return `${Math.round(bytes)} B`;

  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${BYTE_UNITS[unit]}`;
}

export function formatPercent(
  value: number | null | undefined,
  digits = 1,
): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return "n/a";
  }
  return `${value.toFixed(digits)}%`;
}

export function formatPort(port: number | string | null | undefined): string {
  if (port === null || port === undefined || port === "") return "—";
  return String(port);
}

function parseTimestamp(
  value: string | number | Date | null | undefined,
): Date | null {
  if (value === null || value === undefined || value === "") return null;
  if (value instanceof Date) {
    return Number.isNaN(value.getTime()) ? null : value;
  }
  if (typeof value === "number") {
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? null : date;
  }
  const normalized = /^\d{4}-\d{2}-\d{2} \d/.test(value)
    ? value.replace(" ", "T")
    : value;
  const date = new Date(normalized);
  return Number.isNaN(date.getTime()) ? null : date;
}

export function formatDateTime(
  value: string | number | Date | null | undefined,
): string {
  const date = parseTimestamp(value);
  if (date === null) return value ? String(value) : "—";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

export function formatRelativeTime(
  value: string | number | Date | null | undefined,
): string {
  const date = parseTimestamp(value);
  if (date === null) return value ? String(value) : "—";

  const formatter = new Intl.RelativeTimeFormat(undefined, {
    numeric: "auto",
  });
  const seconds = (date.getTime() - Date.now()) / 1000;
  const absolute = Math.abs(seconds);

  if (absolute < 60) return formatter.format(Math.round(seconds), "second");
  if (absolute < 3600) {
    return formatter.format(Math.round(seconds / 60), "minute");
  }
  if (absolute < 86_400) {
    return formatter.format(Math.round(seconds / 3600), "hour");
  }
  return formatter.format(Math.round(seconds / 86_400), "day");
}
