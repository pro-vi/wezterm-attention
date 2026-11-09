import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { hostname } from "node:os";
import { planRestore, applyRestore, readTarget, resumeLine } from "../../examples/restore.mjs";

// The Rust test supplies real `bindings` envelopes; every variant below starts
// from a real row, so the field names are the CLI's own.
const fixture = JSON.parse(readFileSync(process.argv[2], "utf8"));
const real = fixture.bindings.result.rows.find(row => row.current);
const source = { realm_id: real.address.realm_id, incarnation_id: "a".repeat(64) };
const targetScope = { realm_id: "b".repeat(64), incarnation_id: "c".repeat(64) };

function row(provider, session, patch = {}) {
  const transcript = provider === "claude" ? `/home/u/.claude/projects/-p/${session}.jsonl`
    : `/home/u/.codex/sessions/2026/11/09/rollout-2026-11-09T01-02-03-${session}.jsonl`;
  return { ...structuredClone(real), provider, provider_session_id: session, transcript_path: transcript,
    cwd: `/work/${session}`, binding_phase: "active", ...patch };
}
const pane = (id, window, tab, rect, binding, extra = {}) => ({
  topology: { pane_id: id, window_id: window, tab_id: tab, left_col: rect[0], top_row: rect[1],
    size: { cols: rect[2], rows: rect[3] }, cwd: `file:///shell/${id}`, tab_title: "", is_active: false, is_zoomed: false, ...extra },
  binding, ...(binding ? {} : { binding_unknown_reason: "no_current_binding" }),
});
const checkpointOf = panes => ({ schema: 1, scope: source, panes });
const target = (live = new Map(), realm = []) => ({ scope: targetScope, live, realm });

// The resume line, from the row's own config_dir.
assert.equal(resumeLine(row("claude", "s1", { config_dir: "/home/u/.claude2" })).line, "env CLAUDE_CONFIG_DIR=/home/u/.claude2 claude --resume s1");
assert.equal(resumeLine(row("claude", "s1")).line, "env -u CLAUDE_CONFIG_DIR claude --resume s1");
assert.equal(resumeLine(row("codex", "01a1-c")).line, "env -u CODEX_HOME codex resume 01a1-c");
assert.equal(resumeLine(row("codex", "01a1-c", { config_dir: "/home/u/.codex2" })).line,
  "env CODEX_HOME=/home/u/.codex2 codex resume 01a1-c", "a transcript under ~/.codex does not override config_dir");
assert.equal(resumeLine(row("claude", "s1", { config_dir: "/a b/it's" })).line, `env CLAUDE_CONFIG_DIR='/a b/it'\\''s' claude --resume s1`);
for (const [binding, reason] of [
  [row("pi", "p1"), "no_resume_form_for_pi"],
  [row("claude", "s1\n"), "session_id_invalid"],
  [row("claude", "s1", { transcript_path: "/home/u/.claude/projects/-p/other.jsonl" }), "transcript_does_not_name_session"],
  [row("claude", "s1", { config_dir: "relative/dir" }), "config_dir_invalid"],
  [row("claude", "s1", { config_dir: "/bad\u001b[0m" }), "config_dir_invalid"],
  [row("claude", "--dangerously-skip-permissions"), "session_id_invalid"],
  [row("toString", "s1"), "no_resume_form_for_toString"],
  [row("codex", "bbbb", { transcript_path: "/home/u/.codex/sessions/2026/11/09/rollout-2026-11-09T01-02-03-0199-aaaa-bbbb.jsonl" }), "transcript_does_not_name_session"],
]) assert.equal(resumeLine(binding).reason, reason);

