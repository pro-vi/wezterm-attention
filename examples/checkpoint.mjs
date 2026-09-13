#!/usr/bin/env node
// Application-owned checkpoint recipe. Attention owns identity and binding facts.
import { spawnSync } from "node:child_process";
import { openSync, closeSync, writeFileSync, renameSync, rmSync } from "node:fs";
import { isAbsolute, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { randomUUID } from "node:crypto";
import { performance } from "node:perf_hooks";

export function runJson(executable, args, input, env) {
  if (!isAbsolute(executable)) throw new Error("executable must be absolute");
  // Resource budgets only; neither timeout nor missing output means no agent.
  const result = spawnSync(executable, args, { input, env, encoding: "utf8", timeout: 10000, maxBuffer: 8 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error("command failed or was unavailable");
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

export function bindings(attention, socket, run = runJson, limit = "100") {
  const response = run(attention, ["bindings", "--socket", socket, "--limit", limit, "--json"]);
  if (!response || response.schema !== 1 || response.command !== "bindings" || response.status !== "ok"
      || response.complete !== true || !Array.isArray(response.diagnostics) || response.diagnostics.length
      || !response.result || !validScope(response.result.scope) || !Array.isArray(response.result.rows)
      || response.result.truncated !== false) throw new Error("binding discovery is incomplete or unsupported");
  const scope = response.result.scope;
  const current = new Map();
  for (const row of response.result.rows) {
    if (!row || typeof row.current !== "boolean") throw new Error("binding selection is unknown");
    if (!row.current) continue;
    if (row.binding_health !== "valid" || row.reader_confidence !== "confirmed" || row.pane_presence !== "present"
        || !row.address || row.address.realm_id !== scope.realm_id || row.address.incarnation_id !== scope.incarnation_id
        || typeof row.launch_id !== "string" || typeof row.binding_id !== "string") throw new Error("current association is uncertain");
    const pane = paneNumber(row.address.pane_id);
    if (current.has(pane)) throw new Error("duplicate current association");
    current.set(pane, row);
  }
  return { scope, current };
}

export function captureCheckpoint({ attention, socket, wezterm, run = runJson, limit = "100" }) {
  if (!isAbsolute(socket)) throw new Error("socket must be absolute");
  const started = performance.now();
  const before = bindings(attention, socket, run, limit);
  const firstMs = performance.now() - started;
  const topology = run(wezterm, ["--skip-config", "cli", "--no-auto-start", "list", "--format", "json"], undefined, { WEZTERM_UNIX_SOCKET: socket });
  if (!Array.isArray(topology)) throw new Error("topology is unavailable");
  const secondStarted = performance.now();
  const after = bindings(attention, socket, run, limit);
  const secondMs = performance.now() - secondStarted;
  if (before.scope.realm_id !== after.scope.realm_id || before.scope.incarnation_id !== after.scope.incarnation_id) throw new Error("socket identity changed across topology capture");
  const seen = new Set();
  const panes = topology.map(pane => {
    if (!pane || typeof pane !== "object") throw new Error("invalid topology row");
    const id = paneNumber(pane.pane_id);
    if (seen.has(id)) throw new Error("duplicate topology pane");
    seen.add(id);
    return { topology: pane, binding: after.current.get(id) ?? null };
  });
  // A null association is unknown, not proof of no agent. Ended bindings remain
  // explicitly ended; this snapshot supplies neither resume nor control permission.
  return { schema: 1, scope: after.scope, panes, binding_query_ms: [firstMs, secondMs] };
}

export function saveCheckpoint(options, file) {
  if (!isAbsolute(file)) throw new Error("checkpoint file must be absolute");
  const checkpoint = captureCheckpoint(options);
  const temporary = `${file}.${randomUUID()}.tmp`;
  const descriptor = openSync(temporary, "wx", 0o600);
  try {
    try { writeFileSync(descriptor, JSON.stringify(checkpoint) + "\n"); } finally { closeSync(descriptor); }
    renameSync(temporary, file);
  }
  finally { rmSync(temporary, { force: true }); }
  return checkpoint;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [attention, socket, wezterm, file, limit] = process.argv.slice(2);
  try {
    if (!attention || !socket || !wezterm || !file) throw new Error("usage: checkpoint.mjs ATTENTION SOCKET WEZTERM CHECKPOINT [LIMIT]");
    const result = saveCheckpoint({ attention, socket, wezterm, limit }, file);
    console.log(JSON.stringify({ updated: true, binding_query_ms: result.binding_query_ms }));
  } catch (error) { console.error(error instanceof Error ? error.message : "checkpoint failed"); process.exitCode = 1; }
}
