import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useParams } from "react-router-dom";
import {
  ConfigConflictError,
  ConfigValidationError,
  fetchConfig,
  listConfigRevisions,
  normalizeErrors,
  revertConfig,
  saveConfig,
  validateConfig,
} from "../lib/config";
import type { GuidedConfig } from "../lib/config";

function formatTomlValue(value: string | number | boolean): string {
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") return Number.isFinite(value) ? String(value) : "0";
  const escaped = value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  return `"${escaped}"`;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

interface GuidedTarget {
  scope: "top" | "nested";
  key: string;
}

// Exact TOML paths from `crates/backend/src/config_profile.rs`: top-level
// keys patch the document preamble (before the first `[section]`); nested
// keys are dual-written to BOTH `[networking.java]` and `[networking.bedrock]`
// (server reads java-first, falls back to bedrock).
const GUIDED_TARGETS: Record<string, GuidedTarget> = {
  seed: { scope: "top", key: "seed" },
  default_difficulty: { scope: "top", key: "default_difficulty" },
  default_gamemode: { scope: "top", key: "default_gamemode" },
  default_level_name: { scope: "top", key: "default_level_name" },
  hardcore: { scope: "top", key: "hardcore" },
  white_list: { scope: "top", key: "white_list" },
  enforce_whitelist: { scope: "top", key: "enforce_whitelist" },
  tps: { scope: "top", key: "tps" },
  motd: { scope: "nested", key: "motd" },
  max_players: { scope: "nested", key: "max_players" },
  view_distance: { scope: "nested", key: "view_distance" },
  simulation_distance: { scope: "nested", key: "simulation_distance" },
  online_mode: { scope: "nested", key: "online_mode" },
};

function isSectionHeader(line: string): boolean {
  return /^\s*\[[^\]]+\]\s*$/.test(line);
}

function keyLine(key: string, value: string | number | boolean): string {
  return `${key} = ${formatTomlValue(value)}`;
}

function patchTopLevel(content: string, key: string, value: string | number | boolean): string {
  if (content === "") return keyLine(key, value);
  const lines = content.split("\n");
  let firstHeader = lines.findIndex(isSectionHeader);
  if (firstHeader < 0) firstHeader = lines.length;
  const re = new RegExp(`^\\s*${escapeRegExp(key)}\\s*=.*$`);
  for (let i = 0; i < firstHeader; i++) {
    if (re.test(lines[i])) {
      lines[i] = keyLine(key, value);
      return lines.join("\n");
    }
  }
  lines.splice(firstHeader, 0, keyLine(key, value));
  return lines.join("\n");
}

function patchSection(
  lines: string[],
  header: string,
  key: string,
  value: string | number | boolean,
): string[] {
  const out = [...lines];
  const headerIdx = out.findIndex((l) => l.trim() === header);
  const re = new RegExp(`^\\s*${escapeRegExp(key)}\\s*=.*$`);
  if (headerIdx < 0) {
    if (out.length > 0 && out[out.length - 1] !== "") out.push("");
    out.push(header, keyLine(key, value));
    return out;
  }
  let end = out.length;
  for (let i = headerIdx + 1; i < out.length; i++) {
    if (isSectionHeader(out[i])) {
      end = i;
      break;
    }
  }
  for (let i = headerIdx + 1; i < end; i++) {
    if (re.test(out[i])) {
      out[i] = keyLine(key, value);
      return out;
    }
  }
  out.splice(headerIdx + 1, 0, keyLine(key, value));
  return out;
}

function patchNested(content: string, key: string, value: string | number | boolean): string {
  let lines = content.split("\n");
  for (const side of ["java", "bedrock"] as const) {
    lines = patchSection(lines, `[networking.${side}]`, key, value);
  }
  return lines.join("\n");
}

function patchGuidedField(
  content: string,
  field: string,
  value: string | number | boolean,
): string {
  const target = GUIDED_TARGETS[field];
  if (!target) return patchTopLevel(content, field, value);
  return target.scope === "nested"
    ? patchNested(content, target.key, value)
    : patchTopLevel(content, target.key, value);
}

function guidedValue(g: GuidedConfig, key: string): string {
  const v = g[key];
  if (v === undefined || v === null) return "";
  return String(v);
}

function guidedChecked(g: GuidedConfig, key: string): boolean {
  return g[key] === true;
}

