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
import { test, expect, beforeEach, afterEach } from "bun:test";
import { chmodSync, mkdtempSync, mkdirSync, existsSync, readFileSync, readdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ext, { drainTimeoutMs } from "../../pi/index.ts";

type Handler = (data: unknown) => void | Promise<void>;
type TestEvent = Record<string, unknown>;
type TestContext = {
	cwd: string;
	sessionManager: { getSessionId(): string; getSessionFile(): string | undefined };
	model: { id: string } | undefined;
	ui: { notify(message: string, level?: string): void };
};

const notifications: Array<{ message: string; level?: string }> = [];

const testContext: TestContext = {
	cwd: "/tmp/pi-project",
	sessionManager: {
		getSessionId: () => "pi-test-session",
		getSessionFile: () => "/tmp/pi-test-session.jsonl",
	},
	model: { id: "pi-test-model" },
	ui: { notify: (message, level) => notifications.push({ message, ...(level ? { level } : {}) }) },
};

function lifecycleEvent(name: string): TestEvent {
	if (name === "session_start") return { type: name, reason: "startup" };
	if (name === "session_shutdown") return { type: name, reason: "quit" };
	return { type: name };
}

function loadExt() {
	const lifecycle: Record<string, (event?: TestEvent, ctx?: TestContext) => Promise<void> | void> = {};
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
		on: (event: string, h: (event: TestEvent, ctx: TestContext) => Promise<void> | void) => {
			lifecycle[event] = (eventValue = lifecycleEvent(event), ctx = testContext) => h(eventValue, ctx);
		},
		registerCommand: () => {
			commandCalls++;
		},
	};
	// load() re-invokes the extension's default export against the SAME pi — the
	// in-process stand-in for a fresh extension instance after Pi's /reload.
	const load = () => ext(pi as Parameters<typeof ext>[0]);
	// emit() fans a payload out to every registered event handler, mirroring the
	// real bus, and awaits them so assertions never run before the write settles.
	const emit = (data: unknown) => Promise.all(handlers.map((h) => h(data)));
	load();
	return { lifecycle, emit, load, handlers, channels, commandCount: () => commandCalls };
}

// Teardown is centralised so a FAILING assertion still cleans up, and so no test
// inherits env from its neighbour: trailing statements are skipped by a failed
// expect(), which would leak temp dirs exactly when you are iterating on a red
// test, and leave one test's environment in the next.
const tempDirs: string[] = [];
const originalHome = process.env.HOME;
function clearAttentionEnvironment(): void {
	delete process.env.WEZTERM_PANE;
	delete process.env.WEZTERM_ATTENTION_DIR;
	delete process.env.XDG_STATE_HOME;
	if (originalHome === undefined) delete process.env.HOME;
	else process.env.HOME = originalHome;
	delete process.env.WEZTERM_ATTENTION_ROOT;
	delete process.env.WEZTERM_ATTENTION_TEST_LOG;
	delete process.env.WEZTERM_ATTENTION_HOST_PID;
	delete process.env.WEZTERM_ATTENTION_LAUNCH_ID;
	// Every writer in this file is a local fake, so the shipped 2s drain cap is
	// only ever measuring how long this machine takes to spawn a shell. Under the
	// gate's parallel load that exceeded 2s and two tests failed on a property
	// they do not test: one read calls.log before the fake had written it, the
	// other saw the abandoned writer report a signal. Raise the cap instead of
	// retrying the assertion.
	process.env.PI_WEZTERM_ATTENTION_DRAIN_TIMEOUT_MS = "30000";
}

beforeEach(clearAttentionEnvironment);
afterEach(() => {
	for (const d of tempDirs.splice(0)) rmSync(d, { recursive: true, force: true });
	notifications.length = 0;
	clearAttentionEnvironment();
});

