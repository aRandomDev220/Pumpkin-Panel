import { useState } from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { fetchMetricsSummary, listServers } from "../lib/servers";
import {
  METRICS_RANGES,
  fetchAlerts,
  fetchDiagnostics,
  fetchMetricsHistory,
  formatBytesShort,
  isMetricsRange,
  isMonitoringApiError,
} from "../lib/monitoring";
import type {
  BackendAlert,
  MetricSample,
  MetricsRange,
} from "../lib/monitoring";

// ---------------------------------------------------------------------------
// Monitoring page (Phase 6, Task 4).
// CSS-only charts (no chart library). Null metric values are UNAVAILABLE and
// render as gaps — they are never 0-filled.
// ---------------------------------------------------------------------------

const BAR_TRACK_HEIGHT = 64;

function barTitle(
  sample: MetricSample,
  value: number | null,
  unit: string,
): string {
  if (value === null) return `${sample.sampled_at}: unavailable`;
  return `${sample.sampled_at}: ${value}${unit}`;
}

export function MetricHistoryBars({
  samples,
  pick,
  max,
  unit,
  ariaLabel,
}: {
  samples: MetricSample[];
  pick: (s: MetricSample) => number | null;
  max: number;
  unit: string;
  ariaLabel: string;
}): React.JSX.Element {
  const scale = max > 0 ? max : 1;
  return (
    <div
      className="row"
      role="img"
      aria-label={ariaLabel}
      style={{ alignItems: "flex-end", gap: 2 }}
    >
      {samples.map((s) => {
        const value = pick(s);
        if (value === null) {
          return (
            <span
              key={s.sampled_at}
              title={barTitle(s, null, unit)}
              aria-hidden="true"
              style={{
                width: 8,
                height: BAR_TRACK_HEIGHT,
                border: "1px dashed #3a4a42",
                borderRadius: 2,
                boxSizing: "border-box",
              }}
            />
          );
        }
        const height = Math.max(
          2,
          Math.min(100, (value / scale) * 100),
        );
        return (
          <span
            key={s.sampled_at}
            title={barTitle(s, value, unit)}
            aria-hidden="true"
            style={{
              width: 8,
              height: BAR_TRACK_HEIGHT,
              display: "inline-flex",
              alignItems: "flex-end",
              background: "#141c19",
              borderRadius: 2,
            }}
          >
            <span
              style={{
                display: "block",
                width: "100%",
                height: `${height}%`,
                background: "#7dd8a8",
                borderRadius: 2,
              }}
            />
          </span>
        );
      })}
    </div>
  );
}

function maxOf(
  samples: MetricSample[],
  pick: (s: MetricSample) => number | null,
  fallback: number,
): number {
  let max = fallback;
  for (const s of samples) {
    const v = pick(s);
    if (v !== null && v > max) max = v;
  }
  return max;
}

function hasAny(
  samples: MetricSample[],
  pick: (s: MetricSample) => number | null,
): boolean {
  return samples.some((s) => pick(s) !== null);
}

export function PlayersHistoryLine({
  samples,
}: {
  samples: MetricSample[];
}): React.JSX.Element | null {
  if (!hasAny(samples, (s) => s.players_online)) return null;
  const width = Math.max(samples.length, 1);
  const peak = maxOf(samples, (s) => s.players_online, 1);
  const height = 48;
  // Split into segments so null (unavailable) samples break the line
  // instead of being 0-filled.
  const segments: { x: number; y: number }[][] = [];
  let current: { x: number; y: number }[] = [];
  samples.forEach((s, i) => {
    const v = s.players_online;
    if (v === null) {
      if (current.length > 0) segments.push(current);
      current = [];
      return;
    }
    const x = width === 1 ? 0 : (i / (width - 1)) * 100;
    const y = height - 4 - (v / peak) * (height - 8);
    current.push({ x, y });
  });
  if (current.length > 0) segments.push(current);
  return (
    <svg
      viewBox={`0 0 100 ${height}`}
      preserveAspectRatio="none"
      role="img"
      aria-label="Player count history (gaps mark unavailable samples)"
      style={{
        width: "100%",
        height,
        background: "#141c19",
        borderRadius: 4,
        display: "block",
      }}
    >
      {segments.map((seg, i) => (
        <polyline
          key={i}
          points={seg.map((p) => `${p.x},${p.y}`).join(" ")}
          fill="none"
          stroke="#7dd8a8"
          strokeWidth={1.5}
          vectorEffect="non-scaling-stroke"
        />
      ))}
    </svg>
  );
}

