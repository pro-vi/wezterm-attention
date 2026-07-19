// Regression tests for pi/index.ts, driving the REAL registered lifecycle and
// event handlers through a mock ExtensionAPI. Named after the properties they
// lock — several are the negation of a bug fixed during review triage.
// Mutations are awaited deterministically (the event handler returns its queue
// promise), never slept on. Run: `bun test`.
import { test, expect } from "bun:test";
import { mkdtempSync, mkdirSync, existsSync, readFileSync, readdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ext from "../pi/index.ts";

function loadExt() {
	const lifecycle: Record<string, () => Promise<void> | void> = {};
	let eventHandler: (data: unknown) => void | Promise<void> = () => {};
	const pi = {
		events: { on: (_e: string, h: (d: unknown) => void | Promise<void>) => { eventHandler = h; } },
		on: (event: string, h: () => Promise<void> | void) => { lifecycle[event] = h; },
		registerCommand: () => {},
	};
	// deno-lint-ignore no-explicit-any
	ext(pi as any);
	return { lifecycle, eventHandler };
}

function freshDir(prefix: string): string {
	const d = mkdtempSync(join(tmpdir(), prefix));
	process.env.WEZTERM_ATTENTION_DIR = d;
	return d;
}

test("lifecycle: agent_start writes a thinking marker with ttl_ms and source pi", async () => {
	const dir = freshDir("wez-life1-");
	process.env.WEZTERM_PANE = "42";
	const { lifecycle } = loadExt();
	await lifecycle["agent_start"]!();
	const m = JSON.parse(readFileSync(join(dir, "42"), "utf8"));
	expect(m.type).toBe("thinking");
	expect(m.source).toBe("pi");
	expect(typeof m.ttl_ms).toBe("number");
	rmSync(dir, { recursive: true, force: true });
});

test("lifecycle: agent_end writes a stop marker (no ttl_ms)", async () => {
	const dir = freshDir("wez-life2-");
	process.env.WEZTERM_PANE = "42";
	const { lifecycle } = loadExt();
	await lifecycle["agent_end"]!();
	const m = JSON.parse(readFileSync(join(dir, "42"), "utf8"));
	expect(m.type).toBe("stop");
	expect(m.ttl_ms).toBeUndefined();
	rmSync(dir, { recursive: true, force: true });
});

test("event: emitting a notify object writes a labeled notify marker", async () => {
	const dir = freshDir("wez-evt-");
	process.env.WEZTERM_PANE = "42";
	const { eventHandler } = loadExt();
	await eventHandler({ type: "notify", label: "answer me" });
	const m = JSON.parse(readFileSync(join(dir, "42"), "utf8"));
	expect(m.type).toBe("notify");
	expect(m.label).toBe("answer me");
	rmSync(dir, { recursive: true, force: true });
});

test("event: a bare string state is accepted", async () => {
	const dir = freshDir("wez-evtstr-");
	process.env.WEZTERM_PANE = "42";
	const { eventHandler } = loadExt();
	await eventHandler("review");
	expect(JSON.parse(readFileSync(join(dir, "42"), "utf8")).type).toBe("review");
	rmSync(dir, { recursive: true, force: true });
});

test("ordering: notify then clear leaves NO marker (last requested wins)", async () => {
	const dir = freshDir("wez-order1-");
	process.env.WEZTERM_PANE = "42";
	const { eventHandler } = loadExt();
	let present = 0;
	for (let i = 0; i < 30; i++) {
		// Emit both before either settles (exercise the interleaving), then await
		// both deterministically — no sleep, so the assertion can't run early.
		const a = eventHandler("notify");
		const b = eventHandler("clear");
		await Promise.all([a, b]);
		if (existsSync(join(dir, "42"))) present++;
	}
	expect(present).toBe(0);
	rmSync(dir, { recursive: true, force: true });
});

test("ordering: clear then notify leaves a notify marker", async () => {
	const dir = freshDir("wez-order2-");
	process.env.WEZTERM_PANE = "42";
	const { eventHandler } = loadExt();
	writeFileSync(join(dir, "42"), "{}");
	const a = eventHandler("clear");
	const b = eventHandler("notify");
	await Promise.all([a, b]);
	expect(existsSync(join(dir, "42"))).toBe(true);
	expect(JSON.parse(readFileSync(join(dir, "42"), "utf8")).type).toBe("notify");
	rmSync(dir, { recursive: true, force: true });
});

test("traversal: a bad pane id writes NOTHING outside the marker dir", async () => {
	const parent = mkdtempSync(join(tmpdir(), "wez-trav1-"));
	const dir = join(parent, "markerdir");
	mkdirSync(dir);
	const victim = join(parent, "victim.txt");
	writeFileSync(victim, "IMPORTANT");
	process.env.WEZTERM_ATTENTION_DIR = dir;
	process.env.WEZTERM_PANE = "../victim.txt";
	const { eventHandler } = loadExt();
	await eventHandler("notify");
	expect(readFileSync(victim, "utf8")).toBe("IMPORTANT"); // untouched
	rmSync(parent, { recursive: true, force: true });
});

test("traversal: a bad pane id does NOT delete an outside file on clear", async () => {
	const parent = mkdtempSync(join(tmpdir(), "wez-trav2-"));
	const dir = join(parent, "markerdir");
	mkdirSync(dir);
	const victim = join(parent, "victim.txt");
	writeFileSync(victim, "IMPORTANT");
	process.env.WEZTERM_ATTENTION_DIR = dir;
	process.env.WEZTERM_PANE = "../victim.txt";
	const { eventHandler } = loadExt();
	await eventHandler("clear");
	expect(existsSync(victim)).toBe(true); // not deleted
	rmSync(parent, { recursive: true, force: true });
});

test("cleanup: a failed rename does not leak temp files", async () => {
	const dir = freshDir("wez-leak-");
	process.env.WEZTERM_PANE = "42";
	mkdirSync(join(dir, "42")); // marker path is a directory → rename fails
	const { eventHandler } = loadExt();
	for (let i = 0; i < 10; i++) await eventHandler("notify");
	const leaked = readdirSync(dir).filter((f) => f.includes(".tmp."));
	expect(leaked.length).toBe(0);
	rmSync(dir, { recursive: true, force: true });
});

test("missing pane: lifecycle write is a silent no-op, no throw", async () => {
	const dir = freshDir("wez-nopane-");
	delete process.env.WEZTERM_PANE;
	const { lifecycle } = loadExt();
	await lifecycle["agent_start"]!();
	expect(existsSync(join(dir, "undefined"))).toBe(false);
	rmSync(dir, { recursive: true, force: true });
});