// session_shutdown's drain is capped at DRAIN_TIMEOUT_MS and abandons the wait
// without cancelling the write, so on a loaded machine a spawned writer's report
// can land just after it returns. Wait for the report; its arrival is the
// property under test, not how fast the drain got there.
async function reportedNotifications(expected: number) {
	const deadline = Date.now() + 10_000;
	while (notifications.length < expected && Date.now() < deadline) {
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	return notifications;
}

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

function parseRecord(text: string): Record<string, unknown> {
	const parsed: unknown = JSON.parse(text);
	if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
		throw new Error("value is not a JSON object");
	}
	return parsed;
}

// A checkout whose bin/attention logs each call's arguments and the payload it
// was given, one line each, to the returned log.
function fakeWriter(prefix: string): string {
	const root = tempDir(prefix);
	const log = join(root, "calls.log");
	mkdirSync(join(root, "bin"));
	writeFileSync(
		join(root, "bin", "attention"),
		'#!/bin/sh\nIFS= read -r payload || :\nprintf "%s\\n%s\\n" "$*" "$payload" >> "$WEZTERM_ATTENTION_TEST_LOG"\n',
	);
	chmodSync(join(root, "bin", "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	process.env.WEZTERM_ATTENTION_TEST_LOG = log;
	return log;
}

function loggedCalls(log: string): Array<{ command: string; payload: Record<string, unknown> }> {
	if (!existsSync(log)) return [];
	const lines = readFileSync(log, "utf8").trim().split("\n");
	const calls = [];
	for (let index = 0; index + 1 < lines.length; index += 2) {
		calls.push({ command: lines[index]!, payload: parseRecord(lines[index + 1]!) });
	}
	return calls;
}

test("lifecycle: agent_settled — not agent_end — reports the turn settled", async () => {
	// agent_end is nonterminal (auto-retry/compaction can follow); only
	// agent_settled means Pi is truly done, so `stop` must hang off it. Assert
	// agent_end is NOT even registered, so a false mid-run ✓ is impossible.
	const log = fakeWriter("wez-settled-");
	const { lifecycle } = loadExt();
	expect(lifecycle["agent_end"]).toBeUndefined();
	await lifecycle["session_start"]!();
	await lifecycle["agent_settled"]!();
	await lifecycle["session_shutdown"]!(); // lifecycle does not await the writer; drain it
	expect(loggedCalls(log).map((call) => call.command)).toContain("hooks event pi agent_settled");
});

test("lifecycle: handlers return before the writer runs (no agent-critical-path await)", async () => {
	// Pi awaits lifecycle handlers on the agent's OWN critical path:
	//   agent-loop.ts  await emit("tool_execution_start")  → then prepares the tool call
	//   agent.ts       for (listener of listeners) await listener(event, signal)   (serial, no timeout)
	//   agent-session.ts  await this._emitExtensionEvent(event)  → before the TUI notify
	//   runner.ts      await handler(event, ctx)
	// So awaiting the writer here puts the filesystem in the agent's latency
	// budget, and on a mount whose syscalls block forever it wedges the host: one
	// stuck op parks every later lifecycle event (shared serial chain), the TUI never
	// sees the event, and abort cannot release a parked await. Headless is worse —
	// `_resolveIdleWaitIfIdle()` lives in a `finally` whose `try` awaits this emit, so
	// `pi -p` never terminates.
	//
	// Both assertions carry weight: the first locks OUT restoring the await, the
	// second locks IN that the write is deferred rather than dropped.
	const log = fakeWriter("wez-nocritpath-");
	const { lifecycle } = loadExt();
	await lifecycle["session_start"]!();
	await lifecycle["agent_start"]!();
	expect(existsSync(log)).toBe(false); // returned without waiting on the writer
	await lifecycle["session_shutdown"]!();
	expect(loggedCalls(log).map((call) => call.command)).toContain("hooks event pi agent_start"); // ...and it still ran
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

test("v2 dispatch: Pi lifecycle and bus requests use the serialized attention process queue", async () => {
	const root = tempDir("wez-v2-dispatch-");
	const bin = join(root, "bin");
	const log = join(root, "calls.log");
	mkdirSync(bin);
	writeFileSync(
		join(bin, "attention"),
		'#!/bin/sh\nprintf "%s\\n" "$*" >> "$WEZTERM_ATTENTION_TEST_LOG"\nIFS= read -r payload\nprintf "%s\\n" "$payload" >> "$WEZTERM_ATTENTION_TEST_LOG"\n',
	);
	chmodSync(join(bin, "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	process.env.WEZTERM_ATTENTION_TEST_LOG = log;
	const h = loadExt();

	await h.lifecycle["session_start"]!();
	await h.lifecycle["agent_start"]!();
	await h.lifecycle["tool_execution_start"]!();
	await h.lifecycle["agent_settled"]!();
	await h.emit({ type: "notify", label: "answer me" });
	await h.emit("review");
	await h.emit("clear");
	await h.lifecycle["session_shutdown"]!();

	const lines = readFileSync(log, "utf8").trim().split("\n");
	const commands = lines.filter((_, index) => index % 2 === 0);
	const payloads = lines.filter((_, index) => index % 2 === 1).map(parseRecord);
	expect(commands).toEqual([
		"hooks event pi session_start",
		"hooks event pi agent_start",
		"hooks event pi tool_execution_start",
		"hooks event pi agent_settled",
		"hooks event pi bus",
		"hooks event pi bus",
		"hooks event pi bus",
		"hooks event pi session_shutdown",
	]);
	expect(payloads.every((payload) => payload.session_id === "pi-test-session")).toBe(true);
	expect(payloads[0]?.start_source).toBe("startup");
	expect(payloads[4]?.state).toBe("notify");
	expect(payloads[4]?.label).toBe("answer me");
	expect(payloads[5]?.state).toBe("review");
	expect(payloads[6]?.state).toBe("clear");
	expect(payloads[7]?.reason).toBe("quit");
});

test("v2 dispatch: the writer is told Pi's own pid as its host, whatever Pi inherited", async () => {
	const root = tempDir("wez-v2-host-");
	const bin = join(root, "bin");
	const log = join(root, "calls.log");
	mkdirSync(bin);
	writeFileSync(
		join(bin, "attention"),
		'#!/bin/sh\nprintf "%s %s\\n" "${WEZTERM_ATTENTION_HOST_PID-unset}" "${WEZTERM_ATTENTION_LAUNCH_ID-unset}" >> "$WEZTERM_ATTENTION_TEST_LOG"\ncat > /dev/null\n',
	);
	chmodSync(join(bin, "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	process.env.WEZTERM_ATTENTION_TEST_LOG = log;
	// A value Pi inherited names whoever started Pi, not Pi.
	process.env.WEZTERM_ATTENTION_HOST_PID = "1";
	const h = loadExt();

	await h.lifecycle["session_start"]!();
	await h.lifecycle["agent_start"]!();
	await h.lifecycle["session_shutdown"]!();

	expect(readFileSync(log, "utf8").trim().split("\n")).toEqual([
		`${process.pid} unset`,
		`${process.pid} unset`,
		`${process.pid} unset`,
	]);
});

test("v2 dispatch: Pi reload drains queued writes without sending an end event or warning", async () => {
	const root = tempDir("wez-v2-reload-");
	const bin = join(root, "bin");
	const log = join(root, "calls.log");
	mkdirSync(bin);
	writeFileSync(
		join(bin, "attention"),
		'#!/bin/sh\nprintf "%s\\n" "$*" >> "$WEZTERM_ATTENTION_TEST_LOG"\nIFS= read -r payload\nprintf "%s\\n" "$payload" >> "$WEZTERM_ATTENTION_TEST_LOG"\ncase "$*" in *session_shutdown*) printf "%s\\n" "attention: integration_version_mismatch: Pi reload keeps the current binding" >&2;; esac\n',
	);
	chmodSync(join(bin, "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	process.env.WEZTERM_ATTENTION_TEST_LOG = log;
	const h = loadExt();

	await h.lifecycle["session_start"]!();
	await h.lifecycle["session_shutdown"]!({ type: "session_shutdown", reason: "reload" }, testContext);

	const lines = readFileSync(log, "utf8").trim().split("\n");
	expect(lines.filter((_, index) => index % 2 === 0)).toEqual([
		"hooks event pi session_start",
	]);
	expect(notifications).toEqual([]);
});

test("writer: an unset checkout root records nothing and says so once", async () => {
	const dir = freshDir("wez-unset-root-");
	process.env.WEZTERM_PANE = "42";
	const { lifecycle, emit } = loadExt();
	await lifecycle["session_start"]!();
	await lifecycle["agent_start"]!();
	await emit("notify");
	await lifecycle["session_shutdown"]!();
	expect(readdirSync(dir)).toEqual([]);
	expect(await reportedNotifications(1)).toHaveLength(1);
	expect(notifications[0]?.message).toContain("WEZTERM_ATTENTION_ROOT is not set");
	expect(notifications[0]?.level).toBe("warning");
});

// Pi runs in other terminals too, where there is no tab to show anything on.
test("writer: outside a WezTerm pane an unset checkout root records nothing and says nothing", async () => {
	const dir = freshDir("wez-outside-pane-");
	const { lifecycle, emit } = loadExt();
	await lifecycle["session_start"]!();
	await lifecycle["agent_start"]!();
	await emit("notify");
	await lifecycle["session_shutdown"]!();
	expect(readdirSync(dir)).toEqual([]);
	expect(notifications).toEqual([]);
});

test("writer: a configured checkout without a writer logs once", async () => {
	process.env.WEZTERM_ATTENTION_ROOT = tempDir("wez-root-missing-");
	const { lifecycle } = loadExt();
	await lifecycle["session_start"]!();
	await lifecycle["agent_start"]!();
	await lifecycle["session_shutdown"]!();
	expect(await reportedNotifications(1)).toHaveLength(1);
	expect(notifications[0]?.message).toContain("no executable bin/attention");
	expect(notifications[0]?.level).toBe("warning");
});

test("writer: an invoked writer exit logs once", async () => {
	const root = tempDir("wez-root-exit-");
	mkdirSync(join(root, "bin"));
	writeFileSync(join(root, "bin", "attention"), "#!/bin/sh\nIFS= read -r payload || :\nexit 3\n");
	chmodSync(join(root, "bin", "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	const { lifecycle } = loadExt();
	await lifecycle["session_start"]!();
	await lifecycle["agent_start"]!();
	await lifecycle["session_shutdown"]!();
	expect(await reportedNotifications(1)).toHaveLength(1);
	expect(notifications[0]?.message).toBe("wezterm-attention: writer exited with status 3");
});

test("writer: the writer's first diagnostic line is reported without control characters", async () => {
	const root = tempDir("wez-root-line-");
	mkdirSync(join(root, "bin"));
	writeFileSync(
		join(root, "bin", "attention"),
		`#!/bin/sh\nIFS= read -r payload || :\nprintf 'attention: claim_stale: \\033]0;title\\007%s\\r\\nhelp: attention doctor\\n' '${"x".repeat(300)}' >&2\nexit 1\n`,
	);
	chmodSync(join(root, "bin", "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	const { lifecycle } = loadExt();
	await lifecycle["session_start"]!();
	await lifecycle["session_shutdown"]!();
	expect(await reportedNotifications(1)).toHaveLength(1);
	const message = notifications[0]?.message ?? "";
	expect(message.startsWith("wezterm-attention: writer exited with status 1: attention: claim_stale: ]0;title")).toBe(true);
	expect(message).not.toMatch(/\p{Cc}/u);
	expect(message).not.toContain("help: attention doctor");
	expect(message.length).toBeLessThan(300);
});

test("writer: an exit-zero hook diagnostic is reported and never treated as success", async () => {
	const root = tempDir("wez-root-diagnostic-");
	mkdirSync(join(root, "bin"));
	writeFileSync(
		join(root, "bin", "attention"),
		"#!/bin/sh\nIFS= read -r payload || :\nprintf '%s\\n' 'attention: identity_unpublished: test rejection' >&2\nexit 0\n",
	);
	chmodSync(join(root, "bin", "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	const { lifecycle } = loadExt();
	await lifecycle["session_start"]!();
	await lifecycle["agent_start"]!();
	await lifecycle["session_shutdown"]!();
	expect(await reportedNotifications(1)).toHaveLength(1);
	expect(notifications[0]?.message).toBe(
		"wezterm-attention: writer rejected the event: attention: identity_unpublished: test rejection",
	);
});

test("event: a bare string state is accepted", async () => {
	const log = fakeWriter("wez-evtstr-");
	const { lifecycle, emit } = loadExt();
	await lifecycle["session_start"]!();
	await emit("review");
	expect(loggedCalls(log).at(-1)?.payload.state).toBe("review");
});

test("ordering: notify then clear reach the writer in the order they were requested", async () => {
	const log = fakeWriter("wez-order1-");
	const { lifecycle, emit } = loadExt();
	await lifecycle["session_start"]!();
	for (let i = 0; i < 10; i++) {
		// Emit both before either settles (exercise the interleaving), then await
		// both deterministically — no sleep, so the assertion can't run early.
		const a = emit("notify");
		const b = emit("clear");
		await Promise.all([a, b]);
	}
	const states = loggedCalls(log).slice(1).map((call) => call.payload.state);
	expect(states).toEqual(Array.from({ length: 10 }, () => ["notify", "clear"]).flat());
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
		const mod = await import(`../../pi/index.ts?realreload=${gen}`);
		mod.default(makePi() as Parameters<typeof mod.default>[0]);
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
	const log = fakeWriter("wez-failreload-");
	const h = loadExt();
	expect(typeof h.lifecycle["session_shutdown"]).toBe("function");
	await h.lifecycle["session_start"]!();
	await h.lifecycle["session_shutdown"]!(); // shutdown fires, then imagine reload throws
	expect(h.handlers.length).toBe(1); // listener still live
	await h.emit("notify"); // cooperative path still works
	expect(loggedCalls(log).at(-1)?.payload.state).toBe("notify");
});

test("session_shutdown drains in-flight writes before returning", async () => {
	// The drain is what closes the cross-reload write race — locked separately so
	// removing `await mutationChain` turns the suite red.
	const log = fakeWriter("wez-drain-");
	const h = loadExt();
	await h.lifecycle["session_start"]!();
	const pending = h.emit("notify"); // in-flight; deliberately not awaited here
	await h.lifecycle["session_shutdown"]!(); // must not return until `pending` settles
	expect(loggedCalls(log).some((call) => call.payload.state === "notify")).toBe(true); // drained → write landed
	await pending;
});

test("env: a configured writer is never started with a root that is not UTF-8", async () => {
	// The child is given the decoded text, U+FFFD encoded as valid UTF-8, so the
	// writer would take it as a root that no reader resolves.
	const root = tempDir("wez-utf8-writer-");
	const log = join(root, "calls.log");
	const scratch = tempDir("wez-utf8-writer-root-");
	mkdirSync(join(root, "bin"));
	writeFileSync(join(root, "bin", "attention"), '#!/bin/sh\nIFS= read -r payload || :\nprintf "%s\\n" "$*" >> "$WEZTERM_ATTENTION_TEST_LOG"\n');
	chmodSync(join(root, "bin", "attention"), 0o755);
	process.env.WEZTERM_ATTENTION_ROOT = root;
	process.env.WEZTERM_ATTENTION_TEST_LOG = log;
	process.env.WEZTERM_PANE = "42";
	const broken = join(scratch, "x�y");
	const cases: Array<{ dir?: string; stateHome?: string; refused?: string }> = [
		{ dir: broken, refused: "WEZTERM_ATTENTION_DIR" },
		{ stateHome: broken, refused: "XDG_STATE_HOME" },
		{ dir: "", stateHome: broken, refused: "XDG_STATE_HOME" },
		{ dir: scratch, stateHome: broken },
		// A relative XDG_STATE_HOME is ignored by the writer, as by every reader.
		{ stateHome: "x\uFFFDy" },
	];
	for (const { dir, stateHome, refused } of cases) {
		delete process.env.WEZTERM_ATTENTION_DIR;
		delete process.env.XDG_STATE_HOME;
		if (dir !== undefined) process.env.WEZTERM_ATTENTION_DIR = dir;
		if (stateHome !== undefined) process.env.XDG_STATE_HOME = stateHome;
		rmSync(log, { force: true });
		notifications.length = 0;
		const { lifecycle } = loadExt();
		await lifecycle["session_start"]!();
		await lifecycle["session_shutdown"]!();
		if (refused) {
			expect(existsSync(log)).toBe(false);
			expect(await reportedNotifications(1)).toHaveLength(1);
			expect(notifications[0]?.message).toBe(`wezterm-attention: ${refused} is not UTF-8`);
		} else {
			expect(readFileSync(log, "utf8").trim().split("\n")).toEqual([
				"hooks event pi session_start",
				"hooks event pi session_shutdown",
			]);
			expect(notifications).toEqual([]);
		}
	}
	expect(readdirSync(scratch)).toEqual([]);
});

// The states README.md lists for other extensions to emit. It is a public
// cross-extension contract, so every one is locked here rather than left to the
// lifecycle path, which emits none of them over the bus.
const BUS_STATES = ["thinking", "stop", "notify", "review"];

test("event: every documented state reaches the writer as itself", async () => {
	for (const state of BUS_STATES) {
		const log = fakeWriter("wez-state-");
		const h = loadExt();
		await h.lifecycle["session_start"]!();
		await h.emit(state);
		expect(loggedCalls(log).at(-1)?.payload.state).toBe(state);
	}
});

test("event: an unrecognized state is rejected, writing nothing", async () => {
	// normalizeState's `default: undefined` is the gate. Without it an arbitrary
	// string reaches the writer as a bus state it would refuse.
	const log = fakeWriter("wez-bogus-");
	const h = loadExt();
	await h.lifecycle["session_start"]!();
	await h.emit("bogus");
	await h.lifecycle["session_shutdown"]!();
	expect(loggedCalls(log).map((call) => call.command)).toEqual([
		"hooks event pi session_start",
		"hooks event pi session_shutdown",
	]);
});

test("drain override: a delay the timer cannot hold falls back instead of wrapping to 1ms", () => {
	// `setTimeout` keeps its delay in a signed 32-bit int, so 2147483648 fires
	// almost immediately rather than in 24 days — measured at 2ms in Node and 3ms
	// in Bun. Someone setting a huge value wants a longer drain, so accepting it
	// would deliver the shortest one possible with no error. These assert the
	// parser's boundary; none of them starts a timer.
	const set = (value: string | undefined) => {
		if (value === undefined) delete process.env.PI_WEZTERM_ATTENTION_DRAIN_TIMEOUT_MS;
		else process.env.PI_WEZTERM_ATTENTION_DRAIN_TIMEOUT_MS = value;
		return drainTimeoutMs();
	};
	expect(set("2147483647")).toBe(2147483647); // the largest the timer holds
	expect(set("2147483648")).toBe(2000); // one past it: falls back, not 1ms
	expect(set("999999999999999999999")).toBe(2000);
	expect(set("0")).toBe(2000);
	expect(set("30s")).toBe(2000);
	expect(set(undefined)).toBe(2000);
	expect(set("30000")).toBe(30000); // an ordinary override still works
});
