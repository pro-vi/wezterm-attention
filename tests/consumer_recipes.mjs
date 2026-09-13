import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import crypto from "node:crypto";
import fs from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { captureCheckpoint, saveCheckpoint } from "../examples/checkpoint.mjs";
import { inspectBindings } from "../examples/inspect.mjs";

// The Rust test supplies actual production CLI envelopes, not a second schema.
const fixture = JSON.parse(readFileSync(process.argv[2], "utf8"));
const directory = mkdtempSync(join(tmpdir(), "attention-consumer-recipes-"));
const file = join(directory, "checkpoint.json");
try {
  function runner(change = value => value) {
    let queries = 0;
    return (_executable, args, input, env) => {
      if (args[0] === "bindings") return change(structuredClone(fixture.bindings), ++queries);
      if (args[0] === "inspect") {
        const row = fixture.bindings.result.rows.find(row => row.current);
        assert.deepEqual(JSON.parse(input), { address: row.address, launch_id: row.launch_id, binding_id: row.binding_id });
        return change(structuredClone(fixture.inspect), 3);
      }
      assert.deepEqual(args, ["--skip-config", "cli", "--no-auto-start", "list", "--format", "json"]);
      assert.equal(env.WEZTERM_UNIX_SOCKET, "/synthetic.sock");
      return [{ pane_id: 42, tab_id: 7, is_active: true }];
    };
  }
  const options = { attention: "/attention", socket: "/synthetic.sock", wezterm: "/wezterm" };
  const result = captureCheckpoint({ ...options, run: runner() });
  assert.equal(result.panes[0].topology.tab_id, 7);
  assert.deepEqual(result.panes[0].binding, fixture.bindings.result.rows.find(row => row.current));
  for (const change of [
    (value, n) => { if (n === 2) value.result.scope.incarnation_id = "d".repeat(64); return value; },
    value => { value.complete = false; return value; },
    value => { value.result.rows.push(structuredClone(value.result.rows.find(row => row.current))); return value; },
    value => { value.result.rows.find(row => row.current).reader_confidence = "unconfirmed"; return value; },
  ]) {
    writeFileSync(file, "previous checkpoint");
    assert.throws(() => saveCheckpoint({ ...options, run: runner(change) }, file));
    assert.equal(readFileSync(file, "utf8"), "previous checkpoint");
  }
  const unknown = captureCheckpoint({ ...options, run: runner(value => { value.result.rows = []; return value; }) });
  assert.equal(unknown.panes[0].binding, null, "missing association cannot become a historical fallback");
  const facts = inspectBindings({ ...options, run: runner() });
  assert.deepEqual(facts.panes[0], fixture.inspect.result);
  assert.throws(() => inspectBindings({ ...options, run: runner((value, n) => {
    if (n === 3) { value.complete = false; value.result.scope_relation = "binding_changed"; }
    return value;
  }) }));
  let inspected = false;
  assert.throws(() => inspectBindings({ ...options, run: (_exe, args) => {
    if (args[0] === "inspect") inspected = true;
    return { ...fixture.bindings, complete: false };
  } }));
  assert.equal(inspected, false, "incomplete discovery must stop before inspection");
  const originalUUID = crypto.randomUUID;
  const originalWrite = fs.writeFileSync;
  try {
    crypto.randomUUID = () => "collision";
    syncBuiltinESMExports();
    const collision = `${file}.collision.tmp`;
    writeFileSync(collision, "another writer's temporary file");
    assert.throws(() => saveCheckpoint({ ...options, run: runner() }, file));
    assert.equal(readFileSync(collision, "utf8"), "another writer's temporary file");
    rmSync(collision);
    writeFileSync(file, "previous checkpoint");
    fs.writeFileSync = (target, ...args) => {
      if (typeof target === "number") throw new Error("synthetic write failure");
      return originalWrite(target, ...args);
    };
    syncBuiltinESMExports();
    assert.throws(() => saveCheckpoint({ ...options, run: runner() }, file));
    assert.equal(readFileSync(file, "utf8"), "previous checkpoint");
    assert.equal(fs.existsSync(collision), false, "failed owned write must clean up its own temporary file");
  } finally {
    crypto.randomUUID = originalUUID;
    fs.writeFileSync = originalWrite;
    syncBuiltinESMExports();
  }
  console.log("Consumer recipes: stable checkpoint, four retained-checkpoint failures, unknown association, exact inspection and two refusal cases passed");
} finally { rmSync(directory, { recursive: true, force: true }); }
