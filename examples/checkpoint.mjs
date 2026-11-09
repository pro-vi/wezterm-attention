#!/usr/bin/env node
// Application-owned checkpoint recipe. Attention owns identity and binding facts.
import { spawnSync } from "node:child_process";
import { openSync, closeSync, writeFileSync, renameSync, rmSync, readFileSync, readdirSync, lstatSync, linkSync } from "node:fs";
import { isAbsolute, resolve, dirname, basename, join } from "node:path";
import { pathToFileURL } from "node:url";
import { randomUUID } from "node:crypto";
import { performance } from "node:perf_hooks";

const KEPT = 20;
export const LIST = ["--skip-config", "cli", "--no-auto-start", "list", "--format", "json"];

// Resource budgets only; neither timeout nor missing output means no agent.
export function runCommand(executable, args, input, env) {
  if (!isAbsolute(executable)) throw new Error("executable must be absolute");
  return spawnSync(executable, args, { input, env, encoding: "utf8", timeout: 10000, maxBuffer: 8 * 1024 * 1024 });
}

export function runJson(executable, args, input, env, { acceptExit = [0], exec = runCommand } = {}) {
  const result = exec(executable, args, input, env);
  if (result.error || !acceptExit.includes(result.status)) throw new Error("command failed or was unavailable");
  try { return JSON.parse(result.stdout); } catch { throw new Error("command returned invalid JSON"); }
}

function validScope(scope) {
  return scope && typeof scope === "object" && !Array.isArray(scope)
    && Object.keys(scope).length === 2
    && /^[a-f0-9]{64}$/.test(scope.realm_id) && /^[a-f0-9]{64}$/.test(scope.incarnation_id);
}

export function paneNumber(value) {
  const text = typeof value === "number" && Number.isSafeInteger(value) ? String(value) : value;
  if (typeof text !== "string" || !/^(0|[1-9][0-9]{0,19})$/.test(text)) throw new Error("pane number is not canonical");
  return text;
}

export const sessionKey = row => `${row.provider}:${row.provider_session_id}`;

// The scope of a parsed checkpoint, or null when it is not a schema 1 checkpoint.
export function checkpointScope(value) {
  return value?.schema === 1 && validScope(value.scope) && Array.isArray(value.panes) ? value.scope : null;
}

// A `bindings` envelope this recipe can read: an incomplete answer is allowed
// here, a truncated one is not.
export function bindingsEnvelope(response) {
  return response?.schema === 1 && response.command === "bindings" && ["ok", "findings"].includes(response.status)
    && typeof response.complete === "boolean" && Array.isArray(response.diagnostics)
    && Array.isArray(response.result?.rows) && response.result.truncated === false;
}

const inScope = (address, scope) => address && address.realm_id === scope.realm_id && address.incarnation_id === scope.incarnation_id;

// The panes a diagnostic is about, or null when it is about the answer as a
// whole. A conflict names its addresses; a diagnostic whose context holds a
// `pane_id` in this server's scope names that pane.
function diagnosticPanes(diagnostic, scope) {
  const context = diagnostic?.context;
  if (!context || typeof context !== "object") return null;
  if (diagnostic.code === "binding_conflict" && Array.isArray(context.addresses)) {
    return context.addresses.filter(address => inScope(address, scope)).map(address => paneNumber(address.pane_id));
  }
  if (typeof context.pane_id === "string" && inScope(context, scope)) return [paneNumber(context.pane_id)];
  return null;
}

// Why a checkpoint would not trust a pane's current row, most specific first;
// empty when it would.
function distrust(rows, flagged) {
  const [row] = rows;
  return [
    ...(rows.length > 1 ? ["duplicate_current"] : []),
    ...(flagged ? [flagged] : []),
    ...(row && row.binding_health !== "valid" ? [`binding_health_${row.binding_health}`] : []),
    ...(row && row.reader_confidence !== "confirmed" ? [`reader_confidence_${row.reader_confidence}`] : []),
    ...(row && row.pane_presence !== "present" ? [`pane_presence_${row.pane_presence}`] : []),
  ];
}
const CONFLICT_ONLY = new Set(["binding_conflict", "binding_health_conflicted"]);