// A two-window checkpoint: a 60/40 left-right split with the right side cut
// top and bottom, and a second window of one pane.
const panes = [
  pane(10, 1, 5, [0, 0, 60, 40], row("claude", "left"), { is_active: true, tab_title: "work" }),
  pane(11, 1, 5, [61, 0, 40, 20], row("codex", "right-top", { config_dir: "/home/u/.codex2" })),
  pane(12, 1, 5, [61, 21, 40, 19], null),
  pane(20, 2, 9, [0, 0, 80, 24], row("claude", "alone")),
];
const plan = planRestore(checkpointOf(panes), target());
assert.deepEqual(plan.tabs.map(tab => tab.layout), ["exact", "exact"]);
assert.deepEqual(plan.tabs[0].steps, [
  { op: "spawn_window", window: 0, ref: "p0", cwd: "/work/left" },
  { op: "split", ref: "p1", from: "p0", direction: "right", percent: 40, cwd: "/work/right-top" },
  { op: "split", ref: "p2", from: "p1", direction: "bottom", percent: 48, cwd: "/shell/12" },
]);
assert.deepEqual(plan.tabs[1].steps, [{ op: "spawn_window", window: 1, ref: "p3", cwd: "/work/alone" }]);
assert.deepEqual(plan.tabs[0].panes.map(p => [p.checkpoint_pane_id, p.outcome]), [["10", "resume"], ["11", "resume"], ["12", "binding_unknown"]]);
assert.equal(plan.tabs[0].panes[1].line, "env CODEX_HOME=/home/u/.codex2 codex resume right-top");
assert.equal(plan.tabs[0].active, "p0");
assert.equal(plan.tabs[0].title, "work");
assert.deepEqual(plan.counts, { windows: 2, panes: 4, lines: 3 });

// A session live in a listed target pane, an ended agent, an unknown pane.
const live = new Map([["claude:left", "77"]]);
const outcomes = planRestore(checkpointOf([
  pane(10, 1, 5, [0, 0, 80, 24], row("claude", "left")),
  pane(11, 1, 6, [0, 0, 80, 24], row("claude", "ended", { binding_phase: "ended" })),
  pane(12, 1, 7, [0, 0, 80, 24], null),
]), target(live)).tabs.map(tab => tab.panes[0]);
assert.deepEqual(outcomes.map(p => [p.outcome, p.live_pane ?? p.reason ?? null, p.cwd, p.line ?? null]), [
  ["already_running", "77", "/shell/10", null], ["agent_ended", null, "/shell/11", null], ["binding_unknown", "no_current_binding", "/shell/12", null],
]);
assert.throws(() => planRestore(checkpointOf(panes), { ...target(), scope: source }), /of the running server/);

// A directory that no longer exists gets no line; a pane URL may name this host.
const gone = planRestore(checkpointOf([
  pane(10, 1, 5, [0, 0, 80, 24], row("claude", "left")),
  pane(11, 1, 6, [0, 0, 80, 24], null, { cwd: `file://${hostname()}/shell/here` }),
  pane(12, 1, 7, [0, 0, 80, 24], null, { cwd: "file://elsewhere/shell/there" }),
]), { ...target(), exists: path => path !== "/work/left" }).tabs.map(tab => tab.panes[0]);
assert.deepEqual(gone.map(p => [p.outcome, p.reason ?? null, p.cwd, p.line ?? null]), [
  ["no_resume_line", "cwd_missing", "/shell/10", null], ["binding_unknown", "no_current_binding", "/shell/here", null], ["binding_unknown", "no_current_binding", null, null],
]);

// Layout notes: a zoomed tab's real size comes from its neighbours; a share
// past 10–90 is clamped; rectangles with no clean cut fall back to a chain.
const zoomed = planRestore(checkpointOf([
  pane(1, 1, 1, [0, 0, 87, 20], null), pane(2, 1, 1, [0, 21, 173, 42], null, { is_zoomed: true }),
  pane(3, 1, 1, [88, 0, 85, 20], null), pane(4, 1, 1, [88, 21, 85, 21], null),
]), target()).tabs[0];
assert.equal(zoomed.layout, "zoom_derived");
assert.deepEqual(zoomed.steps.map(s => [s.direction ?? s.op, s.percent ?? null]), [["spawn_window", null], ["right", 49], ["bottom", 50], ["bottom", 50]]);
assert.equal(zoomed.zoomed, zoomed.panes.find(p => p.checkpoint_pane_id === "2").ref);
const clamped = planRestore(checkpointOf([pane(1, 1, 1, [0, 0, 197, 40], null), pane(2, 1, 1, [198, 0, 2, 40], null)]), target()).tabs[0];
assert.deepEqual([clamped.layout, clamped.steps[1].percent], ["clamped", 10]);
const chain = planRestore(checkpointOf([pane(1, 1, 1, [0, 0, 50, 30], null), pane(2, 1, 1, [40, 10, 50, 30], null)]), target()).tabs[0];
assert.deepEqual([chain.layout, chain.steps[1].direction, chain.steps[1].percent], ["chain", "bottom", undefined]);

