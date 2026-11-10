#!/usr/bin/env node
// Application-owned restore recipe. It reopens a checkpoint's panes in new
// windows and types each agent's resume line, never pressing Enter.
// Attention's CLI and plugin never spawn a pane or send input to one; this
// script does, and only when run with --apply.
import { readFileSync, statSync } from "node:fs";
import { hostname } from "node:os";
import { isAbsolute, resolve, basename } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";
import { LIST, bindings, bindingsEnvelope, checkpointScope, keptCheckpoints, listTopology, paneNumber, runCommand, runJson, sessionKey } from "./checkpoint.mjs";

const CLI = LIST.slice(0, 3);
// A leading "-" would be read as an option of the agent command.
const SESSION_ID = /^[A-Za-z0-9._:][A-Za-z0-9._:-]{0,127}$/;
// Every Unicode control character (C0, DEL and C1), as Attention's own check.
// WezTerm's parser runs a UTF-8 C1 control as a control (upstream vtparse at
// 4fbd6b8e9, test `utf8_control`), so a C1 character can start an escape
// sequence as ESC does.
const CONTROL = /\p{Cc}/u;
// Resume forms as of Claude Code 2.1.319 and Codex 0.166.0. Codex resumes with
// --no-daemon, as docs/setup.md ("Codex hooks") asks of every Codex start.
const PROVIDERS = {
  claude: { variable: "CLAUDE_CONFIG_DIR", resume: id => `claude --resume ${id}` },
  codex: { variable: "CODEX_HOME", resume: id => `codex --no-daemon resume ${id}` },
};

function shellWord(text) {
  return /^[A-Za-z0-9_/.,:+@%-]+$/.test(text) ? text : `'${text.replaceAll("'", `'\\''`)}'`;
}

// A path from the checkpoint reaches `--cwd` or a typed line only through
// here: a control character in it could press Enter or start an escape
// sequence. Session ids are checked by SESSION_ID.
const safePath = value => (typeof value === "string" && isAbsolute(value) && !CONTROL.test(value) ? value : null);

// The command typed into a reopened pane, or the reason there is none. The
// profile is the agent's own CLAUDE_CONFIG_DIR or CODEX_HOME as its hook saw
// it (`config_dir`); without one the variable is unset, never guessed.
export function resumeLine(binding) {
  const provider = Object.hasOwn(PROVIDERS, binding.provider) ? PROVIDERS[binding.provider] : null;
  if (!provider) return { reason: `no_resume_form_for_${binding.provider}` };
  const id = binding.provider_session_id;
  if (typeof id !== "string" || !SESSION_ID.test(id)) return { reason: "session_id_invalid" };
  const transcript = typeof binding.transcript_path === "string" ? basename(binding.transcript_path) : "";
  const named = binding.provider === "claude" ? transcript === `${id}.jsonl`
    : /^rollout-\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-(.+)\.jsonl$/.exec(transcript)?.[1] === id;
  if (!named) return { reason: "transcript_does_not_name_session" };
  const dir = binding.config_dir ?? null;
  if (dir !== null && !safePath(dir)) return { reason: "config_dir_invalid" };
  const profile = dir ? `env ${provider.variable}=${shellWord(dir)}` : `env -u ${provider.variable}`;
  return { line: `${profile} ${provider.resume(id)}`, profile: dir };
}

// A listed pane's directory from WezTerm's file URL, which may name this host.
// Node's URL parser (24.14.0) lower-cases a host, so this host's name is parsed
// the same way.
function topologyCwd(text) {
  try {
    const url = new URL(text);
    if (url.hostname && url.hostname !== new URL(`file://${hostname()}/`).hostname) return null;
    url.hostname = "";
    return safePath(fileURLToPath(url));
  } catch { return null; }
}

const isDirectory = path => { try { return statSync(path).isDirectory(); } catch { return false; } };
// A target with no `exists` (a caller's own) has every directory.
const dirExists = (target, path) => (target.exists ? target.exists(path) : true);