// Reads one server's bindings. `rows` holds every current row of the server.
// `current` holds, by pane, the rows a checkpoint can trust; `uncertain`
// holds every other current row by pane with its reasons, plus panes a
// diagnostic names that have no current row; `conflicts` lists each
// `binding_conflict` with its panes. It throws on a failure about the answer
// as a whole or on a malformed row.
export function bindings(attention, socket, run = runJson, limit) {
  const args = ["bindings", "--socket", socket, "--json"];
  if (limit !== undefined) args.push("--limit", limit);
  const response = run(attention, args, undefined, undefined, { acceptExit: [0, 1] });
  if (!bindingsEnvelope(response) || !validScope(response.result.scope)) throw new Error("binding discovery is incomplete or unsupported");
  const scope = response.result.scope;
  const flagged = new Map();
  const conflicts = [];
  for (const diagnostic of response.diagnostics) {
    const panes = diagnosticPanes(diagnostic, scope);
    if (!panes) throw new Error("binding discovery is incomplete or unsupported");
    for (const pane of panes) flagged.set(pane, diagnostic.code);
    if (diagnostic.code === "binding_conflict") conflicts.push({ provider_session_id: diagnostic.context.provider_session_id, panes });
  }
  if (!response.complete && !response.diagnostics.length) throw new Error("binding discovery is incomplete or unsupported");
  const byPane = new Map();
  for (const row of response.result.rows) {
    if (!row || typeof row.current !== "boolean") throw new Error("binding selection is unknown");
    if (!row.current || !inScope(row.address, scope)) continue;
    if (typeof row.launch_id !== "string" || typeof row.binding_id !== "string") throw new Error("binding row is malformed");
    const pane = paneNumber(row.address.pane_id);
    if (!byPane.has(pane)) byPane.set(pane, []);
    byPane.get(pane).push(row);
  }
  const current = new Map();
  const uncertain = new Map();
  for (const pane of new Set([...byPane.keys(), ...flagged.keys()])) {
    const rows = byPane.get(pane) ?? [];
    const reasons = distrust(rows, flagged.get(pane));
    if (reasons.length) uncertain.set(pane, { reason: reasons[0], reasons, rows });
    else current.set(pane, rows[0]);
  }
  return { scope, complete: response.complete, rows: [...byPane.values()].flat(), current, uncertain, conflicts };
}

// The server's listed panes, checked: each row an object with a canonical,
// unique pane number.
export function listTopology(wezterm, socket, run = runJson) {
  const topology = run(wezterm, LIST, undefined, { WEZTERM_UNIX_SOCKET: socket });
  if (!Array.isArray(topology)) throw new Error("topology is unavailable");
  const ids = topology.map(pane => {
    if (!pane || typeof pane !== "object") throw new Error("invalid topology row");
    return paneNumber(pane.pane_id);
  });
  const listed = new Set(ids);
  if (listed.size !== ids.length) throw new Error("duplicate topology pane");
  return { topology, ids, listed };
}

// A conflict whose addresses name exactly one listed pane leaves that pane's
// row standing, when the conflict is all that is wrong with it: no other
// listed pane holds the session, and an unlisted pane cannot be restored.
function resolveByTopology(read, listed) {
  const resolved = new Map(read.current);
  for (const conflict of read.conflicts) {
    const panes = conflict.panes.filter(pane => listed.has(pane));
    if (panes.length !== 1) continue;
    const entry = read.uncertain.get(panes[0]);
    if (entry?.rows.length === 1 && entry.rows[0].provider_session_id === conflict.provider_session_id
        && entry.reasons.every(reason => CONFLICT_ONLY.has(reason))) resolved.set(panes[0], entry.rows[0]);
  }
  return resolved;
}

const sameBinding = (a, b) => ["launch_id", "binding_id", "provider", "provider_session_id"].every(field => a[field] === b[field]);

