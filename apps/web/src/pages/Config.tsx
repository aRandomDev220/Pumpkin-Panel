import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
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

function patchTomlKey(content: string, key: string, value: string | number | boolean): string {
  const line = `${key} = ${formatTomlValue(value)}`;
  const re = new RegExp(`^${key}\\s*=.*$`, "m");
  if (re.test(content)) return content.replace(re, line);
  const trimmed = content.endsWith("\n") || content === "" ? content : `${content}\n`;
  return `${trimmed}${line}\n`;
}

function guidedValue(g: GuidedConfig, key: string): string {
  const v = g[key];
  if (v === undefined) return "";
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
    setContent((c) => patchTomlKey(c, key, raw));
  }

  function updateGuidedNumber(key: string, raw: string) {
    const parsed = parseNumberOr(raw, typeof guided[key] === "number" ? (guided[key] as number) : undefined);
    setGuided((g) => ({ ...g, [key]: parsed === "" ? undefined : parsed }));
    if (typeof parsed === "number") setContent((c) => patchTomlKey(c, key, parsed));
  }

  function updateGuidedBool(key: string, checked: boolean) {
    setGuided((g) => ({ ...g, [key]: checked }));
    setContent((c) => patchTomlKey(c, key, checked));
  }

  const data = configQuery.data;
  const dirty = syncedRevision !== null && data != null && content !== data.content;

  return (
    <section>
      <p className="muted">
        <Link to={`/servers/${encodeURIComponent(id)}`}>← Server</Link>
        {" · "}
        <Link to={`/servers/${encodeURIComponent(id)}/files`}>Files</Link>
      </p>
      <h2>Config</h2>

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
          <p className="muted">
            revision {data.revision} · hash <code>{data.hash}</code>
            {dirty ? " · unsaved changes" : " · no unsaved changes"}
          </p>

          <h3>Guided settings</h3>
          <p className="muted">
            Editing a guided field patches the matching <code>key = value</code> line in the raw
            TOML below (appended if absent).
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
                value={guidedValue(guided, "difficulty")}
                onChange={(e) => updateGuidedText("difficulty", e.target.value)}
                placeholder="peaceful / easy / normal / hard"
              />
            </label>
            <label>
              Gamemode{" "}
              <input
                value={guidedValue(guided, "gamemode")}
                onChange={(e) => updateGuidedText("gamemode", e.target.value)}
                placeholder="survival / creative / adventure / spectator"
              />
            </label>
            <label>
              Level name{" "}
              <input
                value={guidedValue(guided, "level_name")}
                onChange={(e) => updateGuidedText("level_name", e.target.value)}
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
                checked={guidedChecked(guided, "whitelist_enabled")}
                onChange={(e) => updateGuidedBool("whitelist_enabled", e.target.checked)}
              />{" "}
              Whitelist enabled
            </label>
            <label>
              <input
                type="checkbox"
                checked={guidedChecked(guided, "whitelist_enforce")}
                onChange={(e) => updateGuidedBool("whitelist_enforce", e.target.checked)}
              />{" "}
              Whitelist enforce
            </label>
            <label>
              <input
                type="checkbox"
                checked={guidedChecked(guided, "online_mode")}
                onChange={(e) => updateGuidedBool("online_mode", e.target.checked)}
              />{" "}
              Online mode
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
              MOTD{" "}
              <input
                value={guidedValue(guided, "motd")}
                onChange={(e) => updateGuidedText("motd", e.target.value)}
                placeholder="A Pumpkin server"
              />
            </label>
            <label>
              Max players{" "}
              <input
                type="number"
                value={guidedValue(guided, "max_players")}
                onChange={(e) => updateGuidedNumber("max_players", e.target.value)}
                placeholder="20"
              />
            </label>
            <label>
              View distance{" "}
              <input
                type="number"
                value={guidedValue(guided, "view_distance")}
                onChange={(e) => updateGuidedNumber("view_distance", e.target.value)}
                placeholder="10"
              />
            </label>
            <label>
              Simulation distance{" "}
              <input
                type="number"
                value={guidedValue(guided, "simulation_distance")}
                onChange={(e) => updateGuidedNumber("simulation_distance", e.target.value)}
                placeholder="10"
              />
            </label>
          </div>

          <h3>Raw TOML</h3>
          <label>
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

          <h3>Revision history</h3>
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
                      {r.hash} · {r.created_at} · {r.created_by}
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
                      <pre className="log" tabIndex={0}>
                        revision: {r.revision}
                        {"\n"}hash: {r.hash}
                        {"\n"}created_at: {r.created_at}
                        {"\n"}created_by: {r.created_by}
                        {"\n\n"}(Full content preview is unavailable from the revisions
                        endpoint; reload the config after revert to inspect.)
                      </pre>
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