function readCheckpoint(file) {
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

// The target server's identity, the sessions its listed panes run, every row
// of the checkpointed realm (from which the plan picks the active sessions the
// checkpoint lacks), and which directories exist.
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
  return { scope: read.scope, live, realm, exists: isDirectory };
}

function rectangle(topology) {
  const r = { x: topology.left_col, y: topology.top_row, w: topology.size?.cols, h: topology.size?.rows };
  return Object.values(r).every(value => Number.isSafeInteger(value) && value >= 0) && r.w > 0 && r.h > 0 ? r : null;
}

// WezTerm resizes a zoomed pane to the whole tab (`set_zoomed` in upstream
// mux/src/tab.rs at 4fbd6b8e9), so a mux server running WezTerm
// 20261018-200420-656e4f47 lists it at its own place with the tab's size; its
// real edge is where its nearest neighbour begins.
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
// in the target, no line for it, or its line and directory. The directory is
// the binding's own, else `fallback`, which the caller has found to exist. A
// line is typed only into a recorded directory that exists here: anywhere
// else the agent would start away from where it ran.
function resumable(binding, target, resume, fallback) {
  const evidence = { provider: binding.provider, session: binding.provider_session_id };
  if (target.live.has(sessionKey(binding))) return { ...evidence, outcome: "already_running", live_pane: target.live.get(sessionKey(binding)) };
  const made = resume(binding);
  if (!made.line) return { ...evidence, outcome: "no_resume_line", reason: made.reason };
  // A caller's resumeLine is held to the rule the default keeps: no control
  // character, which could press Enter or start an escape sequence.
  if (CONTROL.test(made.line)) return { ...evidence, outcome: "no_resume_line", reason: "line_has_control_character" };
  const own = safePath(binding.cwd);
  if (own && !dirExists(target, own)) return { ...evidence, outcome: "no_resume_line", reason: "cwd_missing" };
  const cwd = own ?? fallback;
  if (!cwd) return { ...evidence, outcome: "no_resume_line", reason: "cwd_unknown" };
  return { ...evidence, outcome: "resume", line: made.line, profile: made.profile ?? null, cwd };
}

// What a checkpoint pane becomes.
function paneOutcome(pane, target, resume) {
  const recorded = topologyCwd(pane.topology.cwd);
  const shellCwd = recorded && dirExists(target, recorded) ? recorded : null;
  const binding = pane.binding;
  if (!binding) return { cwd: shellCwd, outcome: "binding_unknown", reason: pane.binding_unknown_reason ?? null };
  const note = pane.binding_note ? { binding_note: pane.binding_note } : {};
  if (binding.binding_phase !== "active") return { cwd: shellCwd, provider: binding.provider, session: binding.provider_session_id, ...note, outcome: "agent_ended" };
  return { cwd: shellCwd, ...resumable(binding, target, resume, shellCwd), ...note };
}

// Turns a checkpoint and the target's state into tabs of steps. It runs
// nothing, and reads the filesystem only through `target.exists`.
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
        .map(pane => ({ pane, topology: pane.topology, r: rectangle(pane.topology), plan: paneOutcome(pane, target, resume) }))
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
    // A session ended in its checkpoint pane may have been resumed in a new
    // pane since, so only active ones count as known.
    const known = new Set(checkpoint.panes.filter(p => p.binding?.binding_phase === "active").map(p => sessionKey(p.binding)));
    const excluded = new Map((checkpoint.excluded_sessions ?? []).map(entry => [sessionKey(entry), entry.reason]));
    const groups = new Map();
    for (const row of target.realm) {
      if (!row?.current || row.binding_phase !== "active" || row.address?.incarnation_id !== checkpoint.scope.incarnation_id) continue;
      const key = sessionKey(row);
      if (known.has(key)) continue;
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(row);
    }
    for (const [key, rows] of groups) {
      const refused = excluded.has(key) ? excluded.get(key) : rows.length > 1 ? "conflicting_records" : null;
      if (refused !== null) { added.push({ provider: rows[0].provider, session: rows[0].provider_session_id, outcome: "not_added", reason: refused }); continue; }
      const result = resumable(rows[0], target, resume);
      if (result.outcome === "already_running") { added.push(result); continue; }
      if (result.outcome !== "resume") { added.push({ ...result, outcome: "not_added" }); continue; }
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
    source: checkpoint.scope, target: target.scope, tabs, added, added_unavailable: target.realm ? null : "realm_read_failed",
    counts: { windows: tabs.filter(tab => tab.steps[0].op === "spawn_window").length, panes: panes.length, lines: panes.filter(p => p.line).length },
  };
}