export function ServerHistoryPanels({
  samples,
}: {
  samples: MetricSample[];
}): React.JSX.Element {
  if (samples.length === 0) {
    return <p className="muted">No samples in this range.</p>;
  }
  return (
    <div>
      <h4>CPU %</h4>
      <MetricHistoryBars
        samples={samples}
        pick={(s) => s.cpu_percent}
        max={maxOf(samples, (s) => s.cpu_percent, 100)}
        unit="%"
        ariaLabel="CPU percent history"
      />
      <h4>Memory</h4>
      <MetricHistoryBars
        samples={samples}
        pick={(s) => s.memory_bytes}
        max={maxOf(samples, (s) => s.memory_bytes, 1)}
        unit=" bytes"
        ariaLabel="Memory bytes history"
      />
      <h4>Players</h4>
      {hasAny(samples, (s) => s.players_online) ? (
        <PlayersHistoryLine samples={samples} />
      ) : (
        <p className="muted">Player data unavailable in this range.</p>
      )}
      <p className="muted">
        Dashed slots mark unavailable samples. Latest memory:{" "}
        {latestAvailable(samples, (s) => s.memory_bytes) !== null
          ? formatBytesShort(
              latestAvailable(samples, (s) => s.memory_bytes) as number,
            )
          : "unavailable"}
      </p>
    </div>
  );
}

function latestAvailable(
  samples: MetricSample[],
  pick: (s: MetricSample) => number | null,
): number | null {
  for (let i = samples.length - 1; i >= 0; i -= 1) {
    const v = pick(samples[i]);
    if (v !== null) return v;
  }
  return null;
}

function alertClass(severity: string): string {
  const s = severity.toLowerCase();
  if (s === "critical" || s === "error" || s === "err") return "err";
  if (s === "warning" || s === "warn") return "warn";
  return "badge";
}

export function AlertsList({
  alerts,
}: {
  alerts: BackendAlert[];
}): React.JSX.Element {
  if (alerts.length === 0) {
    return <p className="muted">No alerts.</p>;
  }
  return (
    <ul>
      {alerts.map((a, i) => (
        <li key={`${a.kind}-${a.server_id ?? "host"}-${i}`} className={alertClass(a.severity)}>
          <span className="badge">[{a.severity.toUpperCase()}]</span>{" "}
          <span className="muted">{a.kind}</span>{" "}
          {a.server_id ? (
            <Link to={`/servers/${encodeURIComponent(a.server_id)}`}>
              {a.server_id}
            </Link>
          ) : (
            <span className="muted">host</span>
          )}{" "}
          — {a.message}
        </li>
      ))}
    </ul>
  );
}

