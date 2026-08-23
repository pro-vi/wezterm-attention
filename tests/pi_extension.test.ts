// Regression tests for pi/index.ts, driving the REAL registered lifecycle and
// event handlers through a mock ExtensionAPI. Named after the properties they
// lock — several are the negation of a bug fixed during review triage.
// Mutations are awaited deterministically (the event handler returns its queue
// promise), never slept on. Run: `bun test`.
//
// The mock event bus models the real one (node:events under the hood): `on`
// appends a handler and returns a disposer that removes it, and `emit` invokes
// every current handler. One deliberate divergence: this `emit` awaits the
// handlers, while the real bus discards their return value. That is a test
// affordance, not a capability production has — do not conclude from these tests
// that emitting is awaitable. Ordering is guaranteed by `mutationChain`, not by
// awaiting the emit.
//
// Fidelity caveat: `loadExt().load()` re-invokes the default export against the
// SAME module scope, so it exercises re-registration but NOT a cache-cleared
// generation with a fresh `mutationChain`. The "reload (real module re-eval)"
// test below covers genuine module re-evaluation via `import(...?gen=N)`; that
// one is what actually locks retire-at-registration against the WeakMap-scope
// and key-choice regressions.
import { test, expect, afterEach } from "bun:test";
import { mkdtempSync, mkdirSync, existsSync, readFileSync, readdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ext from "../pi/index.ts";

type Handler = (data: unknown) => void | Promise<void>;

function loadExt() {
	const lifecycle: Record<string, () => Promise<void> | void> = {};
	const handlers: Handler[] = [];
	const channels: string[] = [];
	let commandCalls = 0;
	const pi = {
		events: {
			on: (channel: string, h: Handler) => {
				channels.push(channel);
				handlers.push(h);
				return () => {
					const i = handlers.indexOf(h);
					if (i >= 0) handlers.splice(i, 1);
				};
			},
		},
		on: (event: string, h: () => Promise<void> | void) => {
			lifecycle[event] = h;
		},
		registerCommand: () => {
			commandCalls++;
		},
	};
	// load() re-invokes the extension's default export against the SAME pi — the
	// in-process stand-in for a fresh extension instance after Pi's /reload.
	const load = () => ext(pi as any);
	// emit() fans a payload out to every registered event handler, mirroring the
	// real bus, and awaits them so assertions never run before the write settles.
	const emit = (data: unknown) => Promise.all(handlers.map((h) => h(data)));
	load();
	return { lifecycle, emit, load, handlers, channels, commandCount: () => commandCalls };
}

// Teardown is centralised so a FAILING assertion still cleans up, and so no test
// inherits env from its neighbour. Previously both were done by trailing
// statements, which a failed expect() skips — leaking temp dirs exactly when you
// are iterating on a red test, and leaving WEZTERM_PANE set to a traversal path
// after the traversal tests.
const tempDirs: string[] = [];
afterEach(() => {
	for (const d of tempDirs.splice(0)) rmSync(d, { recursive: true, force: true });
	delete process.env.WEZTERM_PANE;
	delete process.env.WEZTERM_ATTENTION_DIR;
	delete process.env.PI_WEZTERM_ATTENTION_TTL_MS;
});

// A temp dir registered for automatic teardown.
function tempDir(prefix: string): string {
	const d = mkdtempSync(join(tmpdir(), prefix));
	tempDirs.push(d);
	return d;
}

// ...and pointed at by WEZTERM_ATTENTION_DIR.
function freshDir(prefix: string): string {
	const d = tempDir(prefix);
	process.env.WEZTERM_ATTENTION_DIR = d;
	return d;
}

function readMarker(dir: string, pane = "42"): Record<string, unknown> {
	return JSON.parse(readFileSync(join(dir, pane), "utf8"));
}

test("lifecycle: agent_start writes a thinking marker with ttl_ms, updated_at, source pi", async () => {
	const dir = freshDir("wez-life1-");
	process.env.WEZTERM_PANE = "42";
	const { lifecycle } = loadExt();
	await lifecycle["agent_start"]!();
	await lifecycle["session_shutdown"]!(); // lifecycle no longer awaits I/O; drain it
	const m = readMarker(dir);
	expect(m.type).toBe("thinking");
	expect(m.source).toBe("pi");
	expect(typeof m.revision).toBe("string");
	expect(typeof m.ttl_ms).toBe("number");
	expect(typeof m.updated_at).toBe("number"); // locks the contract field bun won't typecheck
});

test("lifecycle: tool_execution_start writes a thinking marker", async () => {
	const dir = freshDir("wez-tool-");
	process.env.WEZTERM_PANE = "42";
	const { lifecycle } = loadExt();
	await lifecycle["tool_execution_start"]!();
	await lifecycle["session_shutdown"]!(); // lifecycle no longer awaits I/O; drain it
	expect(readMarker(dir).type).toBe("thinking");
});

test("lifecycle: agent_settled — not agent_end — writes the stop marker", async () => {
	// agent_end is nonterminal (auto-retry/compaction can follow); only
	// agent_settled means Pi is truly done, so `stop` must hang off it. Assert
	// agent_end is NOT even registered, so a false mid-run ✓ is impossible.
	const dir = freshDir("wez-settled-");
	process.env.WEZTERM_PANE = "42";
	const { lifecycle } = loadExt();
	expect(lifecycle["agent_end"]).toBeUndefined();
	await lifecycle["agent_settled"]!();
	await lifecycle["session_shutdown"]!(); // lifecycle no longer awaits I/O; drain it
	const m = readMarker(dir);
	expect(m.type).toBe("stop");
	expect(m.ttl_ms).toBeUndefined();
});

test("lifecycle: handlers return before marker I/O lands (no agent-critical-path await)", async () => {
	// Pi awaits lifecycle handlers on the agent's OWN critical path:
	//   agent-loop.ts  await emit("tool_execution_start")  → then prepares the tool call
	//   agent.ts       for (listener of listeners) await listener(event, signal)   (serial, no timeout)
	//   agent-session.ts  await this._emitExtensionEvent(event)  → before the TUI notify
	//   runner.ts      await handler(event, ctx)
	// So awaiting a marker write here puts the filesystem in the agent's latency
	// budget, and on a mount whose syscalls block forever it wedges the host: one
	// stuck op parks every later lifecycle event (shared serial chain), the TUI never
	// sees the event, and abort cannot release a parked await. Headless is worse —
	// `_resolveIdleWaitIfIdle()` lives in a `finally` whose `try` awaits this emit, so
	// `pi -p` never terminates.
	//
	// Both assertions carry weight: the first locks OUT restoring the await, the
	// second locks IN that the write is deferred rather than dropped.
	const dir = freshDir("wez-nocritpath-");
	process.env.WEZTERM_PANE = "42";
	const { lifecycle } = loadExt();
	await lifecycle["agent_start"]!();
	expect(existsSync(join(dir, "42"))).toBe(false); // returned without waiting on I/O
	await lifecycle["session_shutdown"]!();
	expect(existsSync(join(dir, "42"))).toBe(true); // ...and the write still lands
});

test("registration: the extension listens on the wezterm-attention:mark channel", () => {
	freshDir("wez-chan-");
	const { channels } = loadExt();
	expect(channels).toContain("wezterm-attention:mark");
});

test("registration: the extension registers no commands", () => {
	freshDir("wez-nocmd-");
	const { commandCount } = loadExt();
	expect(commandCount()).toBe(0);
});

test("event: emitting a notify object writes a labeled notify marker", async () => {
	const dir = freshDir("wez-evt-");
	process.env.WEZTERM_PANE = "42";
	const { emit } = loadExt();
	await emit({ type: "notify", label: "answer me" });
	const m = readMarker(dir);
	expect(m.type).toBe("notify");
	expect(m.label).toBe("answer me");
});

test("event: a bare string state is accepted", async () => {
	const dir = freshDir("wez-evtstr-");
	process.env.WEZTERM_PANE = "42";
	const { emit } = loadExt();
	await emit("review");
	expect(readMarker(dir).type).toBe("review");
});

test("publication: identical states receive distinct revisions", async () => {
	const dir = freshDir("wez-revision-");
	process.env.WEZTERM_PANE = "42";
	const { emit } = loadExt();

	await emit("notify");
	const first = readMarker(dir).revision;
	await emit("notify");
	const second = readMarker(dir).revision;

	expect(typeof first).toBe("string");
	expect(typeof second).toBe("string");
	expect(second).not.toBe(first);
});

test("ordering: notify then clear leaves NO marker (last requested wins)", async () => {
	const dir = freshDir("wez-order1-");
	process.env.WEZTERM_PANE = "42";
	const { emit } = loadExt();
	let present = 0;
	for (let i = 0; i < 30; i++) {
		// Emit both before either settles (exercise the interleaving), then await
		// both deterministically — no sleep, so the assertion can't run early.
		const a = emit("notify");
		const b = emit("clear");
		await Promise.all([a, b]);
		if (existsSync(join(dir, "42"))) present++;
	}
	expect(present).toBe(0);
});

test("ordering: clear then notify leaves a notify marker", async () => {
	const dir = freshDir("wez-order2-");
	process.env.WEZTERM_PANE = "42";
	const { emit } = loadExt();
	writeFileSync(join(dir, "42"), "{}");
	const a = emit("clear");
	const b = emit("notify");
	await Promise.all([a, b]);
	expect(existsSync(join(dir, "42"))).toBe(true);
	expect(readMarker(dir).type).toBe("notify");
});

test("reload (real module re-eval): retire-at-registration collapses N fresh generations to one listener", async () => {
	// Genuine reload semantics: each generation is a re-evaluated module (fresh
	// module scope / fresh mutationChain), sharing ONE bus, all seeing the same
	// globalThis WeakMap — which the same-instance harness can't reproduce. This
	// is the test that goes red if the registry is module-level instead of
	// globalThis, or keyed on `pi` instead of `pi.events`.
	// Handlers are tagged with the generation that registered them, because
	// cardinality alone does not lock the property this test exists for: a mutant
	// that disposes ITSELF instead of its predecessor (`if (prev) disposeEvent()`
	// rather than `prev?.()` — two same-typed locals on adjacent lines) also leaves
	// exactly one listener, but it is generation 0's, live forever, while every
	// later reload silently registers and immediately retires itself.
	const handlers: Array<{ gen: number; h: (d: unknown) => unknown }> = [];
	let currentGen = -1;
	const bus = {
		on: (_ch: string, h: (d: unknown) => unknown) => {
			const entry = { gen: currentGen, h };
			handlers.push(entry);
			return () => {
				const i = handlers.indexOf(entry);
				if (i >= 0) handlers.splice(i, 1);
			};
		},
	};
	const makePi = () => ({ events: bus, on: () => {}, registerCommand: () => {} });
	for (let gen = 0; gen < 4; gen++) {
		currentGen = gen;
		const mod = await import(`../pi/index.ts?realreload=${gen}`);
		mod.default(makePi() as any);
	}
	expect(handlers.length).toBe(1); // all four generations collapsed to one live listener
	// The survivor is the NEWEST generation. This asserts listener IDENTITY because
	// nothing else distinguishes the generations: they are byte-identical modules and
	// every config value is re-read from process.env at write time.
	//
	// That makes it design-specific on purpose. Under the deferred bus-owned-controller
	// design — one listener installed once per bus, later generations swapping only a
	// delegate — the survivor would legitimately be gen 0 and this line SHOULD fail.
	// If you are doing that migration, revisit this expectation; do not "fix" the
	// controller to preserve gen-3 identity, which would reintroduce the register/
	// retire dance the migration exists to delete.
	expect(handlers.map((e) => e.gen)).toEqual([3]);
});

test("reload: a new generation retires the previous listener (no accumulation)", async () => {
	// Same-instance re-registration (see the fidelity caveat at the top): covers
	// the re-register path; the real-module-re-eval test above covers cross-gen.
	freshDir("wez-reload-");
	const h = loadExt();
	expect(h.handlers.length).toBe(1); // gen-0 registered exactly one listener
	h.load(); // gen-1 registers and retires gen-0's listener
	expect(h.handlers.length).toBe(1); // one, not stacked
	h.load(); // gen-2
	expect(h.handlers.length).toBe(1);
});

test("failed reload: session_shutdown does NOT dispose the listener", async () => {
	// reload() emits session_shutdown BEFORE its fallible work, and a reload
	// failure keeps the session running (handleReloadCommand catches it). So
	// disposing on shutdown would silence notify with no successor. Shutdown must
	// only drain — the listener stays live.
	const dir = freshDir("wez-failreload-");
	process.env.WEZTERM_PANE = "42";
	const h = loadExt();
	expect(typeof h.lifecycle["session_shutdown"]).toBe("function");
	await h.lifecycle["session_shutdown"]!(); // shutdown fires, then imagine reload throws
	expect(h.handlers.length).toBe(1); // listener still live
	await h.emit("notify"); // cooperative path still works
	expect(existsSync(join(dir, "42"))).toBe(true);
});

test("session_shutdown drains in-flight writes before returning", async () => {
	// The drain is what closes the cross-reload write race — locked separately so
	// removing `await mutationChain` turns the suite red.
	const dir = freshDir("wez-drain-");
	process.env.WEZTERM_PANE = "42";
	const h = loadExt();
	const pending = h.emit("notify"); // in-flight; deliberately not awaited here
	await h.lifecycle["session_shutdown"]!(); // must not return until `pending` settles
	expect(existsSync(join(dir, "42"))).toBe(true); // drained → write landed
	await pending;
});

test("traversal: a bad pane id writes NOTHING outside the marker dir", async () => {
	const parent = tempDir("wez-trav1-");
	const dir = join(parent, "markerdir");
	mkdirSync(dir);
	const victim = join(parent, "victim.txt");
	writeFileSync(victim, "IMPORTANT");
	process.env.WEZTERM_ATTENTION_DIR = dir;
	process.env.WEZTERM_PANE = "../victim.txt";
	const { emit } = loadExt();
	await emit("notify");
	expect(readFileSync(victim, "utf8")).toBe("IMPORTANT"); // untouched
});

test("traversal: a bad pane id does NOT delete an outside file on clear", async () => {
	const parent = tempDir("wez-trav2-");
	const dir = join(parent, "markerdir");
	mkdirSync(dir);
	const victim = join(parent, "victim.txt");
	writeFileSync(victim, "IMPORTANT");
	process.env.WEZTERM_ATTENTION_DIR = dir;
	process.env.WEZTERM_PANE = "../victim.txt";
	const { emit } = loadExt();
	await emit("clear");
	expect(existsSync(victim)).toBe(true); // not deleted
});

test("cleanup: a failed rename does not leak temp files", async () => {
	const dir = freshDir("wez-leak-");
	process.env.WEZTERM_PANE = "42";
	mkdirSync(join(dir, "42")); // marker path is a directory → rename fails
	const { emit } = loadExt();
	for (let i = 0; i < 10; i++) await emit("notify");
	const leaked = readdirSync(dir).filter((f) => f.includes(".tmp."));
	expect(leaked.length).toBe(0);
});

test("missing pane: lifecycle write is a silent no-op that creates no file", async () => {
	const dir = freshDir("wez-nopane-");
	delete process.env.WEZTERM_PANE; // the condition under test, not teardown
	const { lifecycle } = loadExt();
	await lifecycle["agent_start"]!();
	await lifecycle["session_shutdown"]!(); // lifecycle no longer awaits I/O; drain it
	expect(readdirSync(dir).length).toBe(0); // nothing written at all, not merely no "undefined" file
});

test('env: a unit-suffixed TTL ("30m") is rejected, not parsed as 30', async () => {
	// F7: parseInt("30m") === 30 silently produced a 30ms TTL. Strict parse must
	// reject it and fall back to the 30-minute default.
	const dir = freshDir("wez-ttl-");
	process.env.WEZTERM_PANE = "42";
	process.env.PI_WEZTERM_ATTENTION_TTL_MS = "30m";
	const { lifecycle } = loadExt();
	await lifecycle["agent_start"]!();
	await lifecycle["session_shutdown"]!(); // lifecycle no longer awaits I/O; drain it
	const m = readMarker(dir);
	expect(m.ttl_ms).toBe(30 * 60 * 1000); // default, NOT 30
});

test("env: a relative WEZTERM_ATTENTION_DIR is rejected (no cwd scatter, no cwd delete)", async () => {
	// F2: markerDirectory requires an absolute result. A relative override would
	// otherwise write markers under cwd (scatter) and let clear rm() a cwd file.
	const relDir = join(process.cwd(), "wez-rel-marker-dir");
	rmSync(relDir, { recursive: true, force: true });
	process.env.WEZTERM_ATTENTION_DIR = "wez-rel-marker-dir";
	process.env.WEZTERM_PANE = "42";
	const { lifecycle, emit } = loadExt();
	await lifecycle["agent_start"]!(); // write path: guarded → no file created
	await lifecycle["session_shutdown"]!(); // lifecycle no longer awaits I/O; drain it
	await emit("clear"); // clear path shares the same guard → no rm of a cwd file
	expect(existsSync(join(relDir, "42"))).toBe(false);
	expect(existsSync(relDir)).toBe(false); // dir never even created
	rmSync(relDir, { recursive: true, force: true });
});

test('env: TTL_MS="0" falls back to the default, not an instantly-stale marker', async () => {
	// "0" is a plausible "disable the TTL" reading, and it passes the digits-only
	// gate — only the `parsed > 0` range check rejects it. Without that check the
	// marker ships ttl_ms: 0 and plugin/init.lua expires the spinner immediately.
	const dir = freshDir("wez-ttl0-");
	process.env.WEZTERM_PANE = "42";
	process.env.PI_WEZTERM_ATTENTION_TTL_MS = "0";
	const { lifecycle } = loadExt();
	await lifecycle["agent_start"]!();
	await lifecycle["session_shutdown"]!(); // lifecycle no longer awaits I/O; drain it
	const m = readMarker(dir);
	expect(m.ttl_ms).toBe(30 * 60 * 1000); // default, NOT 0
});

// The alias table README.md advertises to other extension authors. It is a public
// cross-extension contract, so every row is locked here rather than left to the
// lifecycle path (which only ever calls mark("thinking"|"stop") directly and so
// exercises none of the mapping).
const ALIAS_CASES: Array<[string, string]> = [
	["busy", "thinking"],
	["thinking", "thinking"],
	["ready", "stop"],
	["stop", "stop"],
	["blocked", "notify"],
	["pending", "notify"],
	["notify", "notify"],
	["review", "review"],
];

test("event: every documented alias maps to its canonical marker state", async () => {
	for (const [alias, expected] of ALIAS_CASES) {
		const dir = freshDir("wez-alias-");
		process.env.WEZTERM_PANE = "42";
		const h = loadExt();
		await h.emit(alias);
		const m = readMarker(dir);
		expect(m.type).toBe(expected); // `${alias}` → `${expected}`
		rmSync(dir, { recursive: true, force: true });
	}
});

test("event: an unrecognized state is rejected, writing nothing", async () => {
	// normalizeState's `default: undefined` is the gate. Without it an arbitrary
	// string reaches disk as {"type":"bogus"} — litter the plugin's valid_types
	// table ignores, but litter this writer should never produce.
	const dir = freshDir("wez-bogus-");
	process.env.WEZTERM_PANE = "42";
	const h = loadExt();
	await h.emit("bogus");
	expect(readdirSync(dir).length).toBe(0);
});