class Stop extends Error {
  constructor(reason, detail) { super(reason); this.detail = detail; }
}

// The socket file's identity. A different file at the path is taken as a
// different server, whose window and pane ids are not this run's.
const socketIdentity = path => { try { const s = statSync(path); return `${s.dev}:${s.ino}:${s.birthtimeMs}`; } catch { return null; } };

// Runs a plan. Every pane it touches after creating it is one it created in
// this call; it never closes, moves or resizes a pane.
export function applyRestore(plan, { wezterm, socket, exec = runCommand, identity = socketIdentity, report = event => console.log(JSON.stringify(event)) }) {
  const env = { WEZTERM_UNIX_SOCKET: socket };
  const panes = new Map();
  const windows = new Map();
  const totals = { created: 0, staged: 0, failed: 0 };
  const server = identity(socket);
  // Every command checks that the socket still belongs to the server the run
  // began with.
  const sameServer = () => { if (server === null || identity(socket) !== server) throw new Stop("target_changed"); };
  // Sessions the checkpoint lacks were not looked for; this run cannot add them.
  if (plan.added_unavailable) { totals.failed++; report({ event: "added_unavailable", reason: plan.added_unavailable }); }
  const call = args => { sameServer(); return exec(wezterm, [...CLI, ...args], undefined, env); };
  const listed = () => {
    sameServer();
    let list = null;
    try { list = runJson(wezterm, LIST, undefined, env, { exec }); } catch {}
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
      const tabRoot = panes.get(tab.steps[0].ref);
      // A new window's id, and a split tab's pane count, come from the list;
      // a tab opened by one spawn into a known window needs neither.
      if (tab.steps[0].op === "spawn_window" || tab.steps.length > 1) {
        const list = listed();
        const first = list.find(pane => String(pane.pane_id) === tabRoot);
        if (!first) throw new Stop("uncertain", { step: tab.steps[0].ref, why: "new_pane_not_listed" });
        if (tab.steps[0].op === "spawn_window") windows.set(tab.window, first.window_id);
        const count = list.filter(pane => pane.tab_id === first.tab_id).length;
        if (count !== tab.panes.length) throw new Stop("pane_count_differs", { expected: tab.panes.length, found: count });
      }
      for (const pane of tab.panes) {
        if (!pane.line) continue;
        const id = panes.get(pane.ref);
        const miss = event => { totals.failed++; report({ event, ref: pane.ref, pane_id: id }); };
        // Checked again here, for a plan that did not come from planRestore:
        // a control character in a line could press Enter or start an escape
        // sequence.
        if (CONTROL.test(pane.line)) { miss("line_refused"); continue; }
        const result = call(["send-text", "--pane-id", id, "--no-paste", "--", pane.line]);
        if (result.error) miss("type_uncertain");
        else if (result.status !== 0) miss("type_failed");
        else { totals.staged++; report({ event: "staged", ref: pane.ref, pane_id: id }); }
      }
      if (tab.title) present("title", ["set-tab-title", "--pane-id", tabRoot, tab.title]);
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
      throw new Error(error.message + (kept.length ? `; earlier checkpoints, newest first: ${kept.join(", ")}` : ""));
    }
    if (!flags.includes("--apply")) console.log(JSON.stringify(plan));
    else {
      const result = applyRestore(plan, { wezterm, socket });
      if (!result.complete || result.failed) process.exitCode = 1;
    }
  } catch (error) { console.error(error instanceof Error ? error.message : "restore failed"); process.exitCode = 1; }
}