function parseNumberOr(original: string, fallback: number | undefined): number | string {
  if (original.trim() === "") return "";
  const n = Number(original);
  if (!Number.isFinite(n)) return original;
  if (fallback === undefined && original.trim() === "") return "";
  return n;
}

export default function Config() {
  const { id } = useParams<{ id: string }>();
  const qc = useQueryClient();
  const [content, setContent] = useState("");
  const [guided, setGuided] = useState<GuidedConfig>({});
  const [syncedRevision, setSyncedRevision] = useState<number | null>(null);
  const [conflict, setConflict] = useState(false);
  const [saveErrors, setSaveErrors] = useState<string[]>([]);
  const [validateErrors, setValidateErrors] = useState<string[] | null>(null);
  const [validateOk, setValidateOk] = useState(false);
  const [restartRequired, setRestartRequired] = useState(false);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<number | null>(null);

  const configQuery = useQuery({
    queryKey: ["servers", id, "config"],
    queryFn: () => fetchConfig(id ?? ""),
    enabled: !!id,
    retry: 1,
  });

  const revisionsQuery = useQuery({
    queryKey: ["servers", id, "config", "revisions"],
    queryFn: () => listConfigRevisions(id ?? ""),
    enabled: !!id,
    retry: 1,
  });

  useEffect(() => {
    const data = configQuery.data;
    if (!data) return;
    if (syncedRevision === data.revision) return;
    setContent(data.content ?? "");
    setGuided({ ...(data.guided ?? {}) });
    setSyncedRevision(data.revision);
    setConflict(false);
    setSaveErrors([]);
    setRestartRequired(false);
    setSaveMessage(null);
  }, [configQuery.data, syncedRevision]);

  const validateMutation = useMutation({
    mutationFn: (text: string) => validateConfig(id ?? "", text),
    onSuccess: (result) => {
      setValidateErrors(normalizeErrors(result.errors));
      setValidateOk(result.ok);
    },
    onError: (err) => {
      setValidateOk(false);
      setValidateErrors([err instanceof Error ? err.message : "Validation failed."]);
    },
  });

  const saveMutation = useMutation({
    mutationFn: (text: string) => saveConfig(id ?? "", configQuery.data?.revision ?? 0, text),
    onSuccess: (result) => {
      setConflict(false);
      setSaveErrors([]);
      setRestartRequired(result.restart_required);
      setSaveMessage(`Saved revision ${result.revision} (${result.hash}).`);
      setSyncedRevision(result.revision);
      qc.invalidateQueries({ queryKey: ["servers", id, "config"] });
      qc.invalidateQueries({ queryKey: ["servers", id, "config", "revisions"] });
    },
    onError: (err) => {
      if (err instanceof ConfigConflictError) {
        setConflict(true);
        setSaveMessage(null);
      } else if (err instanceof ConfigValidationError) {
        setSaveErrors(err.errors);
      } else {
        setSaveErrors([err instanceof Error ? err.message : "Save failed."]);
      }
    },
  });

  const revertMutation = useMutation({
    mutationFn: (revision: number) => revertConfig(id ?? "", revision),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["servers", id, "config"] });
      qc.invalidateQueries({ queryKey: ["servers", id, "config", "revisions"] });
    },
  });

  if (!id) return <p className="err">Missing server id.</p>;

  function updateGuidedText(key: string, raw: string) {
    setGuided((g) => ({ ...g, [key]: raw }));
    setContent((c) => patchGuidedField(c, key, raw));
  }

  function updateGuidedNumber(key: string, raw: string) {
    const parsed = parseNumberOr(raw, typeof guided[key] === "number" ? (guided[key] as number) : undefined);
    setGuided((g) => ({ ...g, [key]: parsed === "" ? undefined : parsed }));
    if (typeof parsed === "number") setContent((c) => patchGuidedField(c, key, parsed));
  }

  function updateGuidedBool(key: string, checked: boolean) {
    setGuided((g) => ({ ...g, [key]: checked }));
    setContent((c) => patchGuidedField(c, key, checked));
  }

  const data = configQuery.data;
  const dirty = syncedRevision !== null && data != null && content !== data.content;

  return (
    <section>
      <div className="section-heading">
        <h2>Config</h2>
        <span className="muted">Guided settings and raw TOML</span>
      </div>

      {configQuery.isLoading && <p>Loading config…</p>}
      {configQuery.error && <p className="err">Failed to load config.</p>}
      {configQuery.data && configQuery.data.content === "" && (
        <p className="muted">Config is empty.</p>
      )}

      {data?.profile && data.profile.known === false && (
        <p className="warn" role="alert">
          Unknown server profile (pumpkin_version: {data.profile.pumpkin_version || "unknown"}).
          Guided fields may not match this version — prefer raw TOML and validate before saving.
        </p>
      )}

      {data?.valid === false && (
        <div className="err" role="alert">
          <p>
            The on-disk <code>pumpkin.toml</code> is invalid — fix it below and Save
            (save still validates via PUT).
          </p>
          {data.errors && data.errors.length > 0 && (
            <ul>
              {normalizeErrors(data.errors).map((e, i) => (
                <li key={i}>{e}</li>
              ))}
            </ul>
          )}
        </div>
      )}

      {conflict && (
        <p className="err" role="alert">
          Save conflict (409): the config changed on the server.{" "}
          <button type="button" onClick={() => configQuery.refetch()}>
            Reload
          </button>
        </p>
      )}

      {restartRequired && (
        <p className="warn" role="status">
          Saved — a restart is required for changes to take effect.
        </p>
      )}
      {saveMessage && !conflict && (
        <p className="muted" role="status">
          {saveMessage}
        </p>
      )}
      {saveErrors.length > 0 && (
        <div className="err" role="alert">
          <p>Save rejected (422):</p>
          <ul>
            {saveErrors.map((e, i) => (
              <li key={i}>{e}</li>
            ))}
          </ul>
        </div>
      )}

      {data && (
        <>
            <p className="config-status">
            revision {data.revision} · hash <code>{data.hash}</code>
            {dirty ? " · unsaved changes" : " · no unsaved changes"}
          </p>

           <div className="section-heading"><h3>Guided settings</h3><span className="muted">Common server controls</span></div>
          <p className="muted">
            Editing a guided field patches the raw TOML below. Top-level fields edit the
            document preamble; fields marked “Java + Bedrock” are written to both{" "}
            <code>[networking.java]</code> and <code>[networking.bedrock]</code> together —
            use raw TOML for per-edition control.
          </p>
          <div className="row">
            <label>
              Seed{" "}
              <input
                value={guidedValue(guided, "seed")}
                onChange={(e) => updateGuidedText("seed", e.target.value)}
                placeholder="seed"
              />
            </label>
            <label>
              Difficulty{" "}
              <input
                value={guidedValue(guided, "default_difficulty")}
                onChange={(e) => updateGuidedText("default_difficulty", e.target.value)}
                placeholder="peaceful / easy / normal / hard"
              />
            </label>
            <label>
              Gamemode{" "}
              <input
                value={guidedValue(guided, "default_gamemode")}
                onChange={(e) => updateGuidedText("default_gamemode", e.target.value)}
                placeholder="survival / creative / adventure / spectator"
              />
            </label>
            <label>
              Level name{" "}
              <input
                value={guidedValue(guided, "default_level_name")}
                onChange={(e) => updateGuidedText("default_level_name", e.target.value)}
                placeholder="world"
              />
            </label>
          </div>
          <div className="row">
            <label>
              <input
                type="checkbox"
                checked={guidedChecked(guided, "hardcore")}
                onChange={(e) => updateGuidedBool("hardcore", e.target.checked)}
              />{" "}
              Hardcore
            </label>
            <label>
              <input
                type="checkbox"
                checked={guidedChecked(guided, "white_list")}
                onChange={(e) => updateGuidedBool("white_list", e.target.checked)}
              />{" "}
              Whitelist enabled
            </label>
            <label>
              <input
                type="checkbox"
                checked={guidedChecked(guided, "enforce_whitelist")}
                onChange={(e) => updateGuidedBool("enforce_whitelist", e.target.checked)}
              />{" "}
              Whitelist enforce
            </label>
            <label>
              <input
                type="checkbox"
                checked={guidedChecked(guided, "online_mode")}
                onChange={(e) => updateGuidedBool("online_mode", e.target.checked)}
              />{" "}
              Online mode (Java + Bedrock)
            </label>
          </div>
          <div className="row">
            <label>
              TPS{" "}
              <input
                type="number"
                value={guidedValue(guided, "tps")}
                onChange={(e) => updateGuidedNumber("tps", e.target.value)}
                placeholder="20"
              />
            </label>
            <label>
              MOTD (Java + Bedrock){" "}
              <input
                value={guidedValue(guided, "motd")}
                onChange={(e) => updateGuidedText("motd", e.target.value)}
                placeholder="A Pumpkin server"
              />
            </label>
            <label>
              Max players (Java + Bedrock){" "}
              <input
                type="number"
                value={guidedValue(guided, "max_players")}
                onChange={(e) => updateGuidedNumber("max_players", e.target.value)}
                placeholder="20"
              />
            </label>
            <label>
              View distance (Java + Bedrock){" "}
              <input
                type="number"
                value={guidedValue(guided, "view_distance")}
                onChange={(e) => updateGuidedNumber("view_distance", e.target.value)}
                placeholder="10"
              />
            </label>
            <label>
              Simulation distance (Java + Bedrock){" "}
              <input
                type="number"
                value={guidedValue(guided, "simulation_distance")}
                onChange={(e) => updateGuidedNumber("simulation_distance", e.target.value)}
                placeholder="10"
              />
            </label>
          </div>

           <div className="section-heading"><h3>Raw TOML</h3><span className="muted">Preserves unknown keys</span></div>
           <label className="config-editor">
            <span className="muted">revision {data.revision} content</span>
            <textarea
              value={content}
              onChange={(e) => setContent(e.target.value)}
              rows={18}
              cols={80}
              spellCheck={false}
              style={{ display: "block", width: "100%", fontFamily: "monospace" }}
            />
          </label>
          <div className="row">
            <button
              type="button"
              disabled={validateMutation.isPending}
              onClick={() => {
                setValidateOk(false);
                setValidateErrors(null);
                validateMutation.mutate(content);
              }}
            >
              {validateMutation.isPending ? "Validating…" : "Validate"}
            </button>
            <button
              type="button"
              disabled={saveMutation.isPending}
              onClick={() => {
                setSaveErrors([]);
                saveMutation.mutate(content);
              }}
            >
              {saveMutation.isPending ? "Saving…" : "Save"}
            </button>
          </div>
          {validateMutation.isPending && <p>Validating…</p>}
          {validateErrors !== null && (
            <div role={validateOk ? "status" : "alert"} className={validateOk ? "muted" : "err"}>
              {validateOk && validateErrors.length === 0 ? (
                <p>Config is valid.</p>
              ) : (
                <>
                  <p>{validateOk ? "Valid with warnings:" : "Validation errors:"}</p>
                  <ul>
                    {validateErrors.map((e, i) => (
                      <li key={i}>{e}</li>
                    ))}
                  </ul>
                </>
              )}
            </div>
          )}

           <div className="section-heading"><h3>Revision history</h3><span className="muted">{revisionsQuery.data?.total ?? 0} revisions</span></div>
          {revisionsQuery.isLoading && <p>Loading revisions…</p>}
          {revisionsQuery.error && <p className="err">Failed to load revisions.</p>}
          {revisionsQuery.data && revisionsQuery.data.revisions.length === 0 && (
            <p className="muted">No revisions recorded.</p>
          )}
          {revisionsQuery.data && revisionsQuery.data.revisions.length > 0 && (
            <>
              <p className="muted">total {revisionsQuery.data.total}</p>
              <ul>
                {revisionsQuery.data.revisions.map((r) => (
                  <li key={r.revision}>
                    <button
                      type="button"
                      onClick={() => setExpanded((cur) => (cur === r.revision ? null : r.revision))}
                      aria-expanded={expanded === r.revision}
                    >
                      revision {r.revision}
                    </button>{" "}
                    <span className="muted">
                      {r.hash} · {r.created_at} · {r.created_by ?? "—"}
                    </span>{" "}
                    <button
                      type="button"
                      disabled={revertMutation.isPending}
                      onClick={() => {
                        if (window.confirm(`Revert config to revision ${r.revision}?`)) {
                          revertMutation.mutate(r.revision);
                        }
                      }}
                    >
                      Revert
                    </button>
                    {expanded === r.revision && (
                      <>
                        <p className="muted">
                          revision {r.revision} · {r.hash} · {r.created_at} ·{" "}
                          {r.created_by ?? "—"}
                        </p>
                        <pre className="log" tabIndex={0}>
                          {r.content || "(empty)"}
                        </pre>
                      </>
                    )}
                  </li>
                ))}
              </ul>
            </>
          )}
          {revertMutation.error && (
            <p className="err" role="alert">
              {revertMutation.error instanceof Error ? revertMutation.error.message : "Revert failed."}
            </p>
          )}
        </>
      )}
    </section>
  );
}