export function captureCheckpoint({ attention, socket, wezterm, run = runJson, limit }) {
  if (!isAbsolute(socket)) throw new Error("socket must be absolute");
  const started = performance.now();
  const before = bindings(attention, socket, run, limit);
  const firstMs = performance.now() - started;
  const { topology, ids, listed } = listTopology(wezterm, socket, run);
  const secondStarted = performance.now();
  const after = bindings(attention, socket, run, limit);
  const secondMs = performance.now() - secondStarted;
  if (before.scope.realm_id !== after.scope.realm_id || before.scope.incarnation_id !== after.scope.incarnation_id) throw new Error("socket identity changed across topology capture");
  const first = resolveByTopology(before, listed);
  const second = resolveByTopology(after, listed);
  const panes = topology.map((pane, index) => {
    const id = ids[index];
    const entry = { topology: pane, binding: null };
    const a = first.get(id), b = second.get(id);
    if (a && b && sameBinding(a, b)) {
      entry.binding = b;
      if (!after.current.has(id)) entry.binding_note = "conflict_resolved_by_topology";
    } else if (a || b) {
      entry.binding_unknown_reason = "changed_during_capture";
    } else {
      entry.binding_unknown_reason = after.uncertain.get(id)?.reason ?? before.uncertain.get(id)?.reason ?? "no_current_binding";
    }
    return entry;
  });
  // Active sessions seen only in panes the mux no longer lists were gone
  // before this capture, though no session end reached Attention; a restore
  // must not bring them back as sessions the checkpoint missed.
  const unlisted = new Map();
  const seenListed = new Set();
  for (const row of [...before.rows, ...after.rows]) {
    if (listed.has(paneNumber(row.address.pane_id))) seenListed.add(sessionKey(row));
    else if (row.binding_phase === "active") unlisted.set(sessionKey(row), { provider: row.provider, provider_session_id: row.provider_session_id });
  }
  for (const key of seenListed) unlisted.delete(key);
  // A null binding is unknown, not proof of no agent, and an ended binding stays
  // ended. The snapshot itself grants no permission to resume or control a pane.
  return { schema: 1, scope: after.scope, panes, unlisted_sessions: [...unlisted.values()], binding_query_ms: [firstMs, secondMs] };
}

// The checkpoints of earlier servers kept beside `file`, newest first.
export function keptCheckpoints(file) {
  const prefix = `${basename(file)}.`;
  let entries;
  try { entries = readdirSync(dirname(file)); } catch { return []; }
  return entries
    .filter(entry => entry.startsWith(prefix) && entry.endsWith(".json") && /^[a-f0-9]{64}$/.test(entry.slice(prefix.length, -5)))
    .map(entry => join(dirname(file), entry))
    .flatMap(path => { try { return [{ path, mtime: lstatSync(path).mtimeMs }]; } catch { return []; } })
    .sort((a, b) => b.mtime - a.mtime).map(kept => kept.path);
}

// The first checkpoint after a restart would overwrite the only record of the
// server that stopped, so that file is first linked under its incarnation id;
// restore reads it there. A link, not a rename: until the new checkpoint is
// renamed over it, the old file stays at its path, so a failed save loses
// nothing.
function keepPrevious(file, incarnation) {
  let previous;
  try { previous = JSON.parse(readFileSync(file, "utf8")); } catch (error) {
    if (error.code === "ENOENT") return;
    throw new Error(`existing checkpoint ${file} is unreadable; it was left in place`);
  }
  const old = checkpointScope(previous)?.incarnation_id;
  if (!old) throw new Error(`existing checkpoint ${file} is not a schema 1 checkpoint; it was left in place`);
  if (old === incarnation) return;
  try { linkSync(file, `${file}.${old}.json`); } catch (error) { if (error.code !== "EEXIST") throw error; }
}

export function saveCheckpoint(options, file) {
  if (!isAbsolute(file)) throw new Error("checkpoint file must be absolute");
  const checkpoint = captureCheckpoint(options);
  keepPrevious(file, checkpoint.scope.incarnation_id);
  const temporary = `${file}.${randomUUID()}.tmp`;
  const descriptor = openSync(temporary, "wx", 0o600);
  try {
    try { writeFileSync(descriptor, JSON.stringify(checkpoint) + "\n"); } finally { closeSync(descriptor); }
    renameSync(temporary, file);
  }
  finally { rmSync(temporary, { force: true }); }
  for (const path of keptCheckpoints(file).slice(KEPT)) rmSync(path, { force: true });
  return checkpoint;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [attention, socket, wezterm, file, limit] = process.argv.slice(2);
  try {
    if (!attention || !socket || !wezterm || !file) throw new Error("usage: checkpoint.mjs ATTENTION SOCKET WEZTERM CHECKPOINT [LIMIT]");
    const result = saveCheckpoint({ attention, socket, wezterm, limit }, file);
    const unknown = result.panes.filter(pane => !pane.binding).length;
    console.log(JSON.stringify({ updated: true, panes: result.panes.length, binding_unknown: unknown, binding_query_ms: result.binding_query_ms }));
  } catch (error) { console.error(error instanceof Error ? error.message : "checkpoint failed"); process.exitCode = 1; }
}
