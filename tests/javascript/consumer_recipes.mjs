import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, writeFileSync, rmSync, readdirSync, utimesSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import crypto from "node:crypto";
import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { captureCheckpoint, saveCheckpoint, runJson, keptCheckpoints } from "../../examples/checkpoint.mjs";
import { inspectBindings } from "../../examples/inspect.mjs";

// The Rust test supplies actual production CLI envelopes, not a second schema.
const fixture = JSON.parse(readFileSync(process.argv[2], "utf8"));
const directory = mkdtempSync(join(tmpdir(), "attention-consumer-recipes-"));
const file = join(directory, "checkpoint.json");
const realRow = () => structuredClone(fixture.bindings.result.rows.find(row => row.current));
const scope = fixture.bindings.result.scope;
const at = (row, pane) => { row.address.pane_id = pane; return row; };
// Runs `action` while every write to an open descriptor fails.
function withFailingWrite(action) {
  const original = fs.writeFileSync;
  fs.writeFileSync = (target, ...args) => {
    if (typeof target === "number") throw new Error("synthetic write failure");
    return original(target, ...args);
  };
  syncBuiltinESMExports();
  try { return action(); } finally { fs.writeFileSync = original; syncBuiltinESMExports(); }
}
try {
  function runner(change = value => value, topology = [{ pane_id: 42, tab_id: 7, is_active: true }]) {
    let queries = 0;
    return (_executable, args, input, env) => {
      if (args[0] === "bindings") return change(structuredClone(fixture.bindings), ++queries);
      if (args[0] === "inspect") {
        const row = realRow();
        assert.deepEqual(JSON.parse(input), { address: row.address, launch_id: row.launch_id, binding_id: row.binding_id });
        return change(structuredClone(fixture.inspect), 3);
      }
      assert.deepEqual(args, ["--skip-config", "cli", "--no-auto-start", "list", "--format", "json"]);
      assert.equal(env.WEZTERM_UNIX_SOCKET, "/synthetic.sock");
      return structuredClone(topology);
    };
  }
  const options = { attention: "/attention", socket: "/synthetic.sock", wezterm: "/wezterm" };
  const result = captureCheckpoint({ ...options, run: runner() });
  assert.equal(result.panes[0].topology.tab_id, 7);
  assert.deepEqual(result.panes[0].binding, realRow());
  assert.equal(result.panes[0].binding_unknown_reason, undefined);
  const previous = JSON.stringify(result) + "\n";

  // `bindings` exits 1 on an incomplete answer; runJson accepts that exit only
  // when acceptExit lists it.
  const exitOne = [process.execPath, ["-e", "console.log(JSON.stringify({ok:1})); process.exit(1)"]];
  assert.deepEqual(runJson(...exitOne, undefined, undefined, { acceptExit: [0, 1] }), { ok: 1 });
  assert.throws(() => runJson(...exitOne));

  // A failure about the whole answer keeps the previous checkpoint.
  for (const change of [
    (value, n) => { if (n === 2) value.result.scope.incarnation_id = "d".repeat(64); return value; },
    value => { value.complete = false; return value; },
    value => { value.result.truncated = true; value.complete = false; return value; },
    value => { value.complete = false; value.diagnostics.push({ code: "record_invalid", message: "unreadable", context: { path: "x.json" } }); return value; },
  ]) {
    writeFileSync(file, previous);
    assert.throws(() => saveCheckpoint({ ...options, run: runner(change) }, file));
    assert.equal(readFileSync(file, "utf8"), previous);
  }

  // A doubt about one pane leaves that pane unknown and saves the rest.
  for (const [change, reason] of [
    [value => { value.result.rows.push(realRow()); return value; }, "duplicate_current"],
    [value => { value.result.rows.find(row => row.current).reader_confidence = "unconfirmed"; return value; }, "reader_confidence_unconfirmed"],
    [(value, n) => { if (n === 2) value.result.rows.find(row => row.current).binding_id = "f".repeat(64); return value; }, "changed_during_capture"],
    [value => { value.result.rows = []; return value; }, "no_current_binding"],
  ]) {
    writeFileSync(file, previous);
    const saved = saveCheckpoint({ ...options, run: runner(change) }, file);
    assert.equal(saved.panes[0].binding, null, reason);
    assert.equal(saved.panes[0].binding_unknown_reason, reason);
    assert.deepEqual(JSON.parse(readFileSync(file, "utf8")).panes[0].binding_unknown_reason, reason);
  }

  // A long-running mux: current rows of closed panes, and a conflict between a
  // listed pane and an unlisted one, in an incomplete answer.
  const conflictOf = panes => ({
    code: "binding_conflict", message: "provider session is bound to multiple pane addresses",
    context: { provider: realRow().provider, provider_session_id: realRow().provider_session_id,
      addresses: panes.map(pane => ({ ...scope, pane_id: pane })) },
  });
  const live = (panes, conflicted) => value => {
    value.complete = false;
    value.status = "findings";
    const closed = at(realRow(), "43");
    Object.assign(closed, { pane_presence: "verified_absent", reader_confidence: "unconfirmed", provider_session_id: "closed-session" });
    value.result.rows.push(closed);
    for (const pane of panes.filter(pane => pane !== "42")) value.result.rows.push(at(realRow(), pane));
    for (const row of value.result.rows) if (row.current && conflicted.includes(row.address.pane_id)) row.binding_health = "conflicted";
    value.diagnostics.push(conflictOf(panes));
    return value;
  };
  writeFileSync(file, previous);
  const resolved = saveCheckpoint({ ...options, run: runner(live(["42", "99"], ["42", "99"])) }, file);
  assert.equal(resolved.panes[0].binding.provider_session_id, realRow().provider_session_id);
  assert.equal(resolved.panes[0].binding_note, "conflict_resolved_by_topology");
  assert.deepEqual(resolved.unlisted_sessions.map(entry => entry.provider_session_id), ["closed-session"],
    "a closed pane's active session is recorded as gone; a listed pane's session is not, though an unlisted twin holds it");
  const doubted = captureCheckpoint({ ...options, run: runner(value => {
    live(["42", "99"], ["42", "99"])(value);
    value.result.rows.find(row => row.current && row.address.pane_id === "42").reader_confidence = "unconfirmed";
    return value;
  }) });
  assert.equal(doubted.panes[0].binding, null, "the topology rule lifts only a conflict, not another doubt");
  const both = [{ pane_id: 42, tab_id: 7 }, { pane_id: 44, tab_id: 7 }];
  const unresolved = captureCheckpoint({ ...options, run: runner(live(["42", "44"], ["42", "44"]), both) });
  assert.deepEqual(unresolved.panes.map(pane => pane.binding_unknown_reason), ["binding_conflict", "binding_conflict"]);

  // A checkpoint of another server is kept under its incarnation id, and only
  // the newest twenty kept files remain; other names are never removed.
  const other = "e".repeat(64);
  const stranger = `${file}.not-a-checkpoint.json`;
  writeFileSync(stranger, "someone else's file");
  for (let index = 0; index < 20; index++) {
    const kept = `${file}.${index.toString(16).padStart(64, "0")}.json`;
    writeFileSync(kept, "{}");
    utimesSync(kept, 1000 + index, 1000 + index);
  }
  writeFileSync(file, JSON.stringify({ ...result, scope: { ...scope, incarnation_id: other } }) + "\n");
  saveCheckpoint({ ...options, run: runner() }, file);
  const kept = keptCheckpoints(file);
  assert.equal(kept.length, 20);
  assert.equal(kept[0], `${file}.${other}.json`, "the replaced checkpoint is the newest kept");
  assert.equal(existsSync(`${file}.${"0".repeat(64)}.json`), false, "the oldest kept file is removed");
  assert.equal(readFileSync(stranger, "utf8"), "someone else's file");
  // A save that fails after keeping an earlier server's checkpoint leaves
  // that checkpoint at its path too.
  const earlier = JSON.stringify({ ...result, scope: { ...scope, incarnation_id: "9".repeat(64) } }) + "\n";
  writeFileSync(file, earlier);
  withFailingWrite(() => assert.throws(() => saveCheckpoint({ ...options, run: runner() }, file), /synthetic write failure/));
  assert.equal(readFileSync(file, "utf8"), earlier);
  assert.equal(readFileSync(`${file}.${"9".repeat(64)}.json`, "utf8"), earlier);
  writeFileSync(file, "not a checkpoint");
  assert.throws(() => saveCheckpoint({ ...options, run: runner() }, file), /left in place/);
  assert.equal(readFileSync(file, "utf8"), "not a checkpoint");

  const facts = inspectBindings({ ...options, run: runner() });
  assert.deepEqual(facts.panes[0], fixture.inspect.result);
  assert.deepEqual(facts.uncertain, []);
  assert.throws(() => inspectBindings({ ...options, run: runner((value, n) => {
    if (n === 3) { value.complete = false; value.result.scope_relation = "binding_changed"; }
    return value;
  }) }));
  let inspected = false;
  assert.throws(() => inspectBindings({ ...options, run: (_exe, args) => {
    if (args[0] === "inspect") inspected = true;
    return { ...fixture.bindings, complete: false };
  } }));
  assert.equal(inspected, false, "an incomplete answer that no diagnostic explains stops before inspection");
  inspected = false;
  assert.throws(() => inspectBindings({ ...options, run: (_exe, args) => {
    if (args[0] === "inspect") inspected = true;
    return live(["42", "99"], ["42", "99"])(structuredClone(fixture.bindings));
  } }), /incomplete/);
  assert.equal(inspected, false, "inspection also stops on an incomplete answer a checkpoint can read");
  const originalUUID = crypto.randomUUID;
  try {
    writeFileSync(file, previous);
    crypto.randomUUID = () => "collision";
    syncBuiltinESMExports();
    const collision = `${file}.collision.tmp`;
    writeFileSync(collision, "another writer's temporary file");
    assert.throws(() => saveCheckpoint({ ...options, run: runner() }, file));
    assert.equal(readFileSync(collision, "utf8"), "another writer's temporary file");
    rmSync(collision);
    withFailingWrite(() => assert.throws(() => saveCheckpoint({ ...options, run: runner() }, file), /synthetic write failure/));
    assert.equal(readFileSync(file, "utf8"), previous);
    assert.equal(fs.existsSync(collision), false, "failed owned write must clean up its own temporary file");
  } finally {
    crypto.randomUUID = originalUUID;
    syncBuiltinESMExports();
  }
  assert.equal(readdirSync(directory).filter(name => name.endsWith(".tmp")).length, 0);
  console.log("Consumer recipes: stable checkpoint, four whole-answer refusals, four unknown panes, a conflict resolved by topology, a conflict left unknown, unlisted sessions, kept checkpoints, exact inspection and three inspection refusals passed");
} finally { rmSync(directory, { recursive: true, force: true }); }