// Active sessions of the checkpointed server that the checkpoint lacks.
const old = { ...real.address, incarnation_id: source.incarnation_id };
const realmRow = (session, patch = {}) => ({ ...row("claude", session), address: old, current: true, ...patch });
const withAdded = planRestore(checkpointOf(panes), target(new Map([["claude:now-live", "78"]]), [
  realmRow("missed"),
  realmRow("missed-ended", { binding_phase: "ended" }),
  realmRow("other-server", { address: { ...old, incarnation_id: "d".repeat(64) } }),
  realmRow("left"),
  realmRow("now-live"),
  realmRow("twice"), realmRow("twice", { address: { ...old, pane_id: "99" } }),
]));
assert.deepEqual(withAdded.added.map(a => [a.session, a.outcome, a.reason ?? a.live_pane ?? null]),
  [["missed", "resume_added", null], ["now-live", "already_running", "78"], ["twice", "not_added", "conflicting_records"]]);
assert.deepEqual(withAdded.tabs.at(-1).steps, [{ op: "spawn_tab", window: 0, ref: "p4", cwd: "/work/missed" }]);
assert.equal(planRestore(checkpointOf(panes), target(new Map(), null)).added_unavailable, "realm_read_failed");
const endedThenResumed = checkpointOf([pane(10, 1, 5, [0, 0, 80, 24], row("claude", "again", { binding_phase: "ended" }))]);
assert.deepEqual(planRestore(endedThenResumed, target(new Map(), [realmRow("again")])).added.map(a => [a.session, a.outcome]), [["again", "resume_added"]],
  "a session ended in its checkpoint pane and resumed in a new pane is added");
const goneBefore = { ...checkpointOf(panes), unlisted_sessions: [{ provider: "claude", provider_session_id: "gone-before" }] };
assert.deepEqual(planRestore(goneBefore, target(new Map(), [realmRow("gone-before"), realmRow("missed")])).added.map(a => a.session), ["missed"],
  "a session whose pane was already gone at capture is not added back");
const onlyAdded = planRestore(checkpointOf([]), target(new Map(), [realmRow("missed")]));
assert.deepEqual([onlyAdded.tabs[0].steps[0].op, onlyAdded.counts.windows], ["spawn_window", 1]);

// readTarget reads, and only reads: the target's bindings and list, then the
// checkpointed realm. A target row counts as live only in a listed pane.
const calls = [];
const targetBindings = structuredClone(fixture.bindings);
targetBindings.result.rows = [
  { ...row("claude", "listed"), address: { ...targetBindings.result.scope, pane_id: "5" }, current: true },
  { ...row("claude", "unlisted"), address: { ...targetBindings.result.scope, pane_id: "6" }, current: true },
];
const read = readTarget({ attention: "/attention", socket: "/target.sock", wezterm: "/wezterm", checkpoint: checkpointOf(panes),
  run: (_exe, args, _input, env) => {
    calls.push(args.join(" "));
    if (args[0] === "bindings" && args[1] === "--socket") return targetBindings;
    if (args[0] === "bindings") return fixture.realm;
    assert.equal(env.WEZTERM_UNIX_SOCKET, "/target.sock");
    return [{ pane_id: 5, window_id: 0, tab_id: 0 }];
  } });
