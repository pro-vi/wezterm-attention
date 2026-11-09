#!/usr/bin/env node
// Application-owned restore recipe. It reopens a checkpoint's panes in new
// windows and types each agent's resume line, never pressing Enter.
// Attention's CLI and plugin never act on a pane; this script does, and only
// when run with --apply.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { isAbsolute, resolve, basename } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";
import { LIST, bindings, bindingsEnvelope, checkpointScope, keptCheckpoints, listTopology, paneNumber, runJson, sessionKey } from "./checkpoint.mjs";

const CLI = LIST.slice(0, 3);
const SESSION_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const CONTROL = /[\u0000-\u001f\u007f]/;
const PROFILE_VARIABLE = { claude: "CLAUDE_CONFIG_DIR", codex: "CODEX_HOME" };
const RESUME = { claude: id => `claude --resume ${id}`, codex: id => `codex resume ${id}` };

export function shellWord(text) {
  return /^[A-Za-z0-9_/.,:+@%-]+$/.test(text) ? text : `'${text.replaceAll("'", `'\\''`)}'`;
}

// Every record value that reaches `--cwd` or a typed line passes here.
const safePath = value => (typeof value === "string" && isAbsolute(value) && !CONTROL.test(value) ? value : null);

// The command typed into a reopened pane, or the reason there is none. The
// profile is the agent's own CLAUDE_CONFIG_DIR or CODEX_HOME as its hook saw
// it (`config_dir`); without one the variable is unset, never guessed.
export function resumeLine(binding) {
  const variable = PROFILE_VARIABLE[binding.provider];
  if (!variable) return { reason: `no_resume_form_for_${binding.provider}` };
  const id = binding.provider_session_id;
  if (typeof id !== "string" || !SESSION_ID.test(id)) return { reason: "session_id_invalid" };
  const transcript = typeof binding.transcript_path === "string" ? basename(binding.transcript_path) : "";
  const named = binding.provider === "claude" ? transcript === `${id}.jsonl` : transcript.endsWith(`-${id}.jsonl`);
  if (!named) return { reason: "transcript_does_not_name_session" };
  const dir = binding.config_dir ?? null;
  if (dir !== null && !safePath(dir)) return { reason: "config_dir_invalid" };
  const profile = dir ? `env ${variable}=${shellWord(dir)}` : `env -u ${variable}`;
  return { line: `${profile} ${RESUME[binding.provider](id)}`, profile: dir };
}

function topologyCwd(text) {
  try { return safePath(fileURLToPath(text)); } catch { return null; }
}

export function readCheckpoint(file) {
  let checkpoint;
  try { checkpoint = JSON.parse(readFileSync(file, "utf8")); } catch { throw new Error("checkpoint is unreadable"); }
  if (!checkpointScope(checkpoint)) throw new Error("checkpoint is not a schema 1 checkpoint");
  for (const pane of checkpoint.panes) {
    const t = pane?.topology;
    if (!t || !Number.isSafeInteger(t.window_id) || !Number.isSafeInteger(t.tab_id)) throw new Error("checkpoint pane has no window or tab");
    paneNumber(t.pane_id);
  }
  return checkpoint;
}

class SameServer extends Error {}

// The target server's identity, the sessions its listed panes run, and every
// row of the checkpointed realm, from which the plan picks the active
// sessions the checkpoint lacks.
export function readTarget({ attention, socket, wezterm, checkpoint, run = runJson }) {
  const read = bindings(attention, socket, run);
  if (read.scope.incarnation_id === checkpoint.scope.incarnation_id) throw new SameServer("checkpoint is of the running server");
  const { listed } = listTopology(wezterm, socket, run);
  const live = new Map();
  // A doubtful row still counts as running: skipping a resume is safer than
  // starting a second copy of a session.
  for (const row of read.rows) {
    if (row.binding_phase === "active" && listed.has(paneNumber(row.address.pane_id))) live.set(sessionKey(row), row.address.pane_id);
  }
  let realm = null;
  try {
    const response = run(attention, ["bindings", "--realm", checkpoint.scope.realm_id, "--json"]);
    if (bindingsEnvelope(response) && response.complete) realm = response.result.rows;
  } catch {}
  return { scope: read.scope, live, realm };
}

function rectangle(topology) {
  const r = { x: topology.left_col, y: topology.top_row, w: topology.size?.cols, h: topology.size?.rows };
  return Object.values(r).every(value => Number.isSafeInteger(value) && value >= 0) && r.w > 0 && r.h > 0 ? r : null;
}

// In a zoomed tab WezTerm lists the zoomed pane at its own place but with the
// whole tab's size; its real edge is where its nearest neighbour begins.
function unzoom(items, zoomed) {
  const others = items.filter(item => item !== zoomed);
  if (!zoomed.r || others.some(item => !item.r)) return false;
  const z = zoomed.r;
  const right = others.filter(o => o.r.x > z.x && o.r.y <= z.y && z.y < o.r.y + o.r.h).map(o => o.r.x - 1);
  const below = others.filter(o => o.r.y > z.y && o.r.x <= z.x && z.x < o.r.x + o.r.w).map(o => o.r.y - 1);
  zoomed.r = { x: z.x, y: z.y, w: (right.length ? Math.min(...right) : z.w) - z.x, h: (below.length ? Math.min(...below) : z.h) - z.y };
  return zoomed.r.w > 0 && zoomed.r.h > 0;
}

const span = (items, position, length) => Math.max(...items.map(i => i.r[position] + i.r[length])) - Math.min(...items.map(i => i.r[position]));

// The split tree of a tab's pane rectangles: a left/right cut first, then a
// top/bottom one, with one cell of slack for the divider.
function splitTree(items) {
  if (items.length === 1) return { leaf: items[0] };
  for (const [direction, position, length] of [["right", "x", "w"], ["bottom", "y", "h"]]) {
    const ends = [...new Set(items.map(i => i.r[position] + i.r[length]))].sort((a, b) => a - b);
    for (const end of ends) {
      const first = items.filter(i => i.r[position] + i.r[length] <= end + 1);
      const second = items.filter(i => !first.includes(i));
      if (!first.length || !second.length || second.some(i => i.r[position] < end - 1)) continue;
      const one = splitTree(first), two = splitTree(second);
      if (!one || !two) continue;
      const percent = Math.round((100 * span(second, position, length)) / span(items, position, length));
      return { direction, percent, first: one, second: two };
    }
  }
  return null;
}

const firstLeaf = node => node.leaf ?? firstLeaf(node.first);

// Whether a recorded session can be resumed into a new pane: running already
// in the target, no line for it, or its line and directory.
function resumable(binding, live, resume) {
  const evidence = { provider: binding.provider, session: binding.provider_session_id };
  if (live.has(sessionKey(binding))) return { ...evidence, outcome: "already_running", live_pane: live.get(sessionKey(binding)) };
  const made = resume(binding);
  if (!made.line) return { ...evidence, outcome: "no_resume_line", reason: made.reason };
  return { ...evidence, outcome: "resume", line: made.line, profile: made.profile ?? null, cwd: safePath(binding.cwd) };
}

// What a checkpoint pane becomes.
function paneOutcome(pane, live, resume) {
  const shellCwd = topologyCwd(pane.topology.cwd);
  const binding = pane.binding;
  if (!binding) return { cwd: shellCwd, outcome: "binding_unknown", reason: pane.binding_unknown_reason ?? null };
  const note = pane.binding_note ? { binding_note: pane.binding_note } : {};
  if (binding.binding_phase !== "active") return { cwd: shellCwd, provider: binding.provider, session: binding.provider_session_id, ...note, outcome: "agent_ended" };
  const result = resumable(binding, live, resume);
  return { ...result, ...note, cwd: result.outcome === "resume" ? result.cwd ?? shellCwd : shellCwd };
}

// Turns a checkpoint and the target's state into tabs of steps. Pure: it
// reads nothing and runs nothing.
export function planRestore(checkpoint, target, { resumeLine: resume = resumeLine } = {}) {
  if (target.scope.incarnation_id === checkpoint.scope.incarnation_id) throw new SameServer("checkpoint is of the running server");
  let next = 0;
  const ref = () => `p${next++}`;
  const byWindow = new Map();
  for (const pane of checkpoint.panes) {
    const { window_id: w, tab_id: t } = pane.topology;
    if (!byWindow.has(w)) byWindow.set(w, new Map());
    if (!byWindow.get(w).has(t)) byWindow.get(w).set(t, []);
    byWindow.get(w).get(t).push(pane);
  }
  const tabs = [];
  const windows = [...byWindow.keys()].sort((a, b) => a - b);
  for (const [windowIndex, windowId] of windows.entries()) {
    const tabIds = [...byWindow.get(windowId).keys()].sort((a, b) => a - b);
    for (const [tabIndex, tabId] of tabIds.entries()) {
      const items = byWindow.get(windowId).get(tabId)
        .map(pane => ({ pane, topology: pane.topology, r: rectangle(pane.topology), plan: paneOutcome(pane, target.live, resume) }))
        .sort((a, b) => (a.r?.y ?? 0) - (b.r?.y ?? 0) || (a.r?.x ?? 0) - (b.r?.x ?? 0));
      const zoomed = items.find(item => item.topology.is_zoomed);
      let layout = "exact";
      if (zoomed && items.length > 1) layout = unzoom(items, zoomed) ? "zoom_derived" : "chain";
      const tree = layout !== "chain" && items.every(item => item.r) ? splitTree(items) : null;
      if (!tree && items.length > 1) layout = "chain";
      const root = ref();
      const steps = [{ op: tabIndex === 0 ? "spawn_window" : "spawn_tab", window: windowIndex, ref: root, cwd: (tree ? firstLeaf(tree) : items[0]).plan.cwd }];
      if (tree) {
        const emit = (node, at) => {
          if (node.leaf) { node.leaf.ref = at; return; }
          const created = ref();
          let percent = node.percent;
          if (percent < 10 || percent > 90) { percent = Math.min(90, Math.max(10, percent)); layout = "clamped"; }
          steps.push({ op: "split", ref: created, from: at, direction: node.direction, percent, cwd: firstLeaf(node.second).plan.cwd });
          emit(node.first, at);
          emit(node.second, created);
        };
        emit(tree, root);
      } else {
        items[0].ref = root;
        for (let index = 1; index < items.length; index++) {
          items[index].ref = ref();
          const sameRow = items[index].r && items[index - 1].r && items[index].r.y === items[index - 1].r.y;
          steps.push({ op: "split", ref: items[index].ref, from: items[index - 1].ref, direction: sameRow ? "right" : "bottom", cwd: items[index].plan.cwd });
        }
      }
      tabs.push({
        window: windowIndex, layout, title: items[0].topology.tab_title || null, steps,
        active: items.find(item => item.topology.is_active)?.ref ?? null,
        zoomed: zoomed?.ref ?? null,
        panes: items.map(item => ({ ref: item.ref, checkpoint_pane_id: paneNumber(item.topology.pane_id), ...item.plan })),
      });
    }
  }
  // Active sessions of the checkpointed server that the checkpoint lacks,
  // each in a new tab of the first window.
  const added = [];
  if (target.realm) {
    const known = new Set([...checkpoint.panes.filter(p => p.binding).map(p => sessionKey(p.binding)), ...(checkpoint.unlisted_sessions ?? []).map(sessionKey)]);
    const groups = new Map();
    for (const row of target.realm) {
      if (!row?.current || row.binding_phase !== "active" || row.address?.incarnation_id !== checkpoint.scope.incarnation_id) continue;
      const key = sessionKey(row);
      if (known.has(key)) continue;
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(row);
    }
    for (const rows of groups.values()) {
      if (rows.length > 1) { added.push({ provider: rows[0].provider, session: rows[0].provider_session_id, outcome: "not_added", reason: "conflicting_records" }); continue; }
      const result = resumable(rows[0], target.live, resume);
      if (result.outcome === "already_running") { added.push(result); continue; }
      if (result.outcome !== "resume" || !result.cwd) { added.push({ ...result, outcome: "not_added", reason: result.reason ?? "cwd_unknown" }); continue; }
      const entry = { ...result, outcome: "resume_added" };
      const id = ref();
      added.push(entry);
      tabs.push({ window: 0, layout: "exact", title: null, active: id, zoomed: null,
        steps: [{ op: tabs.length ? "spawn_tab" : "spawn_window", window: 0, ref: id, cwd: entry.cwd }],
        panes: [{ ref: id, checkpoint_pane_id: null, ...entry }] });
    }
  }
  const panes = tabs.flatMap(tab => tab.panes);
  return {
    schema: 1, source: checkpoint.scope, target: target.scope, tabs, added, added_unavailable: target.realm ? null : "realm_read_failed",
    counts: { windows: tabs.filter(tab => tab.steps[0].op === "spawn_window").length, panes: panes.length, lines: panes.filter(p => p.line).length },
  };
}

function execText(executable, args, env) {
  if (!isAbsolute(executable)) throw new Error("executable must be absolute");
  return spawnSync(executable, args, { env, encoding: "utf8", timeout: 10000 });
}

class Stop extends Error {
  constructor(reason, detail) { super(reason); this.detail = detail; }
}

// Runs a plan. Every pane it touches after creating it is one it created in
// this call; it never closes, moves or resizes a pane.
export function applyRestore(plan, { wezterm, socket, exec = execText, run = runJson, report = event => console.log(JSON.stringify(event)) }) {
  const env = { WEZTERM_UNIX_SOCKET: socket };
  const panes = new Map();
  const windows = new Map();
  const totals = { created: 0, staged: 0, failed: 0 };
  const call = args => exec(wezterm, [...CLI, ...args], env);
  const listed = () => {
    const list = run(wezterm, LIST, undefined, env);
    if (!Array.isArray(list)) throw new Stop("target_topology_unavailable");
    return list;
  };
  // As of WezTerm 20260905-195314-b99b1ca2, a split refused with "No space
  // for split!" still leaves its new pane in the mux, in no tab: `cli list`
  // omits it and `get-text` reads it. Each such refusal is reported.
  const noSpace = result => result.status !== 0 && !result.error && /no space/i.test(result.stderr ?? "");
  const create = (step, args) => {
    let result = call(args);
    if (step.percent !== undefined && noSpace(result)) {
      report({ event: "split_retried", ref: step.ref, hidden_pane_possible: true });
      result = call(args.filter((arg, index) => arg !== "--percent" && args[index - 1] !== "--percent"));
    }
    if (result.error) throw new Stop("uncertain", { step: step.ref, why: result.error.code ?? "error" });
    if (result.status !== 0) {
      throw new Stop("failed", { step: step.ref, stderr: (result.stderr ?? "").trim().slice(0, 500), ...(noSpace(result) ? { hidden_pane_possible: true } : {}) });
    }
    const id = (result.stdout ?? "").trim();
    try { paneNumber(id); } catch { throw new Stop("uncertain", { step: step.ref, why: "no_pane_id" }); }
    panes.set(step.ref, id);
    totals.created++;
    report({ event: "created", ref: step.ref, pane_id: id });
  };
  const cwdArgs = cwd => (cwd ? ["--cwd", cwd] : []);
  // Presentation steps report a failure and go on.
  const present = (what, args) => { const result = call(args); if (result.error || result.status !== 0) report({ event: "presentation_failed", what }); };
  try {
    for (const tab of plan.tabs) {
      for (const step of tab.steps) {
        if (step.op === "spawn_window") create(step, ["spawn", "--new-window", ...cwdArgs(step.cwd)]);
        else if (step.op === "spawn_tab") {
          if (!windows.has(step.window)) throw new Stop("failed", { step: step.ref, why: "window_not_opened" });
          create(step, ["spawn", "--window-id", String(windows.get(step.window)), ...cwdArgs(step.cwd)]);
        } else {
          const percent = step.percent === undefined ? [] : ["--percent", String(step.percent)];
          create(step, ["split-pane", "--pane-id", panes.get(step.from), `--${step.direction}`, ...percent, ...cwdArgs(step.cwd)]);
        }
      }
      const opened = tab.panes.map(pane => panes.get(pane.ref));
      // A new window's id, and a split tab's pane count, come from the list;
      // a tab opened by one spawn into a known window needs neither.
      if (tab.steps[0].op === "spawn_window" || tab.steps.length > 1) {
        const list = listed();
        const first = list.find(pane => String(pane.pane_id) === opened[0]);
        if (!first) throw new Stop("uncertain", { step: tab.steps[0].ref, why: "new_pane_not_listed" });
        if (tab.steps[0].op === "spawn_window") windows.set(tab.window, first.window_id);
        const count = list.filter(pane => pane.tab_id === first.tab_id).length;
        if (count !== tab.panes.length) throw new Stop("pane_count_differs", { expected: tab.panes.length, found: count });
      }
      for (const pane of tab.panes) {
        if (!pane.line) continue;
        const id = panes.get(pane.ref);
        const result = call(["send-text", "--pane-id", id, "--no-paste", "--", pane.line]);
        if (result.error || result.status !== 0) { totals.failed++; report({ event: "type_failed", ref: pane.ref, pane_id: id }); }
        else { totals.staged++; report({ event: "staged", ref: pane.ref, pane_id: id }); }
      }
      if (tab.title) present("title", ["set-tab-title", "--pane-id", opened[0], tab.title]);
      if (tab.active && tab.panes.length > 1) present("activate", ["activate-pane", "--pane-id", panes.get(tab.active)]);
      if (tab.zoomed) present("zoom", ["zoom-pane", "--pane-id", panes.get(tab.zoomed), "--zoom"]);
    }
  } catch (error) {
    if (!(error instanceof Stop)) throw error;
    const stopped = { event: "stopped", reason: error.message, ...error.detail, opened: Object.fromEntries(panes) };
    report(stopped);
    report({ event: "done", ...totals, complete: false });
    return { ...totals, complete: false, stopped };
  }
  report({ event: "done", ...totals, complete: true });
  return { ...totals, complete: true };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [file, attention, socket, wezterm, ...flags] = process.argv.slice(2);
  try {
    if (!file || !attention || !socket || !wezterm || flags.some(flag => flag !== "--apply")) {
      throw new Error("usage: restore.mjs CHECKPOINT ATTENTION SOCKET WEZTERM [--apply]");
    }
    for (const path of [file, attention, socket, wezterm]) if (!isAbsolute(path)) throw new Error(`${path} must be an absolute path`);
    const checkpoint = readCheckpoint(file);
    let plan;
    try { plan = planRestore(checkpoint, readTarget({ attention, socket, wezterm, checkpoint })); } catch (error) {
      if (!(error instanceof SameServer)) throw error;
      const kept = keptCheckpoints(file);
      throw new Error(error.message + (kept.length ? `; earlier servers' checkpoints: ${kept.join(", ")}` : ""));
    }
    if (!flags.includes("--apply")) console.log(JSON.stringify(plan));
    else {
      const result = applyRestore(plan, { wezterm, socket });
      if (!result.complete || result.failed) process.exitCode = 1;
    }
  } catch (error) { console.error(error instanceof Error ? error.message : "restore failed"); process.exitCode = 1; }
}
