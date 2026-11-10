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
const sessionRef = row => ({ provider: row.provider, provider_session_id: row.provider_session_id });

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

// Why a checkpoint would not trust a pane's current row, most specific first;
// empty when it would.
function distrust(rows, conflicted) {
  const [row] = rows;
  return [
    ...(rows.length > 1 ? ["duplicate_current"] : []),
    ...(conflicted ? ["binding_conflict"] : []),
    ...(row.binding_health !== "valid" ? [`binding_health_${row.binding_health}`] : []),
    ...(row.reader_confidence !== "confirmed" ? [`reader_confidence_${row.reader_confidence}`] : []),
    ...(row.pane_presence !== "present" ? [`pane_presence_${row.pane_presence}`] : []),
  ];
}
const CONFLICT_ONLY = new Set(["binding_conflict", "binding_health_conflicted"]);

// Reads one server's bindings. `rows` holds every current row of the server.
// `current` holds, by pane, the rows a checkpoint can trust; `uncertain`
// holds every other current row by pane with its reasons. `unprobed` holds,
// by pane, the code of a diagnostic that names that pane alone, which
// Attention gives a pane it could not probe. `conflicts` lists each
// `binding_conflict` with its session, the panes of this server whose current
// row holds it, and whether it also names a pane under another server. It
// throws on a failure about the answer as a whole or on a malformed row.
export function bindings(attention, socket, run = runJson, limit) {
  const args = ["bindings", "--socket", socket, "--json"];
  if (limit !== undefined) args.push("--limit", limit);
  const response = run(attention, args, undefined, undefined, { acceptExit: [0, 1] });
  if (!bindingsEnvelope(response) || !validScope(response.result.scope)) throw new Error("binding discovery is incomplete or unsupported");
  const scope = response.result.scope;
  const byPane = new Map();
  for (const row of response.result.rows) {
    if (!row || typeof row.current !== "boolean") throw new Error("binding selection is unknown");
    if (!row.current || !inScope(row.address, scope)) continue;
    if (typeof row.launch_id !== "string" || typeof row.binding_id !== "string") throw new Error("binding row is malformed");
    const pane = paneNumber(row.address.pane_id);
    if (!byPane.has(pane)) byPane.set(pane, []);
    byPane.get(pane).push(row);
  }
  const conflicted = new Set();
  const unprobed = new Map();
  const conflicts = [];
  for (const diagnostic of response.diagnostics) {
    const context = diagnostic?.context;
    if (diagnostic?.code === "binding_conflict" && Array.isArray(context?.addresses)) {
      // A conflict is about one session. A pane it names through a row that is
      // not the pane's current one, such as a binding a resume replaced, keeps
      // its current row.
      const key = sessionKey(context);
      const holders = context.addresses.filter(address => inScope(address, scope)).map(address => paneNumber(address.pane_id))
        .filter(pane => (byPane.get(pane) ?? []).some(row => sessionKey(row) === key));
      for (const pane of holders) conflicted.add(pane);
      conflicts.push({ session: sessionRef(context), holders,
        elsewhere: context.addresses.some(address => !inScope(address, scope)) });
    } else if (typeof context?.pane_id === "string" && inScope(context, scope)) {
      unprobed.set(paneNumber(context.pane_id), diagnostic.code);
    } else {
      const root = typeof context?.state_root === "string" ? ` for ${context.state_root}` : "";
      throw new Error(`binding discovery is incomplete: ${diagnostic?.code ?? "unknown"}${root}`);
    }
  }
  if (!response.complete && !response.diagnostics.length) throw new Error("binding discovery is incomplete or unsupported");
  const current = new Map();
  const uncertain = new Map();
  for (const [pane, rows] of byPane) {
    const reasons = distrust(rows, conflicted.has(pane));
    if (reasons.length) uncertain.set(pane, { reason: reasons[0], reasons, rows });
    else current.set(pane, rows[0]);
  }
  return { scope, complete: response.complete, rows: [...byPane.values()].flat(), current, uncertain, unprobed, conflicts };
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

// A conflict whose session is held by exactly one listed pane leaves that
// pane's row standing, when the conflict is all that is wrong with it: no
// other listed pane holds the session, and an unlisted pane of this server
// cannot be restored. A conflict that also names a pane under another server
// stays unresolved, since the session may run there.
function resolveByTopology(read, listed) {
  const resolved = new Map(read.current);
  for (const conflict of read.conflicts) {
    const panes = conflict.holders.filter(pane => listed.has(pane));
    if (conflict.elsewhere || panes.length !== 1) continue;
    const entry = read.uncertain.get(panes[0]);
    if (entry.rows.length === 1 && entry.reasons.every(reason => CONFLICT_ONLY.has(reason))) resolved.set(panes[0], entry.rows[0]);
  }
  return resolved;
}

// Attention probes the panes the mux has just listed through the same
// socket. A listed pane it could not probe, or reads as not present, means
// the probe failed, the server stopped, or the pane closed after the list.
// The answer cannot tell these apart, so the capture fails and the previous
// checkpoint stays; a pane that closes after the list costs that minute's save.
function probeFailure(read, listed) {
  for (const [pane, code] of read.unprobed) if (listed.has(pane)) return `${code} for pane ${pane}`;
  const row = read.rows.find(row => inList(listed, row) && row.pane_presence !== "present");
  return row ? `pane ${row.address.pane_id} reads ${row.pane_presence}` : null;
}

const inList = (listed, row) => listed.has(paneNumber(row.address.pane_id));
const active = rows => rows.filter(row => row.binding_phase === "active");

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
  const failure = probeFailure(before, listed) ?? probeFailure(after, listed);
  if (failure) throw new Error(`a pane the mux lists was not confirmed present (${failure})`);
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
  // Sessions a restore must not add back as ones the checkpoint missed, each
  // with its reason. An active session whose active rows, read before the
  // pane list, are only in panes the list omits was gone before this capture,
  // though no session end reached Attention; a row first read after the list
  // may be in a pane opened since, and an ended row in a listed pane holds no
  // session. A session whose conflict also names a pane under another server
  // may run there, and a restore reads only this server's realm.
  const held = new Set(active([...before.rows, ...after.rows]).filter(row => inList(listed, row)).map(sessionKey));
  const excluded = new Map(active(before.rows).filter(row => !held.has(sessionKey(row)))
    .map(row => [sessionKey(row), { ...sessionRef(row), reason: "gone_before_capture" }]));
  for (const conflict of [...before.conflicts, ...after.conflicts]) {
    if (conflict.elsewhere) excluded.set(sessionKey(conflict.session), { ...conflict.session, reason: "bound_under_another_server" });
  }
  // A null binding is unknown, not proof of no agent, and an ended binding stays
  // ended. The snapshot itself grants no permission to resume or control a pane.
  return { schema: 1, scope: after.scope, panes, excluded_sessions: [...excluded.values()], binding_query_ms: [firstMs, secondMs] };
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