assert.deepEqual(calls, ["bindings --socket /target.sock --json", "--skip-config cli --no-auto-start list --format json", `bindings --realm ${source.realm_id} --json`]);
assert.deepEqual([...read.live], [["claude:listed", "5"]]);
const sameServerCalls = [];
assert.throws(() => readTarget({ attention: "/attention", socket: "/target.sock", wezterm: "/wezterm",
  checkpoint: { ...checkpointOf(panes), scope: targetBindings.result.scope },
  run: (_exe, args) => { sameServerCalls.push(args[0]); return targetBindings; } }), /of the running server/);
assert.deepEqual(sameServerCalls, ["bindings"], "a checkpoint of the running server is refused before the larger reads");
assert.ok(Array.isArray(read.realm));

// applyRestore against a recording WezTerm that numbers panes from 100.
function fakeWezterm({ fail = () => null, list = all => all } = {}) {
  const panes = [], log = [];
  let nextPane = 100, nextWindow = 0, nextTab = 0;
  const exec = (_exe, args) => {
    const cli = args.slice(3);
    if (cli[0] === "list") return { status: 0, stdout: JSON.stringify(list(panes)), stderr: "" };
    log.push(cli);
    const forced = fail(cli, log);
    if (forced) return forced;
    const value = (flag) => cli[cli.indexOf(flag) + 1];
    if (cli[0] === "spawn") {
      const window = cli.includes("--new-window") ? nextWindow++ : Number(value("--window-id"));
      panes.push({ pane_id: nextPane, window_id: window, tab_id: nextTab++ });
      return { status: 0, stdout: `${nextPane++}\n`, stderr: "" };
    }
    if (cli[0] === "split-pane") {
      const parent = panes.find(p => String(p.pane_id) === value("--pane-id"));
      panes.push({ pane_id: nextPane, window_id: parent.window_id, tab_id: parent.tab_id });
      return { status: 0, stdout: `${nextPane++}\n`, stderr: "" };
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  return { exec, log };
}
const steady = () => "server";
const applyWith = (wez, report = () => {}, { plan: use = plan, ...extra } = {}) => applyRestore(use, { wezterm: "/w", socket: "/s", exec: wez.exec, identity: steady, report, ...extra });
const events = [];
let wez = fakeWezterm();
const done = applyWith(wez, e => events.push(e));
assert.deepEqual(done, { created: 4, staged: 3, failed: 0, complete: true });
const created = new Set(events.filter(e => e.event === "created").map(e => e.pane_id));
const touched = wez.log.filter(cli => ["send-text", "set-tab-title", "activate-pane", "zoom-pane"].includes(cli[0])).map(cli => cli[cli.indexOf("--pane-id") + 1]);
assert.ok(touched.length && touched.every(id => created.has(id)), "only panes this run created are touched");
assert.deepEqual(wez.log.find(cli => cli[0] === "split-pane"), ["split-pane", "--pane-id", "100", "--right", "--percent", "40", "--cwd", "/work/right-top"]);
assert.deepEqual(wez.log.find(cli => cli[0] === "send-text"), ["send-text", "--pane-id", "100", "--no-paste", "--", "env -u CLAUDE_CONFIG_DIR claude --resume left"]);
assert.ok(wez.log.every(cli => !cli.some(arg => /\n|\r/.test(arg))), "nothing typed carries Enter");

// No space for a split: retried once without --percent.
wez = fakeWezterm({ fail: (cli, log) => cli[0] === "split-pane" && cli.includes("--percent") && log.filter(c => c[0] === "split-pane").length === 1
  ? { status: 1, stdout: "", stderr: "No space for split!" } : null });
const retried = [];
assert.equal(applyWith(wez, e => retried.push(e)).complete, true);
assert.deepEqual(retried.filter(e => e.event === "split_retried"), [{ event: "split_retried", ref: "p1", hidden_pane_possible: true }]);
assert.deepEqual(wez.log.filter(cli => cli[0] === "split-pane")[1], ["split-pane", "--pane-id", "100", "--right", "--cwd", "/work/right-top"]);

// A split that fails for another reason stops the run; a timeout is uncertain.
for (const [failure, reason] of [[{ status: 1, stdout: "", stderr: "pane not found" }, "failed"], [{ error: { code: "ETIMEDOUT" } }, "uncertain"]]) {
  const stops = [];
  wez = fakeWezterm({ fail: cli => (cli[0] === "split-pane" && cli[3] === "--bottom" ? failure : null) });
  const stopped = applyWith(wez, e => stops.push(e));
  assert.equal(stopped.complete, false);
  assert.equal(stopped.stopped.reason, reason);
  assert.deepEqual(stopped.stopped.opened, { p0: "100", p1: "101" });
  assert.equal(wez.log.at(-1)[0], "split-pane", "nothing runs after the failed step");
  assert.equal(wez.log.filter(cli => cli[0] === "split-pane" && cli[3] === "--bottom").length, 1, "only a no-space failure is retried");
  assert.equal(stops.filter(e => e.event === "staged").length, 0);
}

// A typing failure is reported and the rest are still typed; a failed title
// is reported and the run goes on; a tab with the wrong pane count stops.
wez = fakeWezterm({ fail: cli => (cli[0] === "send-text" && cli[2] === "101") || cli[0] === "set-tab-title" ? { status: 1, stdout: "", stderr: "x" } : null });
const partial = [];
const typed = applyWith(wez, e => partial.push(e));
assert.deepEqual([typed.staged, typed.failed, typed.complete], [2, 1, true]);
assert.ok(partial.some(e => e.event === "type_failed" && e.pane_id === "101"));
assert.ok(partial.some(e => e.event === "presentation_failed" && e.what === "title"));
wez = fakeWezterm({ list: all => all.slice(0, 1) });
assert.equal(applyWith(wez).stopped.reason, "pane_count_differs");

// A send-text that times out may have typed: it is reported as type_uncertain,
// not type_failed, and still counts in failed.
wez = fakeWezterm({ fail: cli => (cli[0] === "send-text" && cli[2] === "100" ? { error: { code: "ETIMEDOUT" } } : null) });
const timed = [];
assert.equal(applyWith(wez, e => timed.push(e)).failed, 1);
assert.ok(timed.some(e => e.event === "type_uncertain" && e.pane_id === "100"));

// A line that would press Enter is never sent: a caller's resumeLine that
// makes one gets no line in the plan, and a plan carrying one anyway has it
// refused at apply.
const one = checkpointOf([pane(10, 1, 5, [0, 0, 80, 24], row("claude", "left"))]);
const launcher = planRestore(one, target(), { resumeLine: binding => ({ line: `my-launcher ${binding.provider_session_id}\n`, profile: null }) });
assert.deepEqual([launcher.tabs[0].panes[0].outcome, launcher.tabs[0].panes[0].reason], ["no_resume_line", "line_has_control_character"]);
const edited = planRestore(one, target());
edited.tabs[0].panes[0].line += "\n";
wez = fakeWezterm();
const refused = [];
assert.equal(applyWith(wez, e => refused.push(e), { plan: edited }).failed, 1);
assert.ok(refused.some(e => e.event === "line_refused"));
assert.equal(wez.log.filter(cli => cli[0] === "send-text").length, 0);

// A server that restarts on the socket path mid-run stops the run before the
// next command, whose pane and window numbers could be someone else's: here,
// after the first tab's three panes open and before anything is typed.
wez = fakeWezterm();
const restarted = applyWith(wez, () => {}, { identity: () => (wez.log.length >= 3 ? "second" : "first") });
assert.equal(restarted.stopped.reason, "target_changed");
assert.deepEqual(Object.keys(restarted.stopped.opened), ["p0", "p1", "p2"]);
assert.equal(wez.log.filter(cli => cli[0] === "send-text").length, 0);

console.log("Restore recipe: resume lines, a two-window plan, outcomes, missing directories, layouts, added sessions, a read-only target read, and apply with retries, stops, typing failures, refused lines and a changed server passed");