function DiagnosticsButton(): React.JSX.Element {
  const [downloading, setDownloading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [forbidden, setForbidden] = useState(false);

  // 403-gated: hide the button instead of error-paging.
  if (forbidden) return <></>;

  async function onDownload() {
    setDownloading(true);
    setError(null);
    try {
      const blob = await fetchDiagnostics();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "diagnostics.json";
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
    } catch (err) {
      if (isMonitoringApiError(err, 403)) {
        setForbidden(true);
        return;
      }
      setError(err instanceof Error ? err.message : "Download failed.");
    } finally {
      setDownloading(false);
    }
  }

  return (
    <div className="row">
      <button type="button" onClick={onDownload} disabled={downloading}>
        {downloading ? "Preparing…" : "Download diagnostics (JSON)"}
      </button>
      {error && <span className="err">{error}</span>}
    </div>
  );
}

export default function Monitoring() {
  const [range, setRange] = useState<MetricsRange>("1h");

  const serversQuery = useQuery({
    queryKey: ["servers"],
    queryFn: listServers,
  });
  const summaryQuery = useQuery({
    queryKey: ["metrics", "summary"],
    queryFn: fetchMetricsSummary,
    retry: 1,
  });
  const alertsQuery = useQuery({
    queryKey: ["alerts"],
    queryFn: fetchAlerts,
    retry: 1,
  });

  const servers = serversQuery.data?.servers ?? [];
  const historyQueries = useQueries({
    queries: servers.map((s) => ({
      queryKey: ["servers", s.id, "metrics", "history", range],
      queryFn: () => fetchMetricsHistory(s.id, range),
      retry: 1,
      staleTime: 30_000,
    })),
  });

  return (
    <section>
      <h2>Monitoring</h2>

      <h3>Host</h3>
      {summaryQuery.isLoading && <p>Loading host stats…</p>}
      {summaryQuery.error && <p className="err">Failed to load host stats.</p>}
      {summaryQuery.data && (
        <div className="cards" aria-label="Host stats">
          <div className="card">
            <div className="card-num">
              {summaryQuery.data.host.cpu_percent.toFixed(1)}%
            </div>
            <div className="muted">host CPU</div>
          </div>
          <div className="card">
            <div className="card-num">
              {formatBytesShort(summaryQuery.data.host.memory_used_bytes)} /{" "}
              {formatBytesShort(summaryQuery.data.host.memory_total_bytes)}
            </div>
            <div className="muted">memory used / total</div>
          </div>
          <div className="card">
            <div className="card-num">
              {formatBytesShort(summaryQuery.data.host.disk_free_bytes)} free
            </div>
            <div className="muted">
              of {formatBytesShort(summaryQuery.data.host.disk_total_bytes)}{" "}
              disk
            </div>
          </div>
        </div>
      )}

      <h3>Server history</h3>
      <div className="row">
        <label>
          Range{" "}
          <select
            value={range}
            onChange={(e) => {
              const v = e.target.value;
              if (isMetricsRange(v)) setRange(v);
            }}
            aria-label="Metrics history range"
          >
            {METRICS_RANGES.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        </label>
      </div>
      {serversQuery.isLoading && <p>Loading servers…</p>}
      {serversQuery.error && <p className="err">Failed to load servers.</p>}
      {!serversQuery.isLoading &&
        !serversQuery.error &&
        servers.length === 0 && (
          <p className="muted">No servers to chart yet.</p>
        )}
      {servers.map((s, i) => {
        const q = historyQueries[i];
        return (
          <div key={s.id} className="card" style={{ margin: "12px 0" }}>
            <h4 style={{ margin: "0 0 8px" }}>
              <Link to={`/servers/${encodeURIComponent(s.id)}`}>{s.name}</Link>{" "}
              <span className="muted">({range})</span>
            </h4>
            {q?.isLoading && <p>Loading history…</p>}
            {q?.error && <p className="err">Failed to load history.</p>}
            {q?.data && <ServerHistoryPanels samples={q.data.samples} />}
          </div>
        );
      })}

      <h3>Alerts</h3>
      {alertsQuery.isLoading && <p>Loading alerts…</p>}
      {alertsQuery.error && <p className="err">Failed to load alerts.</p>}
      {alertsQuery.data && <AlertsList alerts={alertsQuery.data.alerts} />}

      <h3>Diagnostics</h3>
      <p className="muted">
        Requires admin access. The button hides itself if your account is
        not permitted.
      </p>
      <DiagnosticsButton />
    </section>
  );
}
